//! SQLite persistence for Starlet.
//!
//! One [`Store`] owns one connection pool against one database file. The file
//! lives in the OS data directory and is opened in WAL mode so the sync worker
//! can write while the UI reads.
//!
//! Every method here is `async` and runs on the caller's Tokio runtime. The UI
//! never calls these directly from `render`; it goes through the background
//! executor and applies results on the application thread.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};

mod ai_runs;
mod fts;
mod repos;
mod state;
mod taxonomy;
mod time;

pub use ai_runs::AiRun;
pub use fts::FtsHit;
pub use state::{
    KEY_AI_ENDPOINT, KEY_AI_MODEL, KEY_AI_PROVIDER, KEY_COLUMN_WIDTHS, KEY_INITIAL_SYNC_DONE,
    KEY_LAST_SYNC, KEY_STAR_WATERMARK,
};
pub use taxonomy::{GroupFacet, TagFacet};
pub use time::{format_ts, parse_ts};

/// Anything the store can fail with.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("malformed json in column {column}: {source}")]
    Json {
        column: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("no data directory available on this platform")]
    NoDataDir,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// The application's SQLite database.
#[derive(Clone, Debug)]
pub struct Store {
    pool: Pool<Sqlite>,
}

impl Store {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // NORMAL is the documented safe pairing with WAL: a crash can lose
            // the tail of the last transaction, which for a rebuildable mirror
            // of GitHub is not worth an fsync per commit.
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));

        Self::from_options(options).await
    }

    /// An ephemeral database. Used by tests and by `--no-store` smoke runs.
    pub async fn open_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .shared_cache(true)
            .foreign_keys(true);
        // A shared-cache in-memory database disappears when the last connection
        // closes, so the pool must hold exactly one.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        let store = Store { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn from_options(options: SqliteConnectOptions) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let store = Store { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    /// Close the pool. WAL checkpoints on the last connection drop.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// The default database path: `<data dir>/starlet/starlet.db`.
///
/// * Linux: `~/.local/share/starlet/starlet.db`
/// * macOS: `~/Library/Application Support/starlet/starlet.db`
/// * Windows: `%APPDATA%\starlet\data\starlet.db`
pub fn default_database_path() -> Result<PathBuf> {
    let dirs =
        directories::ProjectDirs::from("dev", "starlet", "starlet").ok_or(StoreError::NoDataDir)?;
    Ok(dirs.data_dir().join("starlet.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_fts5_is_available() {
        let store = Store::open_in_memory().await.expect("open");
        // FTS5 is not universally compiled into libsqlite3. If the bundled
        // build ever loses it, this fails loudly here rather than silently
        // degrading search at runtime.
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM repos_fts")
            .fetch_one(store.pool())
            .await
            .expect("repos_fts must exist");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn wal_mode_is_active_on_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("starlet.db")).await.unwrap();
        let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
