//! The run loop the UI drives: batch, tag, group, report.
//!
//! Progress is streamed rather than returned so the UI can write each batch to
//! the store as it lands. A run that dies halfway therefore still leaves the
//! user with the tags they already paid for.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use starlet_core::{Group, RepoSummary};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{AiError, AiProvider, RepoTags, RepoWithTags, Result};

/// Repos per tagging request.
///
/// Small enough that one bad batch loses little work and that the reply fits
/// comfortably inside every provider's output limit; large enough that the
/// per-batch system prompt is amortised.
pub const BATCH_SIZE: usize = 25;

/// Everything a run reports back, in the order it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum AiEvent {
    Started { batches: usize },
    Progress { done: usize, total: usize },
    /// One batch's tags, ready to persist.
    Tagged(Vec<RepoTags>),
    Grouped(Vec<Group>),
    Finished { repos: usize, cost: f64 },
    /// A recoverable failure. The run continues; this is a notification, not a
    /// terminal state.
    Failed(String),
    Cancelled,
}

/// Tag `repos` in batches, then group the whole library in one pass.
///
/// `tags_by_repo` holds the tags the store already has (GitHub topics, user
/// tags) keyed by `full_name`; they are unioned with the fresh AI tags so the
/// grouper sees everything known about a repo.
///
/// Failure policy: one failing batch is reported through [`AiEvent::Failed`]
/// and the run carries on, because 24 good batches are worth keeping. Only a
/// run where *every* batch failed returns `Err`. Cancellation is checked
/// between batches — a request in flight is allowed to finish rather than being
/// torn down, since it has already been paid for.
pub async fn analyze(
    provider: &dyn AiProvider,
    repos: &[RepoSummary],
    tags_by_repo: &HashMap<String, Vec<String>>,
    events: UnboundedSender<AiEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let total = repos.len();
    let batches: Vec<&[RepoSummary]> = repos.chunks(BATCH_SIZE).collect();
    emit(&events, AiEvent::Started {
        batches: batches.len(),
    })?;

    if batches.is_empty() {
        emit(&events, AiEvent::Finished {
            repos: 0,
            cost: 0.0,
        })?;
        return Ok(());
    }

    let mut tagged: Vec<RepoTags> = Vec::with_capacity(total);
    let mut done = 0usize;
    let mut failures = 0usize;
    let mut last_error: Option<AiError> = None;

    for batch in &batches {
        if cancelled(&cancel) {
            emit(&events, AiEvent::Cancelled)?;
            return Err(AiError::Cancelled);
        }

        match provider.tag(batch).await {
            Ok(result) => {
                tagged.extend(result.iter().cloned());
                emit(&events, AiEvent::Tagged(result))?;
            }
            Err(error) => {
                tracing::warn!(
                    provider = provider.id(),
                    repos = batch.len(),
                    error = %error,
                    "tagging batch failed; continuing with the rest of the run"
                );
                failures += 1;
                emit(&events, AiEvent::Failed(error.to_string()))?;
                last_error = Some(error);
            }
        }

        // Progress counts attempted repos, not tagged ones: the bar must still
        // reach the end when a batch fails.
        done += batch.len();
        emit(&events, AiEvent::Progress { done, total })?;
    }

    if failures == batches.len() {
        // Nothing worked. Almost always a bad key, a wrong base URL, or a model
        // that does not exist, so surface the real error rather than a summary.
        return Err(last_error.unwrap_or_else(|| {
            AiError::MalformedResponse("every tagging batch failed".into())
        }));
    }

    if cancelled(&cancel) {
        emit(&events, AiEvent::Cancelled)?;
        return Err(AiError::Cancelled);
    }

    let grouping_input = merge_tags(repos, &tagged, tags_by_repo);
    match provider.group(&grouping_input).await {
        Ok(groups) => emit(&events, AiEvent::Grouped(groups))?,
        Err(error) => {
            // The tags are already emitted and persisted, so the caller keeps
            // them; only the sidebar is missing.
            tracing::warn!(provider = provider.id(), error = %error, "grouping pass failed");
            emit(&events, AiEvent::Failed(error.to_string()))?;
            return Err(error);
        }
    }

    emit(&events, AiEvent::Finished {
        repos: tagged.len(),
        cost: provider.estimate(total).usd,
    })?;
    Ok(())
}

/// Flatten fresh AI tags together with whatever the store already knew.
///
/// Existing tags come first because GitHub topics and user tags are the more
/// trustworthy signal, and order is the only priority hint the model gets.
fn merge_tags(
    repos: &[RepoSummary],
    tagged: &[RepoTags],
    tags_by_repo: &HashMap<String, Vec<String>>,
) -> Vec<RepoWithTags> {
    let fresh: HashMap<&str, &[starlet_core::RepoTag]> = tagged
        .iter()
        .map(|t| (t.full_name.as_str(), t.tags.as_slice()))
        .collect();

    repos
        .iter()
        .map(|repo| {
            let mut tags: Vec<String> = Vec::new();
            let mut push = |name: &str| {
                let name = name.trim();
                if !name.is_empty() && !tags.iter().any(|t| t == name) {
                    tags.push(name.to_string());
                }
            };

            if let Some(existing) = tags_by_repo.get(&repo.full_name) {
                existing.iter().for_each(|t| push(t));
            }
            if let Some(ai) = fresh.get(repo.full_name.as_str()) {
                ai.iter().for_each(|t| push(&t.name));
            }

            RepoWithTags {
                full_name: repo.full_name.clone(),
                description: repo.description.clone(),
                tags,
            }
        })
        .collect()
}

fn cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

/// A dropped receiver means the window closed mid-run; treat it exactly like a
/// cancel so the loop stops instead of burning tokens for nobody.
fn emit(events: &UnboundedSender<AiEvent>, event: AiEvent) -> Result<()> {
    events.send(event).map_err(|_| AiError::Cancelled)
}
