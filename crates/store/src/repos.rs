//! Reading and writing the `repos` table.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::Row;
use starlet_core::model::{Contributor, LanguageBytes, Repo, RepoTag, TagSource};

use crate::time::{format_ts, parse_ts};
use crate::{Result, Store, StoreError};

/// The `repos` row exactly as SQLite stores it.
#[derive(Debug, sqlx::FromRow)]
struct RepoRow {
    id: i64,
    node_id: String,
    full_name: String,
    name: String,
    owner: String,
    html_url: String,
    description: Option<String>,
    stargazers: i64,
    last_commit_at: Option<String>,
    primary_language: Option<String>,
    languages_json: String,
    contributors_json: Option<String>,
    starred_at: Option<String>,
    archived: bool,
    fork: bool,
    topics_json: String,
    updated_at: Option<String>,
    synced_at: Option<String>,
}

const REPO_COLUMNS: &str = "id, node_id, full_name, name, owner, html_url, description, \
     stargazers, last_commit_at, primary_language, languages_json, contributors_json, \
     starred_at, archived, fork, topics_json, updated_at, synced_at";

impl RepoRow {
    /// `with_contributors` is false on the list path: 5 000 contributor arrays
    /// are ~10 MB of JSON nobody is looking at until a sheet opens.
    fn into_repo(self, with_contributors: bool) -> Result<Repo> {
        let languages: LanguageBytes =
            serde_json::from_str(&self.languages_json).map_err(|source| StoreError::Json {
                column: "languages_json",
                source,
            })?;
        let topics: Vec<String> =
            serde_json::from_str(&self.topics_json).map_err(|source| StoreError::Json {
                column: "topics_json",
                source,
            })?;
        let contributors = match (with_contributors, self.contributors_json.as_deref()) {
            (true, Some(raw)) => serde_json::from_str(raw).map_err(|source| StoreError::Json {
                column: "contributors_json",
                source,
            })?,
            _ => Vec::new(),
        };

        Ok(Repo {
            id: self.id,
            node_id: self.node_id,
            full_name: self.full_name,
            name: self.name,
            owner: self.owner,
            html_url: self.html_url,
            description: self.description,
            stargazers: self.stargazers,
            last_commit_at: parse_ts(self.last_commit_at.as_deref()),
            primary_language: self.primary_language,
            languages,
            contributors,
            starred_at: parse_ts(self.starred_at.as_deref()),
            archived: self.archived,
            fork: self.fork,
            topics,
            updated_at: parse_ts(self.updated_at.as_deref()),
            synced_at: parse_ts(self.synced_at.as_deref()),
            tags: Vec::new(),
            groups: Vec::new(),
        })
    }
}

impl Store {
    /// Every repo, with tags and groups attached but without contributors.
    ///
    /// This is what the search index is built from, so it is one pass over
    /// three tables rather than a per-repo query.
    pub async fn load_repos(&self) -> Result<Vec<Repo>> {
        let rows: Vec<RepoRow> = sqlx::query_as(&format!("SELECT {REPO_COLUMNS} FROM repos"))
            .fetch_all(self.pool())
            .await?;

        let mut repos: Vec<Repo> = rows
            .into_iter()
            .map(|r| r.into_repo(false))
            .collect::<Result<_>>()?;

        let mut tags = self.all_repo_tags().await?;
        let mut groups = self.all_repo_groups().await?;
        for repo in &mut repos {
            repo.tags = tags.remove(&repo.id).unwrap_or_default();
            repo.groups = groups.remove(&repo.id).unwrap_or_default();
        }
        Ok(repos)
    }

    /// One repo with everything, including cached contributors.
    pub async fn load_repo(&self, id: i64) -> Result<Option<Repo>> {
        let row: Option<RepoRow> =
            sqlx::query_as(&format!("SELECT {REPO_COLUMNS} FROM repos WHERE id = ?"))
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        let Some(row) = row else { return Ok(None) };
        let mut repo = row.into_repo(true)?;
        repo.tags = self.repo_tags(id).await?;
        repo.groups = self.repo_groups(id).await?;
        Ok(Some(repo))
    }

