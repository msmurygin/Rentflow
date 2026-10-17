//! Резервные копии и проверка восстановления.
//!
//! База копируется командой SQLite `VACUUM INTO`: это согласованный снимок работающей базы с учётом WAL
//! (обычный `cp` файла под нагрузкой может дать битую копию). Рядом лежат фото и `manifest.json` с
//! контрольными суммами, версией схемы и числом строк в таблицах. Шифрование и выгрузка за пределы сервера —
//! в `deploy/backup-offsite.sh`; здесь — всё, что можно проверить детерминированно.

use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

/// Таблицы, число строк которых сверяется при проверке копии.
const COUNTED: &[&str] = &[
    "app_user",
    "property",
    "tenancy",
    "register",
    "reading",
    "tariff",
    "bill",
    "invite",
    "pending_reading",
    "audit_log",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub created_at: String,
    pub schema_version: i64,
    pub files: Vec<FileEntry>,
    pub rows: BTreeMap<String, i64>,
}

/// Результат проверки копии: пустой список `problems` — копия пригодна к восстановлению.
#[derive(Debug, Default)]
pub struct Report {
    pub problems: Vec<String>,
    pub rows: BTreeMap<String, i64>,
    pub schema_version: i64,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let bytes =
        std::fs::read(path).with_context(|| format!("не удалось прочитать {}", path.display()))?;
    Ok((hex::encode(Sha256::digest(&bytes)), bytes.len() as u64))
}

async fn open_ro(db: &Path) -> Result<SqlitePool> {
    let opts =
        SqliteConnectOptions::from_str(&format!("sqlite://{}", db.display()))?.read_only(true);
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?)
}

async fn count_rows(pool: &SqlitePool) -> Result<BTreeMap<String, i64>> {
    let mut rows = BTreeMap::new();
    for t in COUNTED {
        // таблицы нет в очень старой схеме — считаем нулём, а не падаем
        let n: Option<i64> = sqlx::query_scalar(&format!("SELECT count(*) FROM {t}"))
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
        rows.insert((*t).to_string(), n.unwrap_or(0));
    }
    Ok(rows)
}

/// Создаёт копию в `out_root/rentflow-ГГГГММДД-ЧЧММСС/` и возвращает путь к ней.
pub async fn create_backup(
    pool: &SqlitePool,
    photo_dir: Option<&Path>,
    out_root: &Path,
    now: NaiveDateTime,
) -> Result<PathBuf> {
    let dir = out_root.join(format!("rentflow-{}", now.format("%Y%m%d-%H%M%S")));
    if dir.exists() {
        bail!("{} уже существует", dir.display());
    }
    std::fs::create_dir_all(&dir)?;
    let db_copy = dir.join("rentflow.db");
    // путь в SQL-литерале: кавычки экранируются удвоением
    let escaped = db_copy.to_string_lossy().replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{escaped}'"))
        .execute(pool)
        .await
        .context("VACUUM INTO не удался")?;

    let mut files = vec![];
    let (sha, bytes) = sha256_file(&db_copy)?;
    files.push(FileEntry {
        path: "rentflow.db".into(),
        sha256: sha,
        bytes,
    });
    if let Some(src) = photo_dir.filter(|p| p.is_dir()) {
        let dst = dir.join("photos");
        std::fs::create_dir_all(&dst)?;
        let mut names: Vec<_> = std::fs::read_dir(src)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name();
            std::fs::copy(e.path(), dst.join(&name))?;
            let (sha, bytes) = sha256_file(&dst.join(&name))?;
            files.push(FileEntry {
                path: format!("photos/{}", name.to_string_lossy()),
                sha256: sha,
                bytes,
            });
        }
    }
    let copy_pool = open_ro(&db_copy).await?;
    let schema_version: i64 =
        sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM _sqlx_migrations")
            .fetch_one(&copy_pool)
            .await
            .unwrap_or(0);
    let rows = count_rows(&copy_pool).await?;
    copy_pool.close().await;
    let manifest = Manifest {
        created_at: now.format("%Y-%m-%dT%H:%M:%S").to_string(),
        schema_version,
        files,
        rows,
    };
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // статус для мониторинга: когда была последняя успешная копия
    std::fs::write(
        out_root.join("last_backup.json"),
        serde_json::json!({ "at": manifest.created_at, "dir": dir.file_name().map(|n| n.to_string_lossy().to_string()) }).to_string(),
    )?;
    Ok(dir)
}

