//! The sync engine.
//!
//! Two modes share one implementation:
//!
//! * **Full** pages the entire star list, which is the only way to learn about
//!   unstars, and is what the first run does.
//! * **Incremental** reads pages until it passes the watermark, refreshes
//!   metadata that has aged past 24 h, and only escalates to a full listing
//!   when the account's star count disagrees with the local row count.
//!
//! Progress is reported through an unbounded channel rather than a callback so
//! the UI can own its own back-pressure and the engine never blocks on a
//! renderer.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Duration, Utc};
use starlet_core::model::Repo;
use starlet_store::{
    KEY_INITIAL_SYNC_DONE, KEY_LAST_SYNC, KEY_STAR_WATERMARK, Store, format_ts, parse_ts,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::client::{Conditional, GRAPHQL_BATCH, GitHub};
use crate::{Result, SyncError};

/// Stars requested per page. 100 is GitHub's maximum.
const PER_PAGE: u32 = 100;
/// Metadata refreshes attempted in one incremental run. Bounds the worst case
/// at roughly a quarter of the hourly REST budget.
const REFRESH_BUDGET: usize = 250;
/// Language backfills attempted per run, in repositories.
const LANGUAGE_BUDGET: usize = 500;
/// Metadata older than this is refreshed.
const STALE_AFTER_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Stars,
    Languages,
    Metadata,
}

impl SyncPhase {
    pub fn label(self) -> &'static str {
        match self {
            SyncPhase::Stars => "Fetching stars",
            SyncPhase::Languages => "Fetching languages",
            SyncPhase::Metadata => "Refreshing metadata",
        }
    }
}

/// What the sync engine tells the UI.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    Started(SyncMode),
    Progress {
        phase: SyncPhase,
        done: usize,
        /// `None` until the total is known — the star count arrives with the
        /// first page, not before it.
        total: Option<usize>,
    },
    /// Rows that changed. The search index reindexes exactly these.
    Upserted(Vec<Repo>),
    /// Repositories the user unstarred.
    Removed(Vec<i64>),
    Finished(SyncSummary),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncSummary {
    pub seen: usize,
    pub written: usize,
    pub removed: usize,
    pub languages_filled: usize,
    pub metadata_refreshed: usize,
}

/// Owns one GitHub client and one store for the duration of a sync.
pub struct SyncEngine {
    github: GitHub,
    store: Store,
    cancel: Arc<AtomicBool>,
}

