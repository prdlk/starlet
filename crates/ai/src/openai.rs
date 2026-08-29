//! OpenAI chat-completions backend.

use async_trait::async_trait;
use serde_json::json;
use starlet_core::{Group, RepoSummary};

use crate::client::{ApiKey, body_or_status, envelope_field, parse_with_one_retry};
use crate::cost;
use crate::parse;
use crate::prompt;
use crate::provider::{AiProvider, CostEstimate, RepoTags, RepoWithTags, Result};

pub const ID: &str = "openai";
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Talks to `/v1/chat/completions`.
///
/// `response_format: json_object` plus `temperature: 0` gets us parseable
/// output almost always; [`crate::parse`] and the single retry cover the rest.
#[derive(Debug, Clone)]
pub struct OpenAi {
    key: ApiKey,
    model: String,
    /// Origin only, no trailing slash. Exists so tests can point the provider
    /// at a local mock server, and so users can route through a proxy.
    base_url: String,
    http: reqwest::Client,
}

impl OpenAi {
    /// An empty `model` selects [`DEFAULT_MODEL`], so a settings file that has
    /// never been touched still works.
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

    /// One round trip returning the assistant's raw text.
    async fn chat(&self, system: &str, user: &str, retry: bool) -> Result<String> {
        let key = self.key.require()?;
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": prompt::with_retry(user, retry) },
            ],
        });

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await?;

        let raw = body_or_status(response, &self.key).await?;
        let envelope: serde_json::Value = serde_json::from_str(&raw)?;
        envelope_field(&envelope, "/choices/0/message/content", ID)
    }
}

#[async_trait]
impl AiProvider for OpenAi {
    fn id(&self) -> &'static str {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn estimate(&self, repos: usize) -> CostEstimate {
        cost::estimate(repos, cost::openai_price(&self.model))
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
