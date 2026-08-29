//! History of BYOK analysis runs.
//!
//! Kept so Settings can show what the last run cost and so a cancelled run is
//! distinguishable from one that never happened.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::time::{format_ts, parse_ts};
use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq)]
pub struct AiRun {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub started_at: Option<DateTime<Utc>>,
    /// `None` while the run is in flight or if it was cancelled.
    pub finished_at: Option<DateTime<Utc>>,
    pub repos_count: i64,
    /// Estimated USD cost. Always `0.0` for local providers.
    pub cost_estimate: f64,
}

impl Store {
    /// Open a run row and return its id.
    pub async fn begin_ai_run(&self, provider: &str, model: &str, repos: i64) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO ai_runs (provider, model, started_at, repos_count) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(provider)
        .bind(model)
        .bind(format_ts(&Utc::now()))
        .bind(repos)
        .fetch_one(self.pool())
        .await?;
        Ok(row.try_get(0)?)
    }

    /// Close a run. A cancelled run is simply never finished.
    pub async fn finish_ai_run(&self, id: i64, repos: i64, cost: f64) -> Result<()> {
        sqlx::query(
            "UPDATE ai_runs SET finished_at = ?, repos_count = ?, cost_estimate = ? WHERE id = ?",
        )
        .bind(format_ts(&Utc::now()))
        .bind(repos)
        .bind(cost)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn recent_ai_runs(&self, limit: i64) -> Result<Vec<AiRun>> {
        let rows = sqlx::query(
            "SELECT id, provider, model, started_at, finished_at, repos_count, cost_estimate \
             FROM ai_runs ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AiRun {
                    id: row.try_get(0)?,
                    provider: row.try_get(1)?,
                    model: row.try_get(2)?,
                    started_at: parse_ts(row.try_get::<Option<String>, _>(3)?.as_deref()),
                    finished_at: parse_ts(row.try_get::<Option<String>, _>(4)?.as_deref()),
                    repos_count: row.try_get(5)?,
                    cost_estimate: row.try_get(6)?,
                })
            })
            .collect()
    }
}
