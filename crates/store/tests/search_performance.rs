//! The full-text half of search, at the stated corpus size.
//!
//! FTS runs off the application thread, so it does not spend the 10 ms frame
//! budget directly. It still has to land within a frame or two or the results
//! visibly re-order after the user has started reading, which is why it is
//! measured rather than assumed.

use std::collections::BTreeMap;
use std::time::Instant;

use starlet_core::model::Repo;
use starlet_store::Store;

const CORPUS: i64 = 5_000;
/// Measured median on the reference machine: 3-4 ms, comfortably inside two
/// frames at 120 Hz. The assertion sits far above that because a shared CI
/// runner is several times slower; it is here to catch a missing index or a
/// full table scan, not to police milliseconds.
const BUDGET_MS: f64 = 60.0;

async fn seeded() -> Store {
    let store = Store::open_in_memory().await.expect("open");
    let words = [
        "modal",
        "editor",
        "search",
        "database",
        "async",
        "runtime",
        "parser",
        "shell",
        "graphics",
        "networking",
    ];
    let repos: Vec<Repo> = (1..=CORPUS)
        .map(|id| {
            let word = words[(id as usize) % words.len()];
            let other = words[(id as usize / 3) % words.len()];
            Repo {
                id,
                node_id: format!("n{id}"),
                full_name: format!("owner{}/repo-{id}", id % 50),
                name: format!("repo-{id}"),
                owner: format!("owner{}", id % 50),
                html_url: format!("https://example.invalid/{id}"),
                description: Some(format!("A {word} tool for {other} workloads")),
                stargazers: id * 7,
                topics: vec![word.to_string(), other.to_string()],
                languages: BTreeMap::new(),
                ..Default::default()
            }
        })
        .collect();
    for chunk in repos.chunks(500) {
        store.upsert_repos(chunk).await.expect("seed");
    }
    store
}

#[tokio::test]
async fn full_text_search_answers_within_a_frame_or_two() {
    let store = seeded().await;

    // Warm the page cache and prove the query actually matches.
    let hits = store.search_fts(&["modal".into()], 2_000).await.unwrap();
    assert!(
        hits.len() > 100,
        "expected a broad match, got {}",
        hits.len()
    );

    let mut samples = Vec::new();
    for term in ["modal", "edit", "data", "runt", "graph"] {
        let started = Instant::now();
        let hits = store.search_fts(&[term.to_string()], 2_000).await.unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert!(!hits.is_empty(), "'{term}' matched nothing");
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[samples.len() / 2];

    assert!(
        median < BUDGET_MS,
        "FTS over {CORPUS} repositories took {median:.2} ms, budget is {BUDGET_MS} ms"
    );
    eprintln!("fts({CORPUS}) median: {median:.2} ms");
}

#[tokio::test]
async fn loading_the_whole_mirror_is_fast_enough_for_a_cold_start() {
    let store = seeded().await;

    let started = Instant::now();
    let repos = store.load_repos().await.expect("load");
    let elapsed = started.elapsed().as_secs_f64() * 1_000.0;

    assert_eq!(repos.len() as i64, CORPUS);
    // This runs on the I/O runtime, not the application thread: the window is
    // already painted and interactive before it finishes, so it is bounded
    // against the whole cold-start budget rather than a frame. The guard
    // exists to catch an accidental N+1 query, which is the failure mode that
    // would turn 80 ms into 8 s.
    // Measured on the reference machine: about 75 ms.
    assert!(
        elapsed < 800.0,
        "loading {CORPUS} repositories took {elapsed:.2} ms"
    );
    eprintln!("load_repos({CORPUS}): {elapsed:.2} ms");
}
