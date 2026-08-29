//! Integration coverage for the SQLite layer.
//!
//! These run against a real in-memory SQLite with the real migrations, because
//! the behaviour under test — FTS triggers, upsert preservation, tag source
//! separation — lives in SQL, not in Rust.

use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use starlet_core::model::{Contributor, Group, Repo, RepoTag, TagSource};
use starlet_store::Store;

fn repo(id: i64, full_name: &str, description: &str, topics: &[&str]) -> Repo {
    let (owner, name) = full_name.split_once('/').unwrap();
    Repo {
        id,
        node_id: format!("node_{id}"),
        full_name: full_name.into(),
        name: name.into(),
        owner: owner.into(),
        html_url: format!("https://github.com/{full_name}"),
        description: Some(description.into()),
        stargazers: id * 100,
        last_commit_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single(),
        primary_language: Some("Rust".into()),
        languages: BTreeMap::from([("Rust".to_string(), 1000i64)]),
        contributors: Vec::new(),
        starred_at: Utc.with_ymd_and_hms(2026, 2, id as u32, 0, 0, 0).single(),
        archived: false,
        fork: false,
        topics: topics.iter().map(|s| s.to_string()).collect(),
        updated_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single(),
        synced_at: Utc.with_ymd_and_hms(2026, 2, 20, 0, 0, 0).single(),
        tags: Vec::new(),
        groups: Vec::new(),
    }
}

async fn seeded() -> Store {
    let store = Store::open_in_memory().await.unwrap();
    store
        .upsert_repos(&[
            repo(
                1,
                "helix-editor/helix",
                "A post-modern modal text editor",
                &["editor", "rust"],
            ),
            repo(2, "sharkdp/bat", "A cat clone with wings", &["cli", "rust"]),
            repo(
                3,
                "BurntSushi/ripgrep",
                "Recursively search directories",
                &["cli", "search"],
            ),
        ])
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn upsert_then_load_round_trips_every_field() {
    let store = seeded().await;
    let mut repos = store.load_repos().await.unwrap();
    repos.sort_by_key(|r| r.id);
    assert_eq!(repos.len(), 3);

    let helix = &repos[0];
    assert_eq!(helix.full_name, "helix-editor/helix");
    assert_eq!(helix.owner, "helix-editor");
    assert_eq!(helix.stargazers, 100);
    assert_eq!(helix.languages.get("Rust"), Some(&1000));
    assert_eq!(helix.topics, ["editor", "rust"]);
    assert_eq!(
        helix.starred_at,
        Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).single()
    );
}

#[tokio::test]
async fn topics_become_github_tags_and_track_edits() {
    let store = seeded().await;
    let repos = store.load_repos().await.unwrap();
    let helix = repos.iter().find(|r| r.id == 1).unwrap();
    assert!(helix.has_tag("editor"));
    assert!(helix.tags.iter().all(|t| t.source == TagSource::Github));

    // Re-sync with a different topic set: the old topic must disappear.
    store
        .upsert_repos(&[repo(
            1,
            "helix-editor/helix",
            "A post-modern modal text editor",
            &["tui"],
        )])
        .await
        .unwrap();
    let helix = store.load_repo(1).await.unwrap().unwrap();
    assert!(helix.has_tag("tui"));
    assert!(!helix.has_tag("editor"));
}

#[tokio::test]
async fn ai_tags_never_displace_user_tags() {
    let store = seeded().await;
    store.add_user_tag(1, "daily-driver").await.unwrap();
    store
        .set_ai_tags(
            1,
            &[
                RepoTag {
                    name: "daily-driver".into(),
                    source: TagSource::Ai,
                    confidence: 0.9,
                },
                RepoTag {
                    name: "text-editor".into(),
                    source: TagSource::Ai,
                    confidence: 0.8,
                },
            ],
        )
        .await
        .unwrap();

    let helix = store.load_repo(1).await.unwrap().unwrap();
    let daily: Vec<_> = helix
        .tags
        .iter()
        .filter(|t| t.name == "daily-driver")
        .collect();
    assert_eq!(
        daily.len(),
        1,
        "the AI duplicate must be dropped, not stored"
    );
    assert_eq!(daily[0].source, TagSource::User);
    assert!(
        helix
            .tags
            .iter()
            .any(|t| t.name == "text-editor" && t.source == TagSource::Ai)
    );

    // A second run replaces AI tags wholesale but leaves the user tag alone.
    store.set_ai_tags(1, &[]).await.unwrap();
    let helix = store.load_repo(1).await.unwrap().unwrap();
    assert_eq!(helix.tags.len(), 3, "user tag + two github topics");
    assert!(helix.has_tag("daily-driver"));
}

