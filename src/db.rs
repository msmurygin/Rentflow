//! Подключение к SQLite и применение миграций.

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

pub async fn connect(url: &str) -> anyhow::Result<SqlitePool> {
    let in_memory = url.contains(":memory:");
    let opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(if in_memory {
            SqliteJournalMode::Memory
        } else {
            SqliteJournalMode::Wal
        })
        .busy_timeout(std::time::Duration::from_secs(10));
    // :memory: живёт в одном соединении — иначе каждое соединение увидит свою пустую базу.
    let pool = SqlitePoolOptions::new()
        .max_connections(if in_memory { 1 } else { 5 })
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