impl SyncEngine {
    pub fn new(github: GitHub, store: Store) -> Self {
        Self {
            github,
            store,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A flag the caller can flip to stop the run at the next checkpoint.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Whether the first full page-through has ever completed.
    pub async fn needs_full_sync(store: &Store) -> bool {
        store
            .get_state(KEY_INITIAL_SYNC_DONE)
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some("1")
    }

    /// Run one sync. Emits events as it goes and returns the summary.
    pub async fn run(
        &self,
        mode: SyncMode,
        events: &UnboundedSender<SyncEvent>,
    ) -> Result<SyncSummary> {
        let _ = events.send(SyncEvent::Started(mode));
        let result = match mode {
            SyncMode::Full => self.run_full(events).await,
            SyncMode::Incremental => self.run_incremental(events).await,
        };

        match &result {
            Ok(summary) => {
                let _ = self
                    .store
                    .set_state(KEY_LAST_SYNC, &format_ts(&Utc::now()))
                    .await;
                let _ = events.send(SyncEvent::Finished(*summary));
            }
            Err(err) => {
                let _ = events.send(SyncEvent::Failed(err.to_string()));
            }
        }
        result
    }

    async fn run_full(&self, events: &UnboundedSender<SyncEvent>) -> Result<SyncSummary> {
        let mut summary = SyncSummary::default();
        let known_before = self.store.known_ids().await?;
        let total = self.github.starred_total().await.ok().map(|n| n as usize);

        let mut seen: HashSet<i64> = HashSet::with_capacity(known_before.len());
        let mut newest_star: Option<DateTime<Utc>> = None;
        let mut page = 1u32;

        loop {
            if self.cancelled() {
                return Err(SyncError::Cancelled);
            }
            let fetched = self.github.starred_page(page, PER_PAGE).await?;
            if fetched.items.is_empty() {
                break;
            }
            let batch = self
                .absorb_page(&fetched.items, &mut seen, &mut newest_star)
                .await?;
            summary.seen += batch.len();
            summary.written += batch.len();
            let _ = events.send(SyncEvent::Upserted(batch));
            let _ = events.send(SyncEvent::Progress {
                phase: SyncPhase::Stars,
                done: summary.seen,
                total,
            });

            if !fetched.has_next {
                break;
            }
            page += 1;
        }

        // A complete listing is the only trustworthy basis for an unstar diff.
        let removed: Vec<i64> = known_before.difference(&seen).copied().collect();
        if !removed.is_empty() {
            self.store.delete_repos(&removed).await?;
            summary.removed = removed.len();
            let _ = events.send(SyncEvent::Removed(removed));
            self.store.prune_orphan_tags().await?;
        }

        if let Some(newest) = newest_star {
            self.store
                .set_state(KEY_STAR_WATERMARK, &format_ts(&newest))
                .await?;
        }
        self.store.set_state(KEY_INITIAL_SYNC_DONE, "1").await?;

        summary.languages_filled = self.backfill_languages(events).await?;
        Ok(summary)
    }

    async fn run_incremental(&self, events: &UnboundedSender<SyncEvent>) -> Result<SyncSummary> {
        let watermark = parse_ts(self.store.get_state(KEY_STAR_WATERMARK).await?.as_deref());

        // No watermark means nothing has ever completed; there is nothing
        // incremental to do.
        let Some(watermark) = watermark else {
            return self.run_full(events).await;
        };

        let mut summary = SyncSummary::default();
        let mut seen = HashSet::new();
        let mut newest_star = None;
        let mut page = 1u32;

        'pages: loop {
            if self.cancelled() {
                return Err(SyncError::Cancelled);
            }
            let fetched = self.github.starred_page(page, PER_PAGE).await?;
            if fetched.items.is_empty() {
                break;
            }
            // Stop at the first star we already knew about. The listing is
            // sorted newest-first, so everything after it is older too.
            let fresh: Vec<_> = fetched
                .items
                .iter()
                .take_while(|item| {
                    parse_ts(item.starred_at.as_deref()).is_none_or(|at| at > watermark)
                })
                .cloned()
                .collect();
            let exhausted = fresh.len() < fetched.items.len();

            if !fresh.is_empty() {
                let batch = self
                    .absorb_page(&fresh, &mut seen, &mut newest_star)
                    .await?;
                summary.seen += batch.len();
                summary.written += batch.len();
                let _ = events.send(SyncEvent::Upserted(batch));
                let _ = events.send(SyncEvent::Progress {
                    phase: SyncPhase::Stars,
                    done: summary.seen,
                    total: None,
                });
            }
            if exhausted || !fetched.has_next {
                break 'pages;
            }
            page += 1;
        }

        if let Some(newest) = newest_star {
            self.store
                .set_state(KEY_STAR_WATERMARK, &format_ts(&newest))
                .await?;
        }

        summary.metadata_refreshed = self.refresh_stale(events).await?;
        summary.languages_filled = self.backfill_languages(events).await?;

        // Unstars leave no trace in a newest-first listing, so compare counts:
        // one GraphQL point, and a full pass only when they disagree.
        if let Ok(remote_total) = self.github.starred_total().await {
            let local_total = self.store.repo_count().await?;
            if remote_total != local_total {
                tracing::info!(
                    "star count drift ({remote_total} remote vs {local_total} local), reconciling"
                );
                let full = self.run_full(events).await?;
                summary.removed = full.removed;
                summary.seen = summary.seen.max(full.seen);
            }
        }

        Ok(summary)
    }

    /// Convert, persist, and remember one page of starred items.
    async fn absorb_page(
        &self,
        items: &[crate::wire::StarredItem],
        seen: &mut HashSet<i64>,
        newest_star: &mut Option<DateTime<Utc>>,
    ) -> Result<Vec<Repo>> {
        let now = Utc::now();
        let repos: Vec<Repo> = items
            .iter()
            .map(|item| {
                let repo = item.repo.clone().into_repo(item.starred_at.as_deref(), now);
                if let Some(at) = repo.starred_at {
                    *newest_star = Some(newest_star.map_or(at, |cur| cur.max(at)));
                }
                repo
            })
            .collect();
        for repo in &repos {
            seen.insert(repo.id);
        }
        self.store.upsert_repos(&repos).await?;
        Ok(repos)
    }

    /// Conditional refresh of repositories whose metadata has aged out.
    ///
    /// `304 Not Modified` is the common case and costs no rate-limit budget,
    /// so the bound here is wall-clock rather than quota.
    async fn refresh_stale(&self, events: &UnboundedSender<SyncEvent>) -> Result<usize> {
        let cutoff = Utc::now() - Duration::hours(STALE_AFTER_HOURS);
        let stale = self.store.stale_ids(cutoff).await?;
        let stale: Vec<_> = stale.into_iter().take(REFRESH_BUDGET).collect();
        if stale.is_empty() {
            return Ok(0);
        }

        let mut refreshed = 0usize;
        let mut changed = Vec::new();
        for (done, (id, full_name, etag)) in stale.iter().enumerate() {
            if self.cancelled() {
                break;
            }
            match self
                .github
                .repo_if_modified(full_name, etag.as_deref())
                .await
            {
                Ok(Conditional::NotModified) => {
                    self.store.touch_synced_at(*id, Utc::now()).await?;
                }
                Ok(Conditional::Modified { value, etag }) => {
                    let existing_star = self.store.load_repo(*id).await?.and_then(|r| r.starred_at);
                    let mut repo = value.into_repo(None, Utc::now());
                    repo.starred_at = existing_star;
                    self.store.upsert_repos(std::slice::from_ref(&repo)).await?;
                    self.store.set_etag(*id, etag.as_deref()).await?;
                    changed.push(repo);
                    refreshed += 1;
                }
                // A 404 here means the repo was deleted or made private. Leave
                // the row alone: the next full sync removes it if it is really
                // gone, and guessing would delete data on a transient error.
                Err(SyncError::NotFound) => {
                    self.store.touch_synced_at(*id, Utc::now()).await?;
                }
                Err(err @ SyncError::RateLimited { .. }) => return Err(err),
                Err(err) => tracing::warn!("refresh {full_name} failed: {err}"),
            }

            if done % 25 == 0 {
                let _ = events.send(SyncEvent::Progress {
                    phase: SyncPhase::Metadata,
                    done,
                    total: Some(stale.len()),
                });
            }
        }

        if !changed.is_empty() {
            let _ = events.send(SyncEvent::Upserted(changed));
        }
        Ok(refreshed)
    }

    /// Fill in language byte counts for repos that have none, in GraphQL
    /// batches so the whole backfill costs one request per 25 repositories.
    async fn backfill_languages(&self, events: &UnboundedSender<SyncEvent>) -> Result<usize> {
        let pending = self
            .store
            .ids_without_languages(LANGUAGE_BUDGET as i64)
            .await?;
        if pending.is_empty() {
            return Ok(0);
        }

        let mut filled = 0usize;
        for (chunk_ix, chunk) in pending.chunks(GRAPHQL_BATCH).enumerate() {
            if self.cancelled() {
                break;
            }
            let names: Vec<String> = chunk.iter().map(|(_, name)| name.clone()).collect();
            match self.github.languages_batch(&names).await {
                Ok(results) => {
                    let mut changed = Vec::with_capacity(results.len());
                    for (id, languages) in results {
                        if languages.is_empty() {
                            continue;
                        }
                        self.store.set_languages(id, &languages).await?;
                        filled += 1;
                        if let Some(repo) = self.store.load_repo(id).await? {
                            changed.push(repo);
                        }
                    }
                    if !changed.is_empty() {
                        let _ = events.send(SyncEvent::Upserted(changed));
                    }
                }
                Err(err @ SyncError::RateLimited { .. }) => return Err(err),
                Err(err) => tracing::warn!("language batch failed: {err}"),
            }
            let _ = events.send(SyncEvent::Progress {
                phase: SyncPhase::Languages,
                done: (chunk_ix + 1) * GRAPHQL_BATCH,
                total: Some(pending.len()),
            });
        }
        Ok(filled)
    }

    /// Fetch and cache the contributor list for one repository.
    ///
    /// Called when a detail sheet opens, never during a sync: contributors are
    /// REST-only and would cost one request per repository.
    pub async fn fetch_contributors(
        &self,
        id: i64,
        full_name: &str,
    ) -> Result<Vec<starlet_core::model::Contributor>> {
        let contributors = self.github.contributors(full_name, 10).await?;
        self.store.set_contributors(id, &contributors).await?;
        Ok(contributors)
    }

    /// The README, from cache when it was fetched within the last 7 days.
    pub async fn fetch_readme(&self, id: i64, full_name: &str) -> Result<Option<String>> {
        let cutoff = Utc::now() - Duration::days(7);
        if let Some(cached) = self.store.readme(id, cutoff).await? {
            return Ok(Some(cached));
        }
        let fetched = self.github.readme(full_name).await?;
        if let Some(markdown) = &fetched {
            self.store.set_readme(id, markdown, Utc::now()).await?;
        }
        Ok(fetched)
    }
}
