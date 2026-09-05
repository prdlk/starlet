//! Sync engine coverage against recorded GitHub responses.
//!
//! Every test drives the real `SyncEngine` against a `wiremock` server, so the
//! media types, pagination headers, conditional requests and GraphQL documents
//! are exercised exactly as they would be in production.

use serde_json::{Value, json};
use starlet_store::Store;
use starlet_sync::{GitHub, SyncEngine, SyncEvent, SyncMode};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `star+json` entry.
fn starred(id: i64, full_name: &str, stars: i64, starred_at: &str) -> Value {
    let (owner, name) = full_name.split_once('/').unwrap();
    json!({
        "starred_at": starred_at,
        "repo": {
            "id": id,
            "node_id": format!("node_{id}"),
            "name": name,
            "full_name": full_name,
            "owner": { "login": owner },
            "html_url": format!("https://github.com/{full_name}"),
            "description": format!("{name} does something useful"),
            "fork": false,
            "archived": false,
            "updated_at": "2026-02-19T12:00:00Z",
            "pushed_at": "2026-02-18T22:31:05Z",
            "stargazers_count": stars,
            "language": "Rust",
            "topics": ["cli", "rust"]
        }
    })
}

fn headers(template: ResponseTemplate) -> ResponseTemplate {
    template
        .insert_header("x-ratelimit-limit", "5000")
        .insert_header("x-ratelimit-remaining", "4999")
}

/// Mount the GraphQL endpoint: star count and language batches.
async fn mount_graphql(server: &MockServer, total: i64) {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("starredRepositories"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "viewer": { "starredRepositories": { "totalCount": total } } }
        }))))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("languages"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "r0": {
                    "databaseId": 1,
                    "languages": { "edges": [
                        { "size": 81234, "node": { "name": "Rust" } },
                        { "size": 210, "node": { "name": "Shell" } }
                    ]}
                }
            }
        }))))
        .mount(server)
        .await;
}

fn drain(rx: &mut UnboundedReceiver<SyncEvent>) -> Vec<SyncEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

fn removed_ids(events: &[SyncEvent]) -> Vec<i64> {
    events
        .iter()
        .filter_map(|e| match e {
            SyncEvent::Removed(ids) => Some(ids.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn upserted_count(events: &[SyncEvent]) -> usize {
    events
        .iter()
        .filter_map(|e| match e {
            SyncEvent::Upserted(repos) => Some(repos.len()),
            _ => None,
        })
        .sum()
}

async fn engine(server: &MockServer) -> (SyncEngine, Store) {
    let store = Store::open_in_memory().await.unwrap();
    let github = GitHub::new("test-token")
        .unwrap()
        .with_base_url(server.uri());
    (SyncEngine::new(github, store.clone()), store)
}

#[tokio::test]
async fn full_sync_pages_through_the_star_list() {
    let server = MockServer::start().await;
    let next_link = format!("<{}/user/starred?page=2>; rel=\"next\"", server.uri());

    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "1"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            headers(ResponseTemplate::new(200).set_body_json(json!([
                starred(1, "helix-editor/helix", 39_000, "2026-02-19T10:00:00Z"),
                starred(2, "sharkdp/bat", 48_000, "2026-02-18T10:00:00Z"),
            ])))
            .insert_header("link", next_link.as_str()),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "2"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            starred(3, "BurntSushi/ripgrep", 47_000, "2026-01-01T10:00:00Z")
        ]))))
        .mount(&server)
        .await;

    mount_graphql(&server, 3).await;

    let (engine, store) = engine(&server).await;
    let (tx, mut rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Full, &tx).await.expect("sync");

    assert_eq!(summary.seen, 3, "both pages must be consumed");
    assert_eq!(store.repo_count().await.unwrap(), 3);

    let events = drain(&mut rx);
    assert!(matches!(
        events.first(),
        Some(SyncEvent::Started(SyncMode::Full))
    ));
    assert!(matches!(events.last(), Some(SyncEvent::Finished(_))));
    assert_eq!(upserted_count(&events), 3 + summary.languages_filled);

    // The watermark is the newest star seen, not the last one processed.
    let watermark = store
        .get_state(starlet_store::KEY_STAR_WATERMARK)
        .await
        .unwrap();
    assert_eq!(watermark.as_deref(), Some("2026-02-19T10:00:00Z"));
    assert_eq!(
        store
            .get_state(starlet_store::KEY_INITIAL_SYNC_DONE)
            .await
            .unwrap()
            .as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn full_sync_backfills_languages_over_graphql() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            starred(1, "helix-editor/helix", 39_000, "2026-02-19T10:00:00Z")
        ]))))
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    let (tx, _rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Full, &tx).await.unwrap();

    assert_eq!(summary.languages_filled, 1);
    let helix = store.load_repo(1).await.unwrap().unwrap();
    assert_eq!(helix.languages.get("Rust"), Some(&81_234));
    assert_eq!(helix.languages.get("Shell"), Some(&210));
    assert_eq!(helix.languages_by_size()[0].0, "Rust");
}

