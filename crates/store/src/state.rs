//! The `sync_state` key/value table.
//!
//! Two namespaces share it: `sync.*` for the sync engine's cursors and
//! watermarks, `ui.*` for interface state that must survive a restart.

use sqlx::Row;

use crate::{Result, Store};

/// The timestamp of the newest star seen by a completed sync.
pub const KEY_STAR_WATERMARK: &str = "sync.star_watermark";
/// RFC 3339 timestamp of the last successful full or incremental run.
pub const KEY_LAST_SYNC: &str = "sync.last_sync";
/// `"1"` once the first full page-through has completed.
pub const KEY_INITIAL_SYNC_DONE: &str = "sync.initial_done";
/// Table column widths, as a JSON array of floats.
pub const KEY_COLUMN_WIDTHS: &str = "ui.column_widths";
/// The selected AI provider id.
pub const KEY_AI_PROVIDER: &str = "ui.ai_provider";
/// The selected AI model for the current provider.
pub const KEY_AI_MODEL: &str = "ui.ai_model";
/// Base URL override, used by the Ollama provider.
pub const KEY_AI_ENDPOINT: &str = "ui.ai_endpoint";

impl Store {
    pub async fn get_state(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM sync_state WHERE key = ?")
            .bind(key)
            .fetch_optional(self.pool())
            .await?;
        match row {
            Some(row) => Ok(Some(row.try_get(0)?)),
            None => Ok(None),
        }
    }

    pub async fn set_state(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (key, value) VALUES (?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn clear_state(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM sync_state WHERE key = ?")
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
