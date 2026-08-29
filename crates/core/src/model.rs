//! The objects Starlet searches over.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bytes of source per language, as GitHub reports them.
pub type LanguageBytes = BTreeMap<String, i64>;

/// A starred repository.
///
/// `contributors` is loaded lazily: the sync engine leaves it empty and the
/// detail sheet fills it on first open. Everything else is written by the full
/// or incremental sync and is always present after the first successful run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Repo {
    /// GitHub's numeric repository id. Stable across renames.
    pub id: i64,
    pub node_id: String,
    /// `owner/name`.
    pub full_name: String,
    pub name: String,
    pub owner: String,
    pub html_url: String,
    pub description: Option<String>,
    pub stargazers: i64,
    /// GitHub's `pushed_at`.
    pub last_commit_at: Option<DateTime<Utc>>,
    pub primary_language: Option<String>,
    pub languages: LanguageBytes,
    pub contributors: Vec<Contributor>,
    pub starred_at: Option<DateTime<Utc>>,
    pub archived: bool,
    pub fork: bool,
    pub topics: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub synced_at: Option<DateTime<Utc>>,
    /// Tags attached to this repo, ordered as the store returned them.
    pub tags: Vec<RepoTag>,
    /// Names of the groups this repo belongs to.
    pub groups: Vec<String>,
}

impl Repo {
    /// Total bytes across every detected language. Used for the language bar.
    pub fn language_total(&self) -> i64 {
        self.languages.values().copied().sum()
    }

    /// Languages sorted by size, largest first.
    pub fn languages_by_size(&self) -> Vec<(&str, i64)> {
        let mut v: Vec<(&str, i64)> = self
            .languages
            .iter()
            .map(|(k, n)| (k.as_str(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// True when the repo carries `name` as a tag, regardless of source.
    pub fn has_tag(&self, name: &str) -> bool {
        self.tags.iter().any(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn in_group(&self, name: &str) -> bool {
        self.groups.iter().any(|g| g.eq_ignore_ascii_case(name))
    }
}

/// Where a tag came from. `User` tags are authoritative and are never
/// overwritten by an AI run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagSource {
    /// A GitHub topic, mirrored into the tag table.
    Github,
    /// Produced by a BYOK AI run.
    Ai,
    /// Created or promoted by the user.
    User,
}

impl TagSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TagSource::Github => "github",
            TagSource::Ai => "ai",
            TagSource::User => "user",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(TagSource::Github),
            "ai" => Some(TagSource::Ai),
            "user" => Some(TagSource::User),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoTag {
    pub name: String,
    pub source: TagSource,
    /// Model confidence for AI tags; `1.0` for user and GitHub tags.
    pub confidence: f32,
}

impl Eq for RepoTag {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contributor {
    pub login: String,
    pub avatar_url: String,
    pub contributions: i64,
}

/// A named cluster of repos produced by the AI grouping pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub summary: String,
    pub source: TagSource,
    /// `full_name`s of the member repos.
    pub members: Vec<String>,
}

/// The trimmed projection sent to an AI provider.
///
/// Deliberately small: sending descriptions and topics is enough for tagging
/// and keeps the token cost predictable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSummary {
    pub full_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub topics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_language: Option<String>,
}

impl From<&Repo> for RepoSummary {
    fn from(r: &Repo) -> Self {
        Self {
            full_name: r.full_name.clone(),
            description: r.description.clone(),
            topics: r.topics.clone(),
            primary_language: r.primary_language.clone(),
        }
    }
}
