//! The prompts, in one place.
//!
//! All three providers send byte-identical instructions; only the transport
//! differs. Keeping the text here means a wording change cannot drift between
//! backends, and [`crate::cost`] can size the per-batch overhead from the real
//! string instead of a guess.

use starlet_core::RepoSummary;

use crate::provider::{RepoWithTags, Result};

/// System prompt for the tagging pass.
///
/// The output contract is restated as a literal skeleton because models follow
/// a shown shape far more reliably than a described one.
pub const TAG_SYSTEM: &str = r#"You label GitHub repositories for a local search tool.

For every repository in the input array, produce between 3 and 6 topical tags.

Rules:
- Tags are lowercase kebab-case, one to three words: "static-site-generator", "rust", "cli".
- A tag says what the project is or what it is for. Never restate the owner or
  the repository name, and never tag popularity, licence, or activity.
- "confidence" is your certainty for that one tag, a number between 0 and 1.
- Echo "full_name" back exactly as it was given. Never invent a repository.

Reply with one JSON object and nothing else. No prose, no explanation, no markdown code fence.

{"repos":[{"full_name":"owner/name","tags":[{"name":"tag","confidence":0.0}]}]}"#;

/// System prompt for the grouping pass.
pub const GROUP_SYSTEM: &str = r#"You organise a personal library of starred GitHub repositories into browsable groups.

You are given every repository with its description and its tags. Cluster them
into coherent groups a human would use as a sidebar.

Rules:
- Between 4 and 20 groups. Prefer fewer, larger, meaningful groups over many tiny ones.
- "name" is a short human title in title case: "Rust CLI Tooling".
- "summary" is one plain sentence saying what the group collects.
- "members" holds "full_name" values copied exactly from the input.
- A repository may appear in more than one group. Leave a repository out rather
  than forcing it into a group it does not belong in.

Reply with one JSON object and nothing else. No prose, no explanation, no markdown code fence.

{"groups":[{"name":"Group Name","summary":"one line","members":["owner/name"]}]}"#;

/// Appended to the user message on the single retry.
///
/// Phrased as a correction rather than a repeat instruction: a model that has
/// already ignored the format once responds better to being told it failed.
pub const RETRY_SUFFIX: &str = "\n\nYour previous reply could not be parsed. \
Reply with the JSON object only: no prose, no explanation, no markdown code fence.";

/// The tagging user message: the batch, serialised as-is.
///
/// `RepoSummary` skips empty fields, so a repo with no description or topics
/// costs almost nothing.
pub fn tag_user(batch: &[RepoSummary]) -> Result<String> {
    Ok(serde_json::to_string(batch)?)
}

/// The grouping user message.
pub fn group_user(repos: &[RepoWithTags]) -> Result<String> {
    Ok(serde_json::to_string(repos)?)
}

/// Appends [`RETRY_SUFFIX`] when `retry` is set.
pub(crate) fn with_retry(user: &str, retry: bool) -> String {
    if retry {
        format!("{user}{RETRY_SUFFIX}")
    } else {
        user.to_string()
    }
}
