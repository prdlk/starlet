//! Ranking must stay inside the per-keystroke budget at the stated corpus size.
//!
//! The product target is under 10 ms per keystroke on 5 000 stars. The fuzzy
//! pass is the synchronous half of that budget — it runs on the application
//! thread before the frame — so it is the half worth guarding with a test.
//!
//! Measured medians on the reference machine, debug profile: 0.9 ms for the
//! worst-case one-character query, 1.1 ms with filters, 0.1 ms for the browse
//! ordering. The assertion is set at the 10 ms product budget, which leaves an
//! order of magnitude of headroom — enough that a shared CI runner passes and
//! an algorithmic regression still fails.

use std::collections::HashMap;
use std::time::Instant;

use starlet_core::model::Repo;
use starlet_core::query;
use starlet_core::rank::Ranker;

const CORPUS: usize = 5_000;
/// Per-keystroke budget for the synchronous half of search.
const BUDGET_MS: f64 = 10.0;

fn corpus() -> Vec<Repo> {
    let owners = [
        "rust-lang",
        "tokio-rs",
        "helix-editor",
        "BurntSushi",
        "sharkdp",
        "clap-rs",
        "serde-rs",
        "hyperium",
        "bevyengine",
        "nushell",
    ];
    let names = [
        "async-engine",
        "fast-parser",
        "tiny-shell",
        "modal-editor",
        "portable-index",
        "zero-copy-store",
        "incremental-graph",
        "declarative-router",
        "headless-daemon",
        "embedded-toolkit",
    ];
    (0..CORPUS as i64)
        .map(|id| {
            let owner = owners[(id as usize) % owners.len()];
            let name = format!("{}-{id}", names[(id as usize / 7) % names.len()]);
            Repo {
                id,
                full_name: format!("{owner}/{name}"),
                owner: owner.into(),
                name,
                description: Some(format!("A component for workload {id}")),
                stargazers: (id * 37) % 200_000,
                primary_language: Some(if id % 3 == 0 {
                    "Rust".into()
                } else {
                    "Go".into()
                }),
                ..Default::default()
            }
        })
        .collect()
}

/// Median of `runs` timings, in milliseconds.
fn median_ms(runs: usize, mut f: impl FnMut()) -> f64 {
    let mut samples: Vec<f64> = (0..runs)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed().as_secs_f64() * 1_000.0
        })
        .collect();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() / 2]
}

#[test]
fn a_keystroke_ranks_five_thousand_repositories_within_budget() {
    let repos = corpus();
    let mut ranker = Ranker::new();
    let fts = HashMap::new();

    // The worst realistic case is a one-character query: almost everything
    // matches, so the ranker scores and sorts the entire corpus.
    let parsed = query::parse("a");
    let warm = ranker.rank(&parsed, &repos, &fts);
    assert!(!warm.is_empty(), "the benchmark query must actually match");

    let median = median_ms(20, || {
        let _ = ranker.rank(&parsed, &repos, &fts);
    });
    assert!(
        median < BUDGET_MS,
        "ranking {CORPUS} repositories took {median:.2} ms, budget is {BUDGET_MS} ms"
    );
    eprintln!("rank({CORPUS}) median: {median:.2} ms");
}

#[test]
fn filtering_and_ranking_together_stay_within_budget() {
    let repos = corpus();
    let mut ranker = Ranker::new();
    let fts = HashMap::new();
    let parsed = query::parse("lang:rust engine stars:>1000");

    let median = median_ms(20, || {
        // This mirrors what the search view does: filter into a candidate
        // slice, then rank it.
        let candidates: Vec<&Repo> = repos.iter().filter(|r| parsed.matches(r)).collect();
        let owned: Vec<Repo> = candidates.into_iter().cloned().collect();
        let _ = ranker.rank(&parsed, &owned, &fts);
    });
    assert!(
        median < BUDGET_MS,
        "filter + rank took {median:.2} ms, budget is {BUDGET_MS} ms"
    );
    eprintln!("filter+rank({CORPUS}) median: {median:.2} ms");
}

#[test]
fn an_empty_query_orders_the_whole_corpus_within_budget() {
    let repos = corpus();
    let mut ranker = Ranker::new();
    let fts = HashMap::new();
    let parsed = query::parse("");

    let median = median_ms(20, || {
        let _ = ranker.rank(&parsed, &repos, &fts);
    });
    assert!(median < BUDGET_MS, "browse ordering took {median:.2} ms");
    eprintln!("browse({CORPUS}) median: {median:.2} ms");
}
