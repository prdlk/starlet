//! Orchestration tests. No network: a stub provider records what it was asked
//! and answers however the test needs.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use starlet_ai::{
    AiError, AiEvent, AiProvider, BATCH_SIZE, CostEstimate, RepoTags, RepoWithTags, Result, analyze,
};
use starlet_core::{Group, RepoSummary, RepoTag, TagSource};

#[derive(Default)]
struct Stub {
    /// Size of each batch handed to `tag`, in call order.
    seen: Mutex<Vec<usize>>,
    /// Zero-based batch indices that should fail.
    fail_at: HashSet<usize>,
    /// Flip this flag once the batch with the given index has been served, to
    /// simulate the user hitting cancel mid-run.
    cancel_after: Option<(usize, Arc<AtomicBool>)>,
    /// What `group` was given, for the tag-union assertions.
    grouped: Mutex<Vec<RepoWithTags>>,
    group_fails: bool,
}

impl Stub {
    fn batch_sizes(&self) -> Vec<usize> {
        self.seen.lock().expect("stub mutex").clone()
    }
}

#[async_trait::async_trait]
impl AiProvider for Stub {
    fn id(&self) -> &'static str {
        "stub"
    }

    fn model(&self) -> &str {
        "stub-1"
    }

    fn estimate(&self, repos: usize) -> CostEstimate {
        CostEstimate {
            input_tokens: repos as u64,
            output_tokens: repos as u64,
            usd: repos as f64 * 0.01,
        }
    }

    async fn tag(&self, batch: &[RepoSummary]) -> Result<Vec<RepoTags>> {
        let index = {
            let mut seen = self.seen.lock().expect("stub mutex");
            seen.push(batch.len());
            seen.len() - 1
        };

        if let Some((after, flag)) = &self.cancel_after
            && *after == index
        {
            flag.store(true, Ordering::Relaxed);
        }

        if self.fail_at.contains(&index) {
            return Err(AiError::MalformedResponse(format!("batch {index} is bad")));
        }

        Ok(batch
            .iter()
            .map(|repo| RepoTags {
                full_name: repo.full_name.clone(),
                tags: vec![RepoTag {
                    name: "ai-tag".into(),
                    source: TagSource::Ai,
                    confidence: 0.9,
                }],
            })
            .collect())
    }

    async fn group(&self, repos: &[RepoWithTags]) -> Result<Vec<Group>> {
        *self.grouped.lock().expect("stub mutex") = repos.to_vec();
        if self.group_fails {
            return Err(AiError::MalformedResponse("no groups".into()));
        }
        Ok(vec![Group {
            name: "Everything".into(),
            summary: "All of it.".into(),
            source: TagSource::Ai,
            members: repos.iter().map(|r| r.full_name.clone()).collect(),
        }])
    }
}

fn repos(n: usize) -> Vec<RepoSummary> {
    (0..n)
        .map(|i| RepoSummary {
            full_name: format!("owner/repo-{i}"),
            description: Some(format!("repo {i}")),
            topics: Vec::new(),
            primary_language: None,
        })
        .collect()
}

/// Run to completion and collect every event; the channel is closed by then.
async fn run(stub: &Stub, repos: &[RepoSummary], cancel: Arc<AtomicBool>) -> (Result<()>, Vec<AiEvent>) {
    run_with_existing(stub, repos, &HashMap::new(), cancel).await
}

async fn run_with_existing(
    stub: &Stub,
    repos: &[RepoSummary],
    existing: &HashMap<String, Vec<String>>,
    cancel: Arc<AtomicBool>,
) -> (Result<()>, Vec<AiEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = analyze(stub, repos, existing, tx, cancel).await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, events)
}

