//! Optional bring-your-own-key analysis for Starlet: tag repositories and
//! cluster them into groups.
//!
//! Nothing here runs unless the user configures a provider and starts a run.
//! The crate is I/O at the edges only — [`parse`] and [`cost`] are pure, which
//! is where the interesting behaviour lives and where the tests are.
//!
//! ```no_run
//! # use std::collections::HashMap;
//! # use std::sync::Arc;
//! # use std::sync::atomic::AtomicBool;
//! # async fn run(repos: Vec<starlet_core::RepoSummary>) -> starlet_ai::Result<()> {
//! let provider = starlet_ai::OpenAi::new("sk-…", "");
//! let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
//! starlet_ai::analyze(&provider, &repos, &HashMap::new(), tx, Arc::new(AtomicBool::new(false))).await
//! # }
//! ```
//!
//! ## Secrets
//!
//! API keys are held in a wrapper whose `Debug` prints `ApiKey(redacted)`, are
//! never placed in a URL, and are scrubbed out of error bodies before an
//! [`AiError::Status`] is built. Nothing in this crate logs a key.

pub mod analysis;
pub mod anthropic;
pub mod cost;
pub mod openai;
pub mod ollama;
pub mod parse;
pub mod prompt;
pub mod provider;

mod client;

pub use analysis::{AiEvent, BATCH_SIZE, analyze};
pub use anthropic::Anthropic;
pub use cost::Price;
pub use openai::OpenAi;
pub use ollama::Ollama;
pub use parse::{parse_groups, parse_tags};
pub use provider::{AiError, AiProvider, CostEstimate, RepoTags, RepoWithTags, Result};

/// Build the provider named by a settings value.
///
/// The ids are the same strings [`AiProvider::id`] returns and the same keychain
/// account suffixes, so a round trip through settings cannot mismatch.
pub fn provider_for(
    id: &str,
    api_key: impl Into<String>,
    model: impl Into<String>,
) -> Option<Box<dyn AiProvider>> {
    match id {
        openai::ID => Some(Box::new(OpenAi::new(api_key, model))),
        anthropic::ID => Some(Box::new(Anthropic::new(api_key, model))),
        ollama::ID => Some(Box::new(Ollama::new(api_key, model))),
        _ => None,
    }
}

/// Every provider id, in the order the settings UI should list them.
pub const PROVIDER_IDS: [&str; 3] = [openai::ID, anthropic::ID, ollama::ID];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_id_builds_a_provider_that_agrees_about_its_own_id() {
        for id in PROVIDER_IDS {
            let provider = provider_for(id, "k", "").expect("listed id must build");
            assert_eq!(provider.id(), id);
            assert!(!provider.model().is_empty(), "{id} must have a default model");
        }
        assert!(provider_for("gemini", "k", "").is_none());
    }

    #[test]
    fn local_runs_are_free_and_hosted_runs_are_not() {
        assert_eq!(Ollama::new("", "").estimate(500).usd, 0.0);
        assert!(OpenAi::new("k", "").estimate(500).usd > 0.0);
    }
}