#[tokio::test]
async fn full_sync_detects_unstars() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            starred(1, "helix-editor/helix", 39_000, "2026-02-19T10:00:00Z")
        ]))))
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    // Seed a repo the remote listing no longer contains.
    let mut ghost = starlet_core::model::Repo {
        id: 99,
        node_id: "node_99".into(),
        full_name: "old/ghost".into(),
        name: "ghost".into(),
        owner: "old".into(),
        html_url: "https://example.invalid".into(),
        ..Default::default()
    };
    ghost.stargazers = 1;
    store.upsert_repos(&[ghost]).await.unwrap();

    let (tx, mut rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Full, &tx).await.unwrap();

    assert_eq!(summary.removed, 1);
    assert_eq!(removed_ids(&drain(&mut rx)), [99]);
    assert!(!store.known_ids().await.unwrap().contains(&99));
}

#[tokio::test]
async fn incremental_sync_stops_at_the_watermark() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "1"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            starred(10, "new/one", 10, "2026-03-02T00:00:00Z"),
            starred(11, "new/two", 20, "2026-03-01T00:00:00Z"),
            starred(1, "old/known", 30, "2026-01-01T00:00:00Z"),
        ]))))
        .mount(&server)
        .await;
    // Second page must never be requested; leaving it unmounted proves it.
    mount_graphql(&server, 3).await;

    let (engine, store) = engine(&server).await;
    store
        .set_state(starlet_store::KEY_STAR_WATERMARK, "2026-02-01T00:00:00Z")
        .await
        .unwrap();
    store
        .upsert_repos(&[starlet_core::model::Repo {
            id: 1,
            node_id: "node_1".into(),
            full_name: "old/known".into(),
            name: "known".into(),
            owner: "old".into(),
            html_url: "https://example.invalid".into(),
            ..Default::default()
        }])
        .await
        .unwrap();

    let (tx, _rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Incremental, &tx).await.unwrap();

    assert_eq!(summary.seen, 2, "only stars newer than the watermark");
    assert_eq!(store.repo_count().await.unwrap(), 3);
    assert_eq!(
        store
            .get_state(starlet_store::KEY_STAR_WATERMARK)
            .await
            .unwrap()
            .as_deref(),
        Some("2026-03-02T00:00:00Z")
    );
}

#[tokio::test]
async fn incremental_sync_stops_at_a_missing_star_timestamp() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .and(query_param("page", "1"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            starred(10, "new/one", 10, "2026-03-02T00:00:00Z"),
            starred(11, "missing/timestamp", 20, "not-a-timestamp"),
            starred(12, "new/two", 30, "2026-03-01T00:00:00Z"),
        ]))))
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    store
        .set_state(starlet_store::KEY_STAR_WATERMARK, "2026-02-01T00:00:00Z")
        .await
        .unwrap();

    let (tx, _rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Incremental, &tx).await.unwrap();

    assert_eq!(
        summary.seen, 1,
        "an invalid timestamp ends the incremental scan"
    );
    assert_eq!(store.repo_count().await.unwrap(), 1);
}

#[tokio::test]
async fn incremental_sync_without_a_watermark_falls_back_to_full() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(
            ResponseTemplate::new(200).set_body_json(json!([starred(
                1,
                "a/b",
                1,
                "2026-02-19T10:00:00Z"
            )])),
        ))
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    engine
        .run(SyncMode::Incremental, &unbounded_channel().0)
        .await
        .unwrap();

    assert_eq!(
        store
            .get_state(starlet_store::KEY_INITIAL_SYNC_DONE)
            .await
            .unwrap()
            .as_deref(),
        Some("1"),
        "the fallback must complete a real full sync"
    );
}