    pub async fn repo_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM repos")
            .fetch_one(self.pool())
            .await?;
        Ok(n)
    }

    /// Insert or refresh a batch, and mirror GitHub topics into the tag table.
    ///
    /// Columns the caller cannot know about — contributors, README cache, ETag
    /// — are preserved when the incoming record leaves them empty.
    pub async fn upsert_repos(&self, repos: &[Repo]) -> Result<u64> {
        if repos.is_empty() {
            return Ok(0);
        }
        let now = format_ts(&Utc::now());
        let mut tx = self.pool().begin().await?;
        let mut written = 0u64;

        for repo in repos {
            let languages = serde_json::to_string(&repo.languages).unwrap_or_else(|_| "{}".into());
            let topics = serde_json::to_string(&repo.topics).unwrap_or_else(|_| "[]".into());
            let contributors = if repo.contributors.is_empty() {
                None
            } else {
                serde_json::to_string(&repo.contributors).ok()
            };

            let result = sqlx::query(
                r#"
                INSERT INTO repos (
                    id, node_id, full_name, name, owner, html_url, description, stargazers,
                    last_commit_at, primary_language, languages_json, contributors_json,
                    starred_at, archived, fork, topics_json, updated_at, synced_at
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
                ON CONFLICT (id) DO UPDATE SET
                    node_id           = excluded.node_id,
                    full_name         = excluded.full_name,
                    name              = excluded.name,
                    owner             = excluded.owner,
                    html_url          = excluded.html_url,
                    description       = excluded.description,
                    stargazers        = excluded.stargazers,
                    last_commit_at    = excluded.last_commit_at,
                    primary_language  = excluded.primary_language,
                    languages_json    = CASE WHEN excluded.languages_json = '{}'
                                             THEN repos.languages_json
                                             ELSE excluded.languages_json END,
                    contributors_json = coalesce(excluded.contributors_json, repos.contributors_json),
                    starred_at        = coalesce(excluded.starred_at, repos.starred_at),
                    archived          = excluded.archived,
                    fork              = excluded.fork,
                    topics_json       = excluded.topics_json,
                    updated_at        = excluded.updated_at,
                    synced_at         = excluded.synced_at
                "#,
            )
            .bind(repo.id)
            .bind(&repo.node_id)
            .bind(&repo.full_name)
            .bind(&repo.name)
            .bind(&repo.owner)
            .bind(&repo.html_url)
            .bind(&repo.description)
            .bind(repo.stargazers)
            .bind(repo.last_commit_at.as_ref().map(format_ts))
            .bind(&repo.primary_language)
            .bind(languages)
            .bind(contributors)
            .bind(repo.starred_at.as_ref().map(format_ts))
            .bind(repo.archived)
            .bind(repo.fork)
            .bind(topics)
            .bind(repo.updated_at.as_ref().map(format_ts))
            .bind(repo.synced_at.as_ref().map(format_ts).unwrap_or_else(|| now.clone()))
            .execute(&mut *tx)
            .await?;
            written += result.rows_affected();

            crate::taxonomy::sync_github_topics(&mut tx, repo.id, &repo.topics).await?;
        }

        tx.commit().await?;
        Ok(written)
    }

    /// Remove repos the user unstarred. Cascades to tags and groups.
    pub async fn delete_repos(&self, ids: &[i64]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool().begin().await?;
        let mut removed = 0;
        for id in ids {
            removed += sqlx::query("DELETE FROM repos WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        tx.commit().await?;
        Ok(removed)
    }

    /// Every locally known repo id. The unstar diff is computed against this.
    pub async fn known_ids(&self) -> Result<HashSet<i64>> {
        let rows = sqlx::query("SELECT id FROM repos")
            .fetch_all(self.pool())
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<i64, _>(0)?))
            .collect()
    }

    /// Repos whose metadata has not been refreshed since `before`.
    pub async fn stale_ids(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<(i64, String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT id, full_name, etag FROM repos \
             WHERE synced_at IS NULL OR synced_at < ? ORDER BY synced_at ASC",
        )
        .bind(format_ts(&before))
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect()
    }

    /// Record a conditional-request validator for the repo endpoint.
    pub async fn set_etag(&self, id: i64, etag: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE repos SET etag = ? WHERE id = ?")
            .bind(etag)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Mark a repo as freshly checked without changing its content. Used when
    /// GitHub answers `304 Not Modified`.
    pub async fn touch_synced_at(&self, id: i64, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE repos SET synced_at = ? WHERE id = ?")
            .bind(format_ts(&at))
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn set_languages(&self, id: i64, languages: &LanguageBytes) -> Result<()> {
        sqlx::query("UPDATE repos SET languages_json = ? WHERE id = ?")
            .bind(serde_json::to_string(languages).unwrap_or_else(|_| "{}".into()))
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Repos with no language breakdown yet, oldest star first.
    pub async fn ids_without_languages(&self, limit: i64) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query(
            "SELECT id, full_name FROM repos WHERE languages_json = '{}' \
             ORDER BY stargazers DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect()
    }

    pub async fn set_contributors(&self, id: i64, contributors: &[Contributor]) -> Result<()> {
        sqlx::query("UPDATE repos SET contributors_json = ? WHERE id = ?")
            .bind(serde_json::to_string(contributors).unwrap_or_else(|_| "[]".into()))
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The cached README, if it was fetched after `not_before`.
    pub async fn readme(&self, id: i64, not_before: DateTime<Utc>) -> Result<Option<String>> {
        let row = sqlx::query("SELECT readme_md, readme_fetched_at FROM repos WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        let Some(row) = row else { return Ok(None) };
        let md: Option<String> = row.try_get(0)?;
        let fetched: Option<String> = row.try_get(1)?;
        match (md, parse_ts(fetched.as_deref())) {
            (Some(md), Some(at)) if at >= not_before => Ok(Some(md)),
            _ => Ok(None),
        }
    }

    pub async fn set_readme(&self, id: i64, markdown: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE repos SET readme_md = ?, readme_fetched_at = ? WHERE id = ?")
            .bind(markdown)
            .bind(format_ts(&at))
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn all_repo_tags(&self) -> Result<HashMap<i64, Vec<RepoTag>>> {
        let rows = sqlx::query(
            "SELECT rt.repo_id, t.name, t.source, rt.confidence \
             FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id \
             ORDER BY t.source DESC, rt.confidence DESC, t.name ASC",
        )
        .fetch_all(self.pool())
        .await?;

        let mut out: HashMap<i64, Vec<RepoTag>> = HashMap::new();
        for row in rows {
            let repo_id: i64 = row.try_get(0)?;
            out.entry(repo_id).or_default().push(RepoTag {
                name: row.try_get(1)?,
                source: TagSource::parse(row.try_get::<String, _>(2)?.as_str())
                    .unwrap_or(TagSource::Github),
                confidence: row.try_get::<f64, _>(3)? as f32,
            });
        }
        Ok(out)
    }

    async fn all_repo_groups(&self) -> Result<HashMap<i64, Vec<String>>> {
        let rows = sqlx::query(
            "SELECT rg.repo_id, g.name FROM repo_groups rg \
             JOIN \"groups\" g ON g.id = rg.group_id ORDER BY g.name",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out: HashMap<i64, Vec<String>> = HashMap::new();
        for row in rows {
            out.entry(row.try_get(0)?)
                .or_default()
                .push(row.try_get(1)?);
        }
        Ok(out)
    }
}
