//! Tags and groups.
//!
//! The three tag sources are kept strictly apart. GitHub topics are mirrored
//! on every sync, AI tags are replaced wholesale by each analysis run, and user
//! tags are only ever changed by the user. That separation is what makes
//! "never overwrite a user tag with an AI tag" a property of the schema rather
//! than a rule someone has to remember.

use sqlx::{Row, Sqlite, Transaction};
use starlet_core::model::{Group, RepoTag, TagSource};

use crate::{Result, Store};

/// A tag with its usage count, for the sidebar facet list.
#[derive(Debug, Clone, PartialEq)]
pub struct TagFacet {
    pub name: String,
    pub source: TagSource,
    pub count: i64,
}

/// A group with its member count.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupFacet {
    pub name: String,
    pub summary: String,
    pub count: i64,
}

/// Insert (or find) a tag and return its id.
async fn tag_id(tx: &mut Transaction<'_, Sqlite>, name: &str, source: TagSource) -> Result<i64> {
    let row = sqlx::query(
        "INSERT INTO tags (name, source) VALUES (?, ?) \
         ON CONFLICT (name, source) DO UPDATE SET name = excluded.name RETURNING id",
    )
    .bind(name)
    .bind(source.as_str())
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.try_get(0)?)
}

/// Replace this repo's `github`-sourced tags with its current topic list.
pub(crate) async fn sync_github_topics(
    tx: &mut Transaction<'_, Sqlite>,
    repo_id: i64,
    topics: &[String],
) -> Result<()> {
    sqlx::query(
        "DELETE FROM repo_tags WHERE repo_id = ? \
         AND tag_id IN (SELECT id FROM tags WHERE source = 'github')",
    )
    .bind(repo_id)
    .execute(&mut **tx)
    .await?;

    for topic in topics {
        let id = tag_id(tx, topic, TagSource::Github).await?;
        sqlx::query(
            "INSERT INTO repo_tags (repo_id, tag_id, confidence) VALUES (?, ?, 1.0) \
             ON CONFLICT (repo_id, tag_id) DO NOTHING",
        )
        .bind(repo_id)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

impl Store {
    pub(crate) async fn repo_tags(&self, repo_id: i64) -> Result<Vec<RepoTag>> {
        let rows = sqlx::query(
            "SELECT t.name, t.source, rt.confidence FROM repo_tags rt \
             JOIN tags t ON t.id = rt.tag_id WHERE rt.repo_id = ? \
             ORDER BY t.source DESC, rt.confidence DESC, t.name ASC",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RepoTag {
                    name: row.try_get(0)?,
                    source: TagSource::parse(row.try_get::<String, _>(1)?.as_str())
                        .unwrap_or(TagSource::Github),
                    confidence: row.try_get::<f64, _>(2)? as f32,
                })
            })
            .collect()
    }

    pub(crate) async fn repo_groups(&self, repo_id: i64) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT g.name FROM repo_groups rg JOIN \"groups\" g ON g.id = rg.group_id \
             WHERE rg.repo_id = ? ORDER BY g.name",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(|r| Ok(r.try_get(0)?)).collect()
    }

    /// Replace this repo's AI tags. User and GitHub tags are untouched, and an
    /// AI tag whose name already exists as a user tag on this repo is dropped:
    /// the user's version wins and there is no point storing both.
    pub async fn set_ai_tags(&self, repo_id: i64, tags: &[RepoTag]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "DELETE FROM repo_tags WHERE repo_id = ? \
             AND tag_id IN (SELECT id FROM tags WHERE source = 'ai')",
        )
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;

        let existing: Vec<String> = sqlx::query(
            "SELECT lower(t.name) FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id \
             WHERE rt.repo_id = ? AND t.source = 'user'",
        )
        .bind(repo_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| r.try_get::<String, _>(0))
        .collect::<std::result::Result<_, _>>()?;

        for tag in tags {
            if existing.contains(&tag.name.to_lowercase()) {
                continue;
            }
            let id = tag_id(&mut tx, &tag.name, TagSource::Ai).await?;
            sqlx::query(
                "INSERT INTO repo_tags (repo_id, tag_id, confidence) VALUES (?, ?, ?) \
                 ON CONFLICT (repo_id, tag_id) DO UPDATE SET confidence = excluded.confidence",
            )
            .bind(repo_id)
            .bind(id)
            .bind(tag.confidence as f64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Attach a user tag. Idempotent.
    pub async fn add_user_tag(&self, repo_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin().await?;
        let id = tag_id(&mut tx, name, TagSource::User).await?;
        sqlx::query(
            "INSERT INTO repo_tags (repo_id, tag_id, confidence) VALUES (?, ?, 1.0) \
             ON CONFLICT (repo_id, tag_id) DO NOTHING",
        )
        .bind(repo_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Turn an AI suggestion into a user tag, dropping the AI original.
    pub async fn promote_tag(&self, repo_id: i64, name: &str) -> Result<()> {
        self.add_user_tag(repo_id, name).await?;
        self.remove_tag(repo_id, name, TagSource::Ai).await
    }

    pub async fn remove_tag(&self, repo_id: i64, name: &str, source: TagSource) -> Result<()> {
        sqlx::query(
            "DELETE FROM repo_tags WHERE repo_id = ? AND tag_id IN \
             (SELECT id FROM tags WHERE name = ? AND source = ?)",
        )
        .bind(repo_id)
        .bind(name)
        .bind(source.as_str())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Tags in use, most used first. Drives the sidebar.
    pub async fn tag_facets(&self) -> Result<Vec<TagFacet>> {
        let rows = sqlx::query(
            "SELECT t.name, t.source, count(rt.repo_id) AS n FROM tags t \
             JOIN repo_tags rt ON rt.tag_id = t.id \
             GROUP BY t.id ORDER BY n DESC, t.name ASC",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(TagFacet {
                    name: row.try_get(0)?,
                    source: TagSource::parse(row.try_get::<String, _>(1)?.as_str())
                        .unwrap_or(TagSource::Github),
                    count: row.try_get(2)?,
                })
            })
            .collect()
    }

    /// Replace every AI-sourced group with `groups`.
    ///
    /// Members are matched by `full_name`; names the store does not know are
    /// ignored, which is the expected outcome when a model invents one.
    pub async fn replace_ai_groups(&self, groups: &[Group]) -> Result<usize> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM \"groups\" WHERE source = 'ai'")
            .execute(&mut *tx)
            .await?;

        let mut linked = 0usize;
        for group in groups {
            let row = sqlx::query(
                "INSERT INTO \"groups\" (name, summary, source) VALUES (?, ?, ?) \
                 ON CONFLICT (name) DO UPDATE SET summary = excluded.summary RETURNING id",
            )
            .bind(&group.name)
            .bind(&group.summary)
            .bind(group.source.as_str())
            .fetch_one(&mut *tx)
            .await?;
            let group_id: i64 = row.try_get(0)?;

            for full_name in &group.members {
                let affected = sqlx::query(
                    "INSERT INTO repo_groups (repo_id, group_id) \
                     SELECT id, ? FROM repos WHERE full_name = ? \
                     ON CONFLICT (repo_id, group_id) DO NOTHING",
                )
                .bind(group_id)
                .bind(full_name)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                linked += affected as usize;
            }
        }
        tx.commit().await?;
        Ok(linked)
    }

    pub async fn group_facets(&self) -> Result<Vec<GroupFacet>> {
        let rows = sqlx::query(
            "SELECT g.name, g.summary, count(rg.repo_id) AS n FROM \"groups\" g \
             LEFT JOIN repo_groups rg ON rg.group_id = g.id \
             GROUP BY g.id ORDER BY n DESC, g.name ASC",
        )
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(GroupFacet {
                    name: row.try_get(0)?,
                    summary: row.try_get(1)?,
                    count: row.try_get(2)?,
                })
            })
            .collect()
    }

    /// Drop tags that no repo references any more.
    pub async fn prune_orphan_tags(&self) -> Result<u64> {
        let n = sqlx::query("DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM repo_tags)")
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(n)
    }
}
