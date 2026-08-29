//! GitHub's JSON, and the conversion into Starlet's domain model.
//!
//! These types exist only at the network boundary. Nothing above `sync` should
//! see a `stargazers_count` or a `pushed_at`.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use starlet_core::model::{Contributor, Repo};

/// One entry from `GET /user/starred` requested with the
/// `application/vnd.github.star+json` media type, which wraps the repository
/// so the star timestamp can travel with it.
#[derive(Debug, Clone, Deserialize)]
pub struct StarredItem {
    pub starred_at: Option<String>,
    pub repo: RepoPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoPayload {
    pub id: i64,
    pub node_id: String,
    pub full_name: String,
    pub name: String,
    pub owner: OwnerPayload,
    pub html_url: String,
    pub description: Option<String>,
    #[serde(default)]
    pub stargazers_count: i64,
    pub pushed_at: Option<String>,
    /// GitHub's primary language for the repo.
    pub language: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub topics: Vec<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OwnerPayload {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContributorPayload {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub contributions: i64,
}

/// `GET /user`, used for the signed-in avatar.
#[derive(Debug, Clone, Deserialize)]
pub struct Viewer {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
    pub name: Option<String>,
}

fn parse_ts(raw: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw?)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

impl RepoPayload {
    /// Build a domain `Repo`.
    ///
    /// `languages` and `contributors` are left empty: the star listing does not
    /// carry them, and the store's upsert preserves whatever it already has
    /// rather than letting this emptiness overwrite a previous fetch.
    pub fn into_repo(self, starred_at: Option<&str>, synced_at: DateTime<Utc>) -> Repo {
        Repo {
            id: self.id,
            node_id: self.node_id,
            full_name: self.full_name,
            name: self.name,
            owner: self.owner.login,
            html_url: self.html_url,
            description: self.description,
            stargazers: self.stargazers_count,
            last_commit_at: parse_ts(self.pushed_at.as_deref()),
            primary_language: self.language,
            languages: Default::default(),
            contributors: Vec::new(),
            starred_at: parse_ts(starred_at),
            archived: self.archived,
            fork: self.fork,
            topics: self.topics,
            updated_at: parse_ts(self.updated_at.as_deref()),
            synced_at: Some(synced_at),
            tags: Vec::new(),
            groups: Vec::new(),
        }
    }
}

impl From<ContributorPayload> for Contributor {
    fn from(p: ContributorPayload) -> Self {
        Contributor {
            login: p.login,
            avatar_url: p.avatar_url,
            contributions: p.contributions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but otherwise verbatim `star+json` payload.
    const STARRED: &str = r#"[{
      "starred_at": "2024-05-02T09:14:11Z",
      "repo": {
        "id": 199902895,
        "node_id": "MDEwOlJlcG9zaXRvcnkxOTk5MDI4OTU=",
        "name": "helix",
        "full_name": "helix-editor/helix",
        "owner": { "login": "helix-editor", "id": 53096353 },
        "html_url": "https://github.com/helix-editor/helix",
        "description": "A post-modern modal text editor.",
        "fork": false,
        "created_at": "2019-08-01T00:00:00Z",
        "updated_at": "2026-02-19T12:00:00Z",
        "pushed_at": "2026-02-18T22:31:05Z",
        "stargazers_count": 39472,
        "language": "Rust",
        "archived": false,
        "topics": ["editor", "rust", "terminal"]
      }
    }]"#;

    #[test]
    fn star_json_maps_onto_the_domain_model() {
        let items: Vec<StarredItem> = serde_json::from_str(STARRED).expect("parse");
        let now = Utc::now();
        let repo = items[0]
            .repo
            .clone()
            .into_repo(items[0].starred_at.as_deref(), now);

        assert_eq!(repo.id, 199_902_895);
        assert_eq!(repo.full_name, "helix-editor/helix");
        assert_eq!(repo.owner, "helix-editor");
        assert_eq!(repo.stargazers, 39_472);
        assert_eq!(repo.primary_language.as_deref(), Some("Rust"));
        assert_eq!(repo.topics, ["editor", "rust", "terminal"]);
        assert_eq!(
            repo.last_commit_at.map(|t| t.to_rfc3339()),
            Some("2026-02-18T22:31:05+00:00".to_string())
        );
        assert_eq!(
            repo.starred_at.map(|t| t.to_rfc3339()),
            Some("2024-05-02T09:14:11+00:00".to_string())
        );
        assert!(
            repo.languages.is_empty(),
            "the star listing carries no language bytes"
        );
    }

    #[test]
    fn absent_optional_fields_do_not_fail_the_row() {
        let json = r#"{
          "id": 1, "node_id": "n", "name": "x", "full_name": "o/x",
          "owner": {"login": "o"}, "html_url": "https://example.invalid",
          "description": null, "pushed_at": null, "language": null, "updated_at": null
        }"#;
        let payload: RepoPayload = serde_json::from_str(json).expect("parse");
        let repo = payload.into_repo(None, Utc::now());
        assert_eq!(repo.stargazers, 0);
        assert!(!repo.archived);
        assert!(repo.topics.is_empty());
        assert!(repo.starred_at.is_none());
    }
}
