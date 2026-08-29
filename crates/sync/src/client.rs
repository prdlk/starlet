//! The GitHub HTTP client.
//!
//! REST and GraphQL live behind one type so rate-limit accounting, conditional
//! requests, and the base-URL seam used by the fixture tests are all in one
//! place.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use starlet_core::model::{Contributor, LanguageBytes};

use crate::wire::{ContributorPayload, RepoPayload, StarredItem, Viewer};
use crate::{Result, SyncError};

const DEFAULT_BASE: &str = "https://api.github.com";
const STAR_MEDIA_TYPE: &str = "application/vnd.github.star+json";
const JSON_MEDIA_TYPE: &str = "application/vnd.github+json";
const RAW_MEDIA_TYPE: &str = "application/vnd.github.raw";
const API_VERSION: &str = "2022-11-28";
const AGENT: &str = concat!("starlet/", env!("CARGO_PKG_VERSION"));

/// Stop issuing requests once the hourly budget is this close to exhausted, so
/// an interactive action (opening a detail sheet) still has room after a sync.
const RESERVE: u32 = 50;

/// How many repositories one GraphQL document asks about.
///
/// Each aliased `repository` field is one node; 25 keeps the query well under
/// GitHub's 500 000-node limit while amortising the request overhead.
pub const GRAPHQL_BATCH: usize = 25;

/// A snapshot of the primary rate limit, as reported by the last response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    pub reset_at: Option<DateTime<Utc>>,
}

impl RateLimit {
    fn absorb(&mut self, headers: &HeaderMap) {
        let num = |name: &str| -> Option<u32> { headers.get(name)?.to_str().ok()?.parse().ok() };
        if let Some(limit) = num("x-ratelimit-limit") {
            self.limit = limit;
        }
        if let Some(remaining) = num("x-ratelimit-remaining") {
            self.remaining = remaining;
        }
        if let Some(reset) = num("x-ratelimit-reset") {
            self.reset_at = Utc.timestamp_opt(reset as i64, 0).single();
        }
    }

    /// How long to wait before the next request, if the budget is spent.
    fn cooldown(&self, now: DateTime<Utc>) -> Option<std::time::Duration> {
        if self.limit == 0 || self.remaining > RESERVE {
            return None;
        }
        let reset = self.reset_at?;
        (reset > now).then(|| (reset - now).to_std().unwrap_or_default())
    }
}

/// The outcome of a conditional GET.
#[derive(Debug, Clone)]
pub enum Conditional<T> {
    Modified { value: T, etag: Option<String> },
    NotModified,
}

/// One page of `GET /user/starred`.
#[derive(Debug, Clone)]
pub struct StarredPage {
    pub items: Vec<StarredItem>,
    /// True when the `Link` header advertises another page.
    pub has_next: bool,
}

/// Build the shared reqwest client. Used by both the API client and the
/// device-flow client so they agree on timeouts and the user agent.
pub(crate) fn user_agent_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(SyncError::from)
}