#[tokio::test]
async fn a_count_mismatch_triggers_reconciliation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(
            ResponseTemplate::new(200).set_body_json(json!([starred(
                1,
                "a/b",
                1,
                "2026-01-01T00:00:00Z"
            )])),
        ))
        .mount(&server)
        .await;
    // The account has one star; the local mirror will have two.
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    store
        .set_state(starlet_store::KEY_STAR_WATERMARK, "2026-02-01T00:00:00Z")
        .await
        .unwrap();
    for (id, full_name) in [(1i64, "a/b"), (2, "gone/away")] {
        let (owner, name) = full_name.split_once('/').unwrap();
        store
            .upsert_repos(&[starlet_core::model::Repo {
                id,
                node_id: format!("node_{id}"),
                full_name: full_name.into(),
                name: name.into(),
                owner: owner.into(),
                html_url: "https://example.invalid".into(),
                ..Default::default()
            }])
            .await
            .unwrap();
    }

    let (tx, mut rx) = unbounded_channel();
    let summary = engine.run(SyncMode::Incremental, &tx).await.unwrap();

    assert_eq!(summary.removed, 1);
    assert_eq!(removed_ids(&drain(&mut rx)), [2]);
    assert_eq!(store.repo_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_not_modified_refresh_costs_nothing_but_a_touch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([]))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/stale/repo"))
        .and(header("if-none-match", "W/\"cafe\""))
        .respond_with(headers(ResponseTemplate::new(304)))
        .expect(1)
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    store
        .set_state(starlet_store::KEY_STAR_WATERMARK, "2026-02-01T00:00:00Z")
        .await
        .unwrap();
    let mut stale = starlet_core::model::Repo {
        id: 5,
        node_id: "node_5".into(),
        full_name: "stale/repo".into(),
        name: "repo".into(),
        owner: "stale".into(),
        html_url: "https://example.invalid".into(),
        ..Default::default()
    };
    stale.synced_at = Some(chrono::Utc::now() - chrono::Duration::days(3));
    stale.languages.insert("Rust".into(), 1);
    store.upsert_repos(&[stale]).await.unwrap();
    store.set_etag(5, Some("W/\"cafe\"")).await.unwrap();

    let summary = engine
        .run(SyncMode::Incremental, &unbounded_channel().0)
        .await
        .unwrap();
    assert_eq!(summary.metadata_refreshed, 0, "304 is not a refresh");

    // The touch must move `synced_at` forward so the repo is not retried.
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    assert!(store.stale_ids(cutoff).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_modified_refresh_updates_metadata_and_keeps_the_star_date() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([]))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/fresh/repo"))
        .respond_with(
            headers(
                ResponseTemplate::new(200)
                    .set_body_json(starred(7, "fresh/repo", 12_345, "ignored")["repo"].clone()),
            )
            .insert_header("etag", "W/\"new\""),
        )
        .mount(&server)
        .await;
    mount_graphql(&server, 1).await;

    let (engine, store) = engine(&server).await;
    store
        .set_state(starlet_store::KEY_STAR_WATERMARK, "2026-02-01T00:00:00Z")
        .await
        .unwrap();
    let mut old = starlet_core::model::Repo {
        id: 7,
        node_id: "node_7".into(),
        full_name: "fresh/repo".into(),
        name: "repo".into(),
        owner: "fresh".into(),
        html_url: "https://example.invalid".into(),
        stargazers: 1,
        ..Default::default()
    };
    old.starred_at = chrono::DateTime::parse_from_rfc3339("2025-06-01T00:00:00Z")
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc));
    old.synced_at = Some(chrono::Utc::now() - chrono::Duration::days(3));
    old.languages.insert("Rust".into(), 1);
    store.upsert_repos(&[old]).await.unwrap();

    let summary = engine
        .run(SyncMode::Incremental, &unbounded_channel().0)
        .await
        .unwrap();
    assert_eq!(summary.metadata_refreshed, 1);

    let repo = store.load_repo(7).await.unwrap().unwrap();
    assert_eq!(repo.stargazers, 12_345);
    assert_eq!(
        repo.starred_at.map(|d| d.to_rfc3339()),
        Some("2025-06-01T00:00:00+00:00".into()),
        "a metadata refresh must not lose when the user starred it"
    );
}

#[tokio::test]
async fn rate_limiting_surfaces_as_a_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("retry-after", "60")
                .set_body_string("API rate limit exceeded"),
        )
        .mount(&server)
        .await;

    let (engine, _store) = engine(&server).await;
    let (tx, mut rx) = unbounded_channel();
    let err = engine.run(SyncMode::Full, &tx).await.unwrap_err();

    assert!(
        matches!(
            err,
            starlet_sync::SyncError::RateLimited {
                retry_after_secs: Some(60),
                ..
            }
        ),
        "got {err:?}"
    );
    assert!(
        drain(&mut rx)
            .iter()
            .any(|e| matches!(e, SyncEvent::Failed(_)))
    );
}

