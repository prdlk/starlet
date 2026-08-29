//! Local Ollama backend.
//!
//! The privacy-preserving option: nothing leaves the machine, and the run is
//! free, so the UI can offer it without a spend confirmation.

use async_trait::async_trait;
use serde_json::json;
use starlet_core::{Group, RepoSummary};

use crate::client::{ApiKey, body_or_status, envelope_field, parse_with_one_retry};
use crate::cost::{self, Price};
use crate::parse;
use crate::prompt;
use crate::provider::{AiProvider, CostEstimate, RepoTags, RepoWithTags, Result};

pub const ID: &str = "ollama";
pub const DEFAULT_MODEL: &str = "llama3.1";
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Talks to `/api/chat` with `format: json` and streaming off.
#[derive(Debug, Clone)]
pub struct Ollama {
    /// Normally empty. A non-empty value is sent as a bearer token, which is
    /// what a reverse proxy in front of a remote Ollama expects; a local daemon
    /// ignores the header.
    key: ApiKey,
    model: String,
    base_url: String,
    http: reqwest::Client,
}

impl Ollama {
    /// `api_key` may be empty: local Ollama has no auth. An empty `model`
    /// selects [`DEFAULT_MODEL`].
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
        let body = json!({
            "model": self.model,
            "stream": false,
            "format": "json",
            "options": { "temperature": 0 },
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": prompt::with_retry(user, retry) },
            ],
        });

        let mut request = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&body);
        if !self.key.is_empty() {
            request = request.bearer_auth(self.key.as_str());
        }

        let raw = body_or_status(request.send().await?, &self.key).await?;
        let envelope: serde_json::Value = serde_json::from_str(&raw)?;
        envelope_field(&envelope, "/message/content", ID)
    }
}

#[async_trait]
impl AiProvider for Ollama {
    fn id(&self) -> &'static str {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// Token counts are still reported so the UI can show the size of the job;
    /// only the price is zero, because the user's own GPU is doing the work.
    fn estimate(&self, repos: usize) -> CostEstimate {
        cost::estimate(repos, Price::FREE)
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
