//! `rentflow set-baselines` на файловой базе: без --yes ничего не пишет, с --yes ставит значения и пишет аудит.
mod common;
use common::*;
use std::process::Command;

fn run(db: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rentflow"))
        .env("DATABASE_URL", format!("sqlite://{db}"))
        .env_remove("TELEGRAM_BOT_TOKEN")
        .arg("set-baselines")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_sets_baselines_only_with_yes_and_audits() {
    let dir = std::env::temp_dir().join(format!("rf-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("rentflow.db").to_string_lossy().to_string();
    let pool = rentflow::db::connect(&format!("sqlite://{db}"))
        .await
        .unwrap();
    let app = std::sync::Arc::new(rentflow::app::App::new(
        pool.clone(),
        Default::default(),
        std::sync::Arc::new(rentflow::ocr::NoExtractor),
    ));
    app.register_owner(&sender(OWNER_A), now()).await.unwrap();
    let uid: i64 = sqlx::query_scalar("SELECT id FROM app_user WHERE telegram_id = ?")
        .bind(OWNER_A)
        .fetch_one(&pool)
        .await
        .unwrap();
    let pid = app
        .create_property(
            uid,
            &rentflow::setup::NewProperty {
                name: "Кв".into(),
                tenant_name: "Ж".into(),
                baselines: ["cold", "hot", "t1", "t2"]
                    .iter()
                    .map(|c| (c.to_string(), "0".into()))
                    .collect(),
                prices: ["cold", "hot", "t1", "t2"]
                    .iter()
                    .map(|c| (c.to_string(), "1".into()))
                    .collect(),
                sewer_price: Some("1".into()),
                ..Default::default()
            },
            now(),
        )
        .await
        .unwrap();
    let id = pid.to_string();
    let vals = ["cold=761", "hot=420", "t1=148", "t2=60"];
    let current = |pool: sqlx::SqlitePool| async move {
        sqlx::query_as::<_, (String, String)>(
            "SELECT g.code, r.value FROM register g JOIN reading r ON r.register_id=g.id AND r.kind='baseline' AND r.status='confirmed' ORDER BY g.id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };

    let mut a = vec![id.as_str()];
    a.extend(vals);
    let (ok, out) = run(&db, &a);
    assert!(
        ok && out.contains("cold: 0 -> 761") && out.contains("Ничего не записано"),
        "{out}"
    );
    assert!(current(pool.clone()).await.iter().all(|(_, v)| v == "0"));

    a.push("--yes");
    let (ok, out) = run(&db, &a);
    assert!(ok && out.contains("Готово"), "{out}");
    assert_eq!(
        current(pool.clone()).await,
        vec![
            ("cold".into(), "761".into()),
            ("hot".into(), "420".into()),
            ("t1".into(), "148".into()),
            ("t2".into(), "60".into())
        ]
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='baselines_update'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 1);
    let _ = std::fs::remove_dir_all(dir);
}