/// Authenticated GitHub client.
#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    token: String,
    base: String,
    rate: Arc<Mutex<RateLimit>>,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the token.
        f.debug_struct("GitHub")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        Ok(Self {
            http: user_agent_client()?,
            token: token.into(),
            base: DEFAULT_BASE.to_string(),
            rate: Arc::new(Mutex::new(RateLimit::default())),
        })
    }

    /// Point the client at another origin. Required by the fixture tests.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn rate_limit(&self) -> RateLimit {
        *self.rate.lock().expect("rate limit mutex")
    }

    /// Wait out the reset window if the budget is exhausted.
    ///
    /// Called before every request. Sleeping here rather than failing keeps a
    /// long first sync correct at the cost of being slow, which is the right
    /// trade for a background job.
    async fn respect_budget(&self) {
        let cooldown = self.rate_limit().cooldown(Utc::now());
        if let Some(wait) = cooldown {
            tracing::info!("rate limit budget spent, sleeping {}s", wait.as_secs());
            tokio::time::sleep(wait).await;
        }
    }

    /// Every request states its own `Accept`.
    ///
    /// This must be a parameter, not a default plus an override: reqwest's
    /// `header` *appends*, so setting it twice sends two media types and
    /// GitHub silently answers with the first one it recognises — which for
    /// the star listing means losing `starred_at`.
    fn request(
        &self,
        method: reqwest::Method,
        url: String,
        accept: &'static str,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, accept)
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(USER_AGENT, HeaderValue::from_static(AGENT))
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<Response> {
        self.respect_budget().await;
        let response = builder.send().await?;
        self.rate
            .lock()
            .expect("rate limit mutex")
            .absorb(response.headers());
        Ok(response)
    }

    /// Turn a non-success status into a typed error, distinguishing the two
    /// cases a caller can actually act on: auth failure and throttling.
    async fn check(&self, response: Response) -> Result<Response> {
        let status = response.status();
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            return Ok(response);
        }
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let reset_at = self.rate_limit().reset_at;
        let body = response.text().await.unwrap_or_default();

        Err(match status {
            StatusCode::UNAUTHORIZED => SyncError::Auth("the GitHub token was rejected".into()),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => SyncError::RateLimited {
                retry_after_secs: retry_after,
                reset_at,
            },
            StatusCode::NOT_FOUND => SyncError::NotFound,
            _ => SyncError::Api {
                status: status.as_u16(),
                message: body.chars().take(300).collect(),
            },
        })
    }

    /// `GET /user`. Identifies the signed-in account for the avatar.
    pub async fn viewer(&self) -> Result<Viewer> {
        let response = self
            .send(self.request(
                reqwest::Method::GET,
                format!("{}/user", self.base),
                JSON_MEDIA_TYPE,
            ))
            .await?;
        Ok(self.check(response).await?.json().await?)
    }

    /// One page of stars, newest first, with `starred_at` attached.
    pub async fn starred_page(&self, page: u32, per_page: u32) -> Result<StarredPage> {
        let url = format!(
            "{}/user/starred?per_page={per_page}&page={page}&sort=created&direction=desc",
            self.base
        );
        let response = self
            .send(self.request(reqwest::Method::GET, url, STAR_MEDIA_TYPE))
            .await?;
        let response = self.check(response).await?;
        let has_next = has_next_page(&response);
        let items: Vec<StarredItem> = response.json().await?;
        Ok(StarredPage { items, has_next })
    }

    /// Conditional `GET /repos/{full_name}`.
    ///
    /// A `304` costs nothing against the rate limit, which is what makes the
    /// 24 h metadata refresh affordable for thousands of repos.
    pub async fn repo_if_modified(
        &self,
        full_name: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<RepoPayload>> {
        let mut builder = self.request(
            reqwest::Method::GET,
            format!("{}/repos/{full_name}", self.base),
            JSON_MEDIA_TYPE,
        );
        if let Some(etag) = etag {
            builder = builder.header(IF_NONE_MATCH, etag);
        }
        let response = self.send(builder).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let response = self.check(response).await?;
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(Conditional::Modified {
            value: response.json().await?,
            etag,
        })
    }

    /// Top contributors, most commits first.
    ///
    /// REST-only: the GraphQL schema has no contributors connection, so this
    /// cannot join the batched language query and is fetched lazily instead.
    pub async fn contributors(&self, full_name: &str, limit: u32) -> Result<Vec<Contributor>> {
        let url = format!(
            "{}/repos/{full_name}/contributors?per_page={limit}",
            self.base
        );
        let response = self
            .send(self.request(reqwest::Method::GET, url, JSON_MEDIA_TYPE))
            .await?;
        match self.check(response).await {
            Ok(response) => {
                let payload: Vec<ContributorPayload> = response.json().await?;
                Ok(payload.into_iter().map(Contributor::from).collect())
            }
            // Empty repositories answer 204; forks of huge repos can 404 here.
            Err(SyncError::NotFound) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    /// The rendered README as Markdown, or `None` when the repo has none.
    pub async fn readme(&self, full_name: &str) -> Result<Option<String>> {
        let url = format!("{}/repos/{full_name}/readme", self.base);
        let response = self
            .send(self.request(reqwest::Method::GET, url, RAW_MEDIA_TYPE))
            .await?;
        match self.check(response).await {
            Ok(response) => Ok(Some(response.text().await?)),
            Err(SyncError::NotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// How many repositories the account has starred, in one GraphQL point.
    ///
    /// Comparing this to the local row count is how an incremental sync decides
    /// whether an unstar happened without paging the whole list.
    pub async fn starred_total(&self) -> Result<i64> {
        #[derive(Deserialize)]
        struct Data {
            viewer: ViewerNode,
        }
        #[derive(Deserialize)]
        struct ViewerNode {
            #[serde(rename = "starredRepositories")]
            starred_repositories: TotalCount,
        }
        #[derive(Deserialize)]
        struct TotalCount {
            #[serde(rename = "totalCount")]
            total_count: i64,
        }

        let data: Data = self
            .graphql("query { viewer { starredRepositories { totalCount } } }")
            .await?;
        Ok(data.viewer.starred_repositories.total_count)
    }

    /// Language byte counts for up to [`GRAPHQL_BATCH`] repositories.
    ///
    /// Returns `(repo id, languages)`. Repositories that have disappeared come
    /// back as `null` and are simply absent from the result.
    pub async fn languages_batch(
        &self,
        full_names: &[String],
    ) -> Result<Vec<(i64, LanguageBytes)>> {
        if full_names.is_empty() {
            return Ok(Vec::new());
        }
        let query = build_languages_query(full_names);
        let data: serde_json::Value = self.graphql(&query).await?;

        let mut out = Vec::with_capacity(full_names.len());
        let Some(object) = data.as_object() else {
            return Ok(out);
        };
        for node in object.values() {
            let Some(id) = node.get("databaseId").and_then(|v| v.as_i64()) else {
                continue;
            };
            let mut languages: LanguageBytes = BTreeMap::new();
            let edges = node
                .get("languages")
                .and_then(|l| l.get("edges"))
                .and_then(|e| e.as_array());
            for edge in edges.into_iter().flatten() {
                let size = edge.get("size").and_then(|s| s.as_i64()).unwrap_or(0);
                let name = edge
                    .get("node")
                    .and_then(|n| n.get("name"))
                    .and_then(|n| n.as_str());
                if let Some(name) = name {
                    languages.insert(name.to_string(), size);
                }
            }
            out.push((id, languages));
        }
        Ok(out)
    }

    /// Issue a GraphQL document and unwrap `data`, surfacing `errors`.
    async fn graphql<T: serde::de::DeserializeOwned>(&self, query: &str) -> Result<T> {
        #[derive(Deserialize)]
        struct Envelope<T> {
            data: Option<T>,
            #[serde(default)]
            errors: Vec<GraphQlError>,
        }
        #[derive(Deserialize)]
        struct GraphQlError {
            message: String,
        }

        let builder = self
            .request(
                reqwest::Method::POST,
                format!("{}/graphql", self.base),
                JSON_MEDIA_TYPE,
            )
            .json(&serde_json::json!({ "query": query }));
        let response = self.check(self.send(builder).await?).await?;
        let envelope: Envelope<T> = response.json().await?;

        if let Some(data) = envelope.data {
            return Ok(data);
        }
        let message = envelope
            .errors
            .into_iter()
            .map(|e| e.message)
            .collect::<Vec<_>>()
            .join("; ");
        Err(SyncError::Api {
            status: 200,
            message: if message.is_empty() {
                "empty GraphQL response".into()
            } else {
                message
            },
        })
    }
}

/// Aliased `repository` fields, one per input, so a batch costs one request.
fn build_languages_query(full_names: &[String]) -> String {
    let mut query = String::from("query {");
    for (ix, full_name) in full_names.iter().enumerate() {
        let Some((owner, name)) = full_name.split_once('/') else {
            continue;
        };
        // GraphQL string literals: only `"` and `\` need escaping here, and
        // GitHub owner/name characters cannot contain either.
        query.push_str(&format!(
            " r{ix}: repository(owner: \"{}\", name: \"{}\") {{ databaseId languages(first: 20, orderBy: {{field: SIZE, direction: DESC}}) {{ edges {{ size node {{ name }} }} }} }}",
            owner.replace(['"', '\\'], ""),
            name.replace(['"', '\\'], ""),
        ));
    }
    query.push_str(" }");
    query
}

/// `Link: <…>; rel="next"` is GitHub's only reliable end-of-list signal: a full
/// page does not mean another page exists.
fn has_next_page(response: &Response) -> bool {
    response
        .headers()
        .get(reqwest::header::LINK)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|link| link.contains("rel=\"next\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_query_aliases_every_repo() {
        let query = build_languages_query(&["a/b".into(), "c/d".into()]);
        assert!(query.contains("r0: repository(owner: \"a\", name: \"b\")"));
        assert!(query.contains("r1: repository(owner: \"c\", name: \"d\")"));
        assert!(query.contains("databaseId"));
    }

    #[test]
    fn language_query_skips_malformed_names() {
        let query = build_languages_query(&["nope".into(), "a/b".into()]);
        assert!(!query.contains("r0:"));
        assert!(query.contains("r1: repository(owner: \"a\", name: \"b\")"));
    }

    #[test]
    fn cooldown_only_applies_once_the_reserve_is_reached() {
        let now = Utc::now();
        let plenty = RateLimit {
            limit: 5000,
            remaining: 4000,
            reset_at: Some(now + chrono::Duration::minutes(30)),
        };
        assert!(plenty.cooldown(now).is_none());

        let spent = RateLimit {
            limit: 5000,
            remaining: 3,
            reset_at: Some(now + chrono::Duration::minutes(10)),
        };
        let wait = spent.cooldown(now).expect("must wait");
        assert!(wait.as_secs() > 500 && wait.as_secs() <= 600);

        // A past reset is not a reason to sleep.
        let stale = RateLimit {
            limit: 5000,
            remaining: 0,
            reset_at: Some(now - chrono::Duration::minutes(1)),
        };
        assert!(stale.cooldown(now).is_none());
    }

    #[test]
    fn an_unseen_rate_limit_never_blocks() {
        assert!(RateLimit::default().cooldown(Utc::now()).is_none());
    }
}
