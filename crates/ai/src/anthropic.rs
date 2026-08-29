//! Anthropic messages backend.

use async_trait::async_trait;
use serde_json::json;
use starlet_core::{Group, RepoSummary};

use crate::client::{ApiKey, body_or_status, parse_with_one_retry};
use crate::cost;
use crate::parse;
use crate::prompt;
use crate::provider::{AiError, AiProvider, CostEstimate, RepoTags, RepoWithTags, Result};

pub const ID: &str = "anthropic";
pub const DEFAULT_MODEL: &str = "claude-3-5-haiku-latest";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The only API version this client speaks. Pinned rather than tracked: a new
/// version can change the response shape, and that is a code change.
const API_VERSION: &str = "2023-06-01";

/// Headroom for a full batch of 25 repos at six tags each, or a whole library's
/// worth of groups. Anthropic requires the field, and a truncated reply is a
/// parse failure that burns the retry, so this is set generously.
const MAX_TOKENS: u32 = 8192;

/// Talks to `/v1/messages`.
///
/// Unlike OpenAI, the system prompt is a top-level field rather than a message,
/// which is why the two providers cannot share a request builder.
#[derive(Debug, Clone)]
pub struct Anthropic {
    key: ApiKey,
    model: String,
    /// Origin only, no trailing slash; the seam mock servers and proxies use.
    base_url: String,
    http: reqwest::Client,
}

impl Anthropic {
    /// An empty `model` selects [`DEFAULT_MODEL`].
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            key: ApiKey::new(api_key),
            model: if model.trim().is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                model
            },
            base_url: DEFAULT_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
        self
    }

    async fn chat(&self, system: &str, user: &str, retry: bool) -> Result<String> {
        let key = self.key.require()?;
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "temperature": 0,
            "system": system,
            "messages": [
                { "role": "user", "content": prompt::with_retry(user, retry) },
            ],
        });

        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await?;

        let raw = body_or_status(response, &self.key).await?;
        let envelope: serde_json::Value = serde_json::from_str(&raw)?;
        text_blocks(&envelope)
    }
}

/// Concatenate every `text` block in the reply.
///
/// A single block is the norm, but the API is free to split one answer across
/// several, and joining is the only way to get valid JSON back out of that.
fn text_blocks(envelope: &serde_json::Value) -> Result<String> {
    let blocks = envelope
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            AiError::MalformedResponse("anthropic response has no `content` array".into())
        })?;

    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
            continue;
        }
        if let Some(chunk) = block.get("text").and_then(serde_json::Value::as_str) {
            text.push_str(chunk);
        }
    }
    if text.is_empty() {
        return Err(AiError::MalformedResponse(
            "anthropic response contained no text blocks".into(),
        ));
    }
    Ok(text)
}

#[async_trait]
impl AiProvider for Anthropic {
    fn id(&self) -> &'static str {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn estimate(&self, repos: usize) -> CostEstimate {
        cost::estimate(repos, cost::anthropic_price(&self.model))
    }

    async fn tag(&self, batch: &[RepoSummary]) -> Result<Vec<RepoTags>> {
        let user = prompt::tag_user(batch)?;
        parse_with_one_retry(
            ID,
            |retry| self.chat(prompt::TAG_SYSTEM, &user, retry),
            parse::parse_tags,
        )
        .await
    }

    async fn group(&self, repos: &[RepoWithTags]) -> Result<Vec<Group>> {
        let user = prompt::group_user(repos)?;
        parse_with_one_retry(
            ID,
            |retry| self.chat(prompt::GROUP_SYSTEM, &user, retry),
            parse::parse_groups,
        )
        .await
    }
}