fn tagged_counts(events: &[AiEvent]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|e| match e {
            AiEvent::Tagged(tags) => Some(tags.len()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn chunks_at_batch_size_and_emits_one_tagged_event_per_batch() {
    let stub = Stub::default();
    let all = repos(60);
    let (outcome, events) = run(&stub, &all, Arc::new(AtomicBool::new(false))).await;

    outcome.expect("a clean run succeeds");
    assert_eq!(BATCH_SIZE, 25);
    assert_eq!(stub.batch_sizes(), [25, 25, 10]);
    assert_eq!(events[0], AiEvent::Started { batches: 3 });
    assert_eq!(tagged_counts(&events), [25, 25, 10]);

    let progress: Vec<&AiEvent> = events
        .iter()
        .filter(|e| matches!(e, AiEvent::Progress { .. }))
        .collect();
    assert_eq!(
        progress,
        [
            &AiEvent::Progress { done: 25, total: 60 },
            &AiEvent::Progress { done: 50, total: 60 },
            &AiEvent::Progress { done: 60, total: 60 },
        ]
    );

    match events.last().expect("a finished event") {
        AiEvent::Finished { repos, cost } => {
            assert_eq!(*repos, 60);
            assert!((cost - 0.6).abs() < 1e-9);
        }
        other => panic!("expected Finished, got {other:?}"),
    }
    assert!(matches!(events[events.len() - 2], AiEvent::Grouped(_)));
}

#[tokio::test]
async fn cancelling_between_batches_stops_the_run() {
    let cancel = Arc::new(AtomicBool::new(false));
    let stub = Stub {
        cancel_after: Some((0, Arc::clone(&cancel))),
        ..Stub::default()
    };
    let all = repos(60);
    let (outcome, events) = run(&stub, &all, Arc::clone(&cancel)).await;

    assert!(matches!(outcome, Err(AiError::Cancelled)));
    assert_eq!(stub.batch_sizes(), [25], "no batch starts after the cancel");
    assert_eq!(
        tagged_counts(&events),
        [25],
        "the in-flight batch is still delivered; it was already paid for"
    );
    assert_eq!(events.last(), Some(&AiEvent::Cancelled));
    assert!(!events.iter().any(|e| matches!(e, AiEvent::Grouped(_))));
}

#[tokio::test]
async fn a_cancel_before_the_first_batch_costs_nothing() {
    let stub = Stub::default();
    let all = repos(10);
    let (outcome, events) = run(&stub, &all, Arc::new(AtomicBool::new(true))).await;

    assert!(matches!(outcome, Err(AiError::Cancelled)));
    assert!(stub.batch_sizes().is_empty());
    assert_eq!(events, [AiEvent::Started { batches: 1 }, AiEvent::Cancelled]);
}

#[tokio::test]
async fn one_failing_batch_does_not_abort_the_run() {
    let stub = Stub {
        fail_at: HashSet::from([1]),
        ..Stub::default()
    };
    let all = repos(60);
    let (outcome, events) = run(&stub, &all, Arc::new(AtomicBool::new(false))).await;

    outcome.expect("a partial run still succeeds");
    assert_eq!(stub.batch_sizes(), [25, 25, 10], "every batch is attempted");
    assert_eq!(tagged_counts(&events), [25, 10], "the bad batch yields nothing");

    let failures: Vec<&AiEvent> = events
        .iter()
        .filter(|e| matches!(e, AiEvent::Failed(_)))
        .collect();
    assert_eq!(failures.len(), 1, "the failure is surfaced, not swallowed");

    // Progress still reaches the total so the bar completes.
    assert!(events.contains(&AiEvent::Progress { done: 60, total: 60 }));
    assert!(matches!(
        events.last(),
        Some(AiEvent::Finished { repos: 35, .. })
    ));
}

#[tokio::test]
async fn a_run_where_every_batch_fails_is_an_error() {
    let stub = Stub {
        fail_at: HashSet::from([0, 1]),
        ..Stub::default()
    };
    let all = repos(30);
    let (outcome, events) = run(&stub, &all, Arc::new(AtomicBool::new(false))).await;

    assert!(matches!(outcome, Err(AiError::MalformedResponse(_))));
    assert!(tagged_counts(&events).is_empty());
    assert!(!events.iter().any(|e| matches!(e, AiEvent::Finished { .. })));
}

#[tokio::test]
async fn grouping_sees_existing_tags_unioned_with_the_new_ones() {
    let stub = Stub::default();
    let all = repos(2);
    let existing = HashMap::from([
        (
            "owner/repo-0".to_string(),
            vec!["rust".to_string(), "ai-tag".to_string()],
        ),
        ("missing/repo".to_string(), vec!["ignored".to_string()]),
    ]);
    let (outcome, _) =
        run_with_existing(&stub, &all, &existing, Arc::new(AtomicBool::new(false))).await;
    outcome.expect("run succeeds");

    let grouped = stub.grouped.lock().expect("stub mutex").clone();
    assert_eq!(grouped.len(), 2, "every repo reaches the grouper");
    assert_eq!(
        grouped[0].tags,
        ["rust", "ai-tag"],
        "existing tags lead and duplicates collapse"
    );
    assert_eq!(grouped[1].tags, ["ai-tag"]);
    assert_eq!(grouped[0].description.as_deref(), Some("repo 0"));
}

#[tokio::test]
async fn a_failed_grouping_pass_keeps_the_tags_but_reports_the_error() {
    let stub = Stub {
        group_fails: true,
        ..Stub::default()
    };
    let all = repos(5);
    let (outcome, events) = run(&stub, &all, Arc::new(AtomicBool::new(false))).await;

    assert!(matches!(outcome, Err(AiError::MalformedResponse(_))));
    assert_eq!(tagged_counts(&events), [5], "tags were already delivered");
    assert!(matches!(events.last(), Some(AiEvent::Failed(_))));
}

#[tokio::test]
async fn an_empty_library_finishes_without_calling_the_provider() {
    let stub = Stub::default();
    let (outcome, events) = run(&stub, &[], Arc::new(AtomicBool::new(false))).await;

    outcome.expect("nothing to do is not a failure");
    assert!(stub.batch_sizes().is_empty());
    assert_eq!(
        events,
        [
            AiEvent::Started { batches: 0 },
            AiEvent::Finished { repos: 0, cost: 0.0 },
        ]
    );
}