#[tokio::test]
async fn an_expired_token_asks_for_reauthentication() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/starred"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Bad credentials"))
        .mount(&server)
        .await;

    let (engine, _store) = engine(&server).await;
    let err = engine
        .run(SyncMode::Full, &unbounded_channel().0)
        .await
        .unwrap_err();
    assert!(err.needs_reauth(), "got {err:?}");
}

#[tokio::test]
async fn contributors_and_readme_are_fetched_and_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/a/b/contributors"))
        .respond_with(headers(ResponseTemplate::new(200).set_body_json(json!([
            { "login": "alice", "avatar_url": "https://example.invalid/a.png", "contributions": 400 },
            { "login": "bob", "avatar_url": "https://example.invalid/b.png", "contributions": 12 }
        ]))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/a/b/readme"))
        .and(header("accept", "application/vnd.github.raw"))
        .respond_with(headers(
            ResponseTemplate::new(200).set_body_string("# Title\n\nBody."),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let (engine, store) = engine(&server).await;
    store
        .upsert_repos(&[starlet_core::model::Repo {
            id: 42,
            node_id: "node_42".into(),
            full_name: "a/b".into(),
            name: "b".into(),
            owner: "a".into(),
            html_url: "https://example.invalid".into(),
            ..Default::default()
        }])
        .await
        .unwrap();

    let contributors = engine.fetch_contributors(42, "a/b").await.unwrap();
    assert_eq!(contributors.len(), 2);
    assert_eq!(contributors[0].login, "alice");
    assert_eq!(
        store
            .load_repo(42)
            .await
            .unwrap()
            .unwrap()
            .contributors
            .len(),
        2
    );

    assert_eq!(
        engine.fetch_readme(42, "a/b").await.unwrap().as_deref(),
        Some("# Title\n\nBody.")
    );
    // Second call is served from cache; the `.expect(1)` above enforces it.
    assert!(engine.fetch_readme(42, "a/b").await.unwrap().is_some());
}

#[tokio::test]
async fn a_repository_without_a_readme_is_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/a/b/readme"))
        .respond_with(headers(
            ResponseTemplate::new(404).set_body_string("Not Found"),
        ))
        .mount(&server)
        .await;

    let (engine, store) = engine(&server).await;
    store
        .upsert_repos(&[starlet_core::model::Repo {
            id: 42,
            node_id: "n".into(),
            full_name: "a/b".into(),
            name: "b".into(),
            owner: "a".into(),
            html_url: "https://example.invalid".into(),
            ..Default::default()
        }])
        .await
        .unwrap();
    assert_eq!(engine.fetch_readme(42, "a/b").await.unwrap(), None);
}

mod device_flow {
    use starlet_sync::{DeviceFlow, PollOutcome};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_device_grant_carries_the_user_code_and_interval() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dc-123",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 5
            })))
            .mount(&server)
            .await;

        let flow = DeviceFlow::new("Iv1.test")
            .unwrap()
            .with_base_url(&server.uri());
        let grant = flow.request_code().await.unwrap();
        assert_eq!(grant.user_code, "WDJB-MJHT");
        assert_eq!(grant.poll_interval().as_secs(), 5);
        assert_eq!(grant.expires_in().as_secs(), 900);
    }

    #[tokio::test]
    async fn polling_reports_pending_slow_down_and_success() {
        for (body, expected) in [
            (
                serde_json::json!({ "error": "authorization_pending" }),
                PollOutcome::Pending,
            ),
            (
                serde_json::json!({ "error": "slow_down", "interval": 10 }),
                PollOutcome::SlowDown(std::time::Duration::from_secs(10)),
            ),
            (
                serde_json::json!({ "access_token": "gho_secret", "token_type": "bearer" }),
                PollOutcome::Authorized("gho_secret".into()),
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/login/oauth/access_token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;

            let flow = DeviceFlow::new("Iv1.test")
                .unwrap()
                .with_base_url(&server.uri());
            assert_eq!(flow.poll_once("dc-123").await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn a_declined_or_expired_grant_is_an_error() {
        for error in ["access_denied", "expired_token"] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/login/oauth/access_token"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "error": error })),
                )
                .mount(&server)
                .await;

            let flow = DeviceFlow::new("Iv1.test")
                .unwrap()
                .with_base_url(&server.uri());
            let err = flow.poll_once("dc-123").await.unwrap_err();
            assert!(err.needs_reauth(), "{error} should ask for another sign-in");
        }
    }
}
