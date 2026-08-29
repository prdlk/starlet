//! The provider seam: one trait, the values that cross it, and the error type.
//!
//! Everything the UI needs to drive an analysis run is declared here so that
//! adding a fourth backend never widens the public surface.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use starlet_core::{Group, RepoSummary, RepoTag};

/// Fallible AI work.
///
/// The error parameter is defaulted so callers write `Result<T>` while
/// `?` against a foreign error type still type-checks.
pub type Result<T, E = AiError> = std::result::Result<T, E>;

/// Everything that can go wrong talking to a model.
///
/// No variant ever carries an API key: [`AiError::Status`] bodies are redacted
/// at the call site before the variant is built, and the key never appears in a
/// URL for any supported provider.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// Connection, TLS, or timeout failure. Retrying is the caller's decision.
    #[error("http transport failure: {0}")]
    Http(#[from] reqwest::Error),

    /// The provider answered, but not with success. `message` is the response
    /// body, truncated and redacted.
    #[error("provider returned http {code}: {message}")]
    Status { code: u16, message: String },

    /// The response envelope was not the JSON this provider's API documents.
    #[error("could not decode the provider response: {0}")]
    Json(#[from] serde_json::Error),

    /// The envelope decoded but the model's own output did not survive
    /// [`crate::parse`]. Carries a human-readable reason, never model text
    /// verbatim, so it is safe to surface in the UI.
    #[error("model returned malformed output: {0}")]
    MalformedResponse(String),

    /// No key is configured for a provider that requires one. Checked before
    /// any request is built so a misconfigured provider costs nothing.
    #[error("no api key is configured for this provider")]
    MissingKey,

    /// The caller flipped the cancel flag, or the event receiver went away.
    #[error("analysis was cancelled")]
    Cancelled,
}

/// Tags the model produced for one repository.
///
/// Always `TagSource::Ai`; the store merges these under user tags, which win.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoTags {
    pub full_name: String,
    pub tags: Vec<RepoTag>,
}

/// The grouping pass input: a repo plus its *flattened* tag names from every
/// source, because the grouper does not care where a tag came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoWithTags {
    pub full_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub tags: Vec<String>,
}

/// A pre-flight price quote shown before the user spends money.
///
/// Deliberately an over-estimate: see [`crate::cost`] for the token model. A
/// run that comes in cheaper than quoted is a pleasant surprise; the reverse is
/// a support ticket.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct CostEstimate {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
}

/// A model backend that can tag and group repositories.
///
/// Implementations are cheap to clone and hold no per-run state, so the UI can
/// build one at settings-load time and keep it for the process lifetime.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Stable identifier used as the settings key and keychain account suffix.
    ///
    /// Changing one of these orphans a stored key, so they are frozen.
    fn id(&self) -> &'static str;

    /// The model this instance will call.
    fn model(&self) -> &str;

    /// Predicted USD cost for `repos` repositories. Local providers return 0.
    fn estimate(&self, repos: usize) -> CostEstimate;

    /// Tag one batch. Callers chunk with [`crate::analysis::BATCH_SIZE`].
    async fn tag(&self, batch: &[RepoSummary]) -> Result<Vec<RepoTags>>;

    /// Cluster the whole library in a single pass; groups only make sense with
    /// global visibility, so this is deliberately not batched.
    async fn group(&self, repos: &[RepoWithTags]) -> Result<Vec<Group>>;
}