#[tokio::test]
async fn promoting_an_ai_tag_makes_it_a_user_tag() {
    let store = seeded().await;
    store
        .set_ai_tags(
            2,
            &[RepoTag {
                name: "pager".into(),
                source: TagSource::Ai,
                confidence: 0.7,
            }],
        )
        .await
        .unwrap();
    store.promote_tag(2, "pager").await.unwrap();

    let bat = store.load_repo(2).await.unwrap().unwrap();
    let pager: Vec<_> = bat.tags.iter().filter(|t| t.name == "pager").collect();
    assert_eq!(pager.len(), 1);
    assert_eq!(pager[0].source, TagSource::User);
}

#[tokio::test]
async fn fts_finds_repos_by_description_and_ranks_them() {
    let store = seeded().await;
    let hits = store.search_fts(&["modal".into()], 50).await.unwrap();
    assert_eq!(hits.keys().copied().collect::<Vec<_>>(), [1]);
    assert!(
        hits[&1] > 0.0,
        "relevance must be positive, got {}",
        hits[&1]
    );

    // Prefix behaviour: "recur" matches "Recursively".
    let hits = store.search_fts(&["recur".into()], 50).await.unwrap();
    assert!(hits.contains_key(&3));

    // Multiple terms are ANDed.
    assert!(
        store
            .search_fts(&["cat".into(), "wings".into()], 50)
            .await
            .unwrap()
            .contains_key(&2)
    );
    assert!(
        store
            .search_fts(&["cat".into(), "modal".into()], 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn fts_index_tracks_tags_and_deletions() {
    let store = seeded().await;
    assert!(
        store
            .search_fts(&["daily".into()], 50)
            .await
            .unwrap()
            .is_empty()
    );

    store.add_user_tag(1, "daily-driver").await.unwrap();
    assert!(
        store
            .search_fts(&["daily".into()], 50)
            .await
            .unwrap()
            .contains_key(&1),
        "the repo_tags trigger must refresh the FTS tags column"
    );

    store
        .remove_tag(1, "daily-driver", TagSource::User)
        .await
        .unwrap();
    assert!(
        store
            .search_fts(&["daily".into()], 50)
            .await
            .unwrap()
            .is_empty()
    );

    store.delete_repos(&[3]).await.unwrap();
    assert!(
        store
            .search_fts(&["recursively".into()], 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.repo_count().await.unwrap(), 2);
}

#[tokio::test]
async fn fts_index_follows_a_description_change() {
    let store = seeded().await;
    store
        .upsert_repos(&[repo(
            2,
            "sharkdp/bat",
            "Syntax highlighting pager",
            &["cli"],
        )])
        .await
        .unwrap();
    assert!(
        store
            .search_fts(&["wings".into()], 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_fts(&["pager".into()], 50)
            .await
            .unwrap()
            .contains_key(&2)
    );
}

#[tokio::test]
async fn a_refresh_preserves_lazily_fetched_columns() {
    let store = seeded().await;
    let now = Utc::now();
    store
        .set_contributors(
            1,
            &[Contributor {
                login: "archseer".into(),
                avatar_url: "https://example.invalid/a.png".into(),
                contributions: 900,
            }],
        )
        .await
        .unwrap();
    store.set_readme(1, "# Helix", now).await.unwrap();
    store.set_etag(1, Some("W/\"abc\"")).await.unwrap();

    // A star-list refresh knows nothing about contributors, README, or ETag.
    let mut refreshed = repo(
        1,
        "helix-editor/helix",
        "A post-modern modal text editor",
        &["editor"],
    );
    refreshed.stargazers = 31_000;
    refreshed.languages.clear();
    store.upsert_repos(&[refreshed]).await.unwrap();

    let helix = store.load_repo(1).await.unwrap().unwrap();
    assert_eq!(helix.stargazers, 31_000);
    assert_eq!(helix.contributors.len(), 1);
    assert_eq!(
        helix.languages.get("Rust"),
        Some(&1000),
        "empty map must not wipe languages"
    );
    assert_eq!(
        store
            .readme(1, now - chrono::Duration::days(7))
            .await
            .unwrap()
            .as_deref(),
        Some("# Helix")
    );
}

#[tokio::test]
async fn a_stale_readme_reads_as_absent() {
    let store = seeded().await;
    let long_ago = Utc::now() - chrono::Duration::days(30);
    store.set_readme(1, "# Old", long_ago).await.unwrap();
    let cutoff = Utc::now() - chrono::Duration::days(7);
    assert_eq!(store.readme(1, cutoff).await.unwrap(), None);
}

#[tokio::test]
async fn groups_link_by_full_name_and_ignore_unknown_members() {
    let store = seeded().await;
    let linked = store
        .replace_ai_groups(&[Group {
            name: "Terminal tools".into(),
            summary: "Things you run in a shell".into(),
            source: TagSource::Ai,
            members: vec![
                "sharkdp/bat".into(),
                "BurntSushi/ripgrep".into(),
                "not/real".into(),
            ],
        }])
        .await
        .unwrap();
    assert_eq!(linked, 2);

    let facets = store.group_facets().await.unwrap();
    assert_eq!(facets.len(), 1);
    assert_eq!(facets[0].count, 2);

    let bat = store.load_repo(2).await.unwrap().unwrap();
    assert!(bat.in_group("Terminal tools"));

    // A second run replaces the previous AI grouping entirely.
    store
        .replace_ai_groups(&[Group {
            name: "Editors".into(),
            summary: "".into(),
            source: TagSource::Ai,
            members: vec!["helix-editor/helix".into()],
        }])
        .await
        .unwrap();
    let names: Vec<_> = store
        .group_facets()
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, ["Editors"]);
}

#[tokio::test]
async fn tag_facets_are_counted_and_ordered() {
    let store = seeded().await;
    let facets = store.tag_facets().await.unwrap();
    let cli = facets.iter().find(|f| f.name == "cli").unwrap();
    assert_eq!(cli.count, 2);
    assert_eq!(facets[0].count, 2, "most used first");
}

#[tokio::test]
async fn stale_ids_respect_the_watermark() {
    let store = seeded().await;
    let cutoff = Utc.with_ymd_and_hms(2026, 2, 21, 0, 0, 0).unwrap();
    assert_eq!(store.stale_ids(cutoff).await.unwrap().len(), 3);

    store
        .touch_synced_at(1, cutoff + chrono::Duration::hours(1))
        .await
        .unwrap();
    let stale = store.stale_ids(cutoff).await.unwrap();
    assert_eq!(stale.len(), 2);
    assert!(!stale.iter().any(|(id, _, _)| *id == 1));
}

#[tokio::test]
async fn key_value_state_round_trips() {
    let store = Store::open_in_memory().await.unwrap();
    assert_eq!(
        store.get_state(starlet_store::KEY_LAST_SYNC).await.unwrap(),
        None
    );
    store
        .set_state(starlet_store::KEY_LAST_SYNC, "2026-02-20T00:00:00Z")
        .await
        .unwrap();
    store
        .set_state(starlet_store::KEY_LAST_SYNC, "2026-02-21T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(
        store
            .get_state(starlet_store::KEY_LAST_SYNC)
            .await
            .unwrap()
            .as_deref(),
        Some("2026-02-21T00:00:00Z")
    );
    store
        .clear_state(starlet_store::KEY_LAST_SYNC)
        .await
        .unwrap();
    assert_eq!(
        store.get_state(starlet_store::KEY_LAST_SYNC).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn ai_runs_record_start_and_finish() {
    let store = Store::open_in_memory().await.unwrap();
    let id = store
        .begin_ai_run("openai", "gpt-5-mini", 120)
        .await
        .unwrap();
    assert_eq!(store.recent_ai_runs(5).await.unwrap()[0].finished_at, None);
    store.finish_ai_run(id, 118, 0.04).await.unwrap();

    let run = &store.recent_ai_runs(5).await.unwrap()[0];
    assert_eq!(run.provider, "openai");
    assert_eq!(run.repos_count, 118);
    assert!(run.finished_at.is_some());
    assert!((run.cost_estimate - 0.04).abs() < 1e-9);
}

#[tokio::test]
async fn unstarring_cascades_to_tags_and_groups() {
    let store = seeded().await;
    store.add_user_tag(3, "grep").await.unwrap();
    store
        .replace_ai_groups(&[Group {
            name: "Search".into(),
            summary: String::new(),
            source: TagSource::Ai,
            members: vec!["BurntSushi/ripgrep".into()],
        }])
        .await
        .unwrap();

    store.delete_repos(&[3]).await.unwrap();
    assert!(!store.known_ids().await.unwrap().contains(&3));
    assert_eq!(store.group_facets().await.unwrap()[0].count, 0);
    assert!(store.prune_orphan_tags().await.unwrap() >= 1);
    assert!(
        !store
            .tag_facets()
            .await
            .unwrap()
            .iter()
            .any(|f| f.name == "grep")
    );
}