/// Проверяет копию: контрольные суммы, целостность SQLite, внешние ключи, число строк, наличие фото.
pub async fn verify_backup(dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).context("нет manifest.json")?,
    )?;
    for f in &manifest.files {
        match sha256_file(&dir.join(&f.path)) {
            Ok((sha, bytes)) if sha == f.sha256 && bytes == f.bytes => {}
            Ok(_) => report
                .problems
                .push(format!("{}: контрольная сумма не совпала", f.path)),
            Err(_) => report
                .problems
                .push(format!("{}: файл отсутствует", f.path)),
        }
    }
    let db = dir.join("rentflow.db");
    if !db.is_file() {
        report.problems.push("rentflow.db отсутствует".into());
        return Ok(report);
    }
    let pool = open_ro(&db).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        report
            .problems
            .push(format!("integrity_check: {integrity}"));
    }
    let fk: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    if !fk.is_empty() {
        report
            .problems
            .push(format!("нарушены внешние ключи: {} строк", fk.len()));
    }
    report.schema_version =
        sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
    if report.schema_version != manifest.schema_version {
        report
            .problems
            .push("версия схемы не совпала с манифестом".into());
    }
    report.rows = count_rows(&pool).await?;
    if report.rows != manifest.rows {
        report
            .problems
            .push("число строк в таблицах не совпало с манифестом".into());
    }
    // каждое фото, на которое ссылается подтверждённое показание, должно лежать в копии
    let refs: Vec<(String,)> = sqlx::query_as(
        "SELECT photo_path FROM reading WHERE photo_path IS NOT NULL AND status = 'confirmed'",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    for (path,) in refs {
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !dir.join("photos").join(&name).is_file() {
            report
                .problems
                .push(format!("фото {name} не найдено в копии"));
        }
    }
    pool.close().await;
    Ok(report)
}

/// Восстанавливает копию в пустой каталог данных (staging или новый сервер). Непустой каталог не трогаем.
pub async fn restore_backup(dir: &Path, target: &Path) -> Result<Report> {
    let report = verify_backup(dir).await?;
    if !report.ok() {
        bail!("копия не прошла проверку: {}", report.problems.join("; "));
    }
    if target.exists() && std::fs::read_dir(target)?.next().is_some() {
        bail!(
            "{} не пуст — восстановление отменено, ничего не перезаписано",
            target.display()
        );
    }
    std::fs::create_dir_all(target.join("photos"))?;
    std::fs::copy(dir.join("rentflow.db"), target.join("rentflow.db"))?;
    let photos = dir.join("photos");
    if photos.is_dir() {
        for e in std::fs::read_dir(&photos)? {
            let e = e?;
            std::fs::copy(e.path(), target.join("photos").join(e.file_name()))?;
        }
    }
    Ok(report)
}

/// Отметка «копия выгружена за пределы сервера» — её ставит `deploy/backup-offsite.sh` после успешной выгрузки.
pub fn mark_offsite(out_root: &Path, now: NaiveDateTime) -> Result<()> {
    std::fs::create_dir_all(out_root)?;
    std::fs::write(
        out_root.join("last_offsite.json"),
        serde_json::json!({ "at": now.format("%Y-%m-%dT%H:%M:%S").to_string() }).to_string(),
    )?;
    Ok(())
}

/// Возраст отметки в часах (`None` — файла нет или он не читается).
pub fn mark_age_hours(file: &Path, now: NaiveDateTime) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    let at = NaiveDateTime::parse_from_str(v["at"].as_str()?, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some((now - at).num_hours())
}

/// Открывает базу (применяя недостающие миграции) и проверяет её: так проверяется, что НОВЫЙ выпуск
/// принимает данные production. Запускать только на восстановленной копии, не на рабочей базе.
pub async fn migrate_check(database_url: &str) -> Result<Report> {
    let pool = crate::db::connect(database_url)
        .await
        .context("миграции не применились")?;
    let mut report = Report::default();
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        report
            .problems
            .push(format!("integrity_check: {integrity}"));
    }
    let fk: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    if !fk.is_empty() {
        report
            .problems
            .push(format!("нарушены внешние ключи: {} строк", fk.len()));
    }
    report.schema_version =
        sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await?;
    report.rows = count_rows(&pool).await?;
    pool.close().await;
    Ok(report)
}
