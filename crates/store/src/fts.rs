//! The BM25 half of search.
//!
//! `nucleo` handles `owner/name`; this handles everything written in prose.
//! The two are combined in `starlet_core::rank`.

use std::collections::HashMap;

use sqlx::Row;

use crate::{Result, Store};

/// One full-text hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FtsHit {
    pub repo_id: i64,
    /// Positive relevance: SQLite's `bm25()` negated, so bigger is better.
    pub relevance: f32,
}

/// Column weights passed to `bm25()`, in table-declaration order.
///
/// `full_name` is deliberately the *lowest* weight: the fuzzy matcher already
/// owns that field and gets 70 % of the final score. What FTS contributes that
/// nucleo cannot is the description and the tag vocabulary.
const W_FULL_NAME: f32 = 1.0;
const W_DESCRIPTION: f32 = 5.0;
const W_TOPICS: f32 = 3.0;
const W_TAGS: f32 = 4.0;

impl Store {
    /// Score every repo whose indexed text matches `terms`.
    ///
    /// Terms are ANDed and each is treated as a prefix, matching the
    /// type-as-you-go behaviour of the fuzzy side. Returns an empty map when
    /// there is nothing to search for.
    pub async fn search_fts(&self, terms: &[String], limit: i64) -> Result<HashMap<i64, f32>> {
        let Some(match_expr) = build_match(terms) else {
            return Ok(HashMap::new());
        };

        let sql = format!(
            "SELECT rowid, -bm25(repos_fts, {W_FULL_NAME}, {W_DESCRIPTION}, {W_TOPICS}, {W_TAGS}) AS rel \
             FROM repos_fts WHERE repos_fts MATCH ? ORDER BY rel DESC LIMIT ?"
        );
        let rows = sqlx::query(&sql)
            .bind(match_expr)
            .bind(limit)
            .fetch_all(self.pool())
            .await;

        // A malformed MATCH expression is a user typing, not a bug. Treat it
        // as "no full-text signal" and let the fuzzy matcher carry the query.
        let rows = match rows {
            Ok(rows) => rows,
            Err(sqlx::Error::Database(err)) => {
                tracing::debug!("fts match rejected: {err}");
                return Ok(HashMap::new());
            }
            Err(err) => return Err(err.into()),
        };

        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get(0)?;
            let rel: f64 = row.try_get(1)?;
            out.insert(id, rel as f32);
        }
        Ok(out)
    }
}

/// Turn free-text terms into a safe FTS5 MATCH expression.
///
/// Every term is emitted as a quoted string followed by `*`, so FTS5 operators
/// the user typed (`OR`, `NEAR`, `-`, `^`) are data, not syntax. Embedded
/// double quotes are doubled per the FTS5 string literal rules.
fn build_match(terms: &[String]) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(terms.len());
    for term in terms {
        let cleaned: String = term
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '"'))
            .collect();
        let cleaned = cleaned.trim_matches(|c: char| !c.is_alphanumeric());
        if cleaned.is_empty() {
            continue;
        }
        parts.push(format!("\"{}\"*", cleaned.replace('"', "\"\"")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_quoted_prefixes_joined_by_and() {
        assert_eq!(
            build_match(&["rust".into(), "http".into()]).unwrap(),
            "\"rust\"* AND \"http\"*"
        );
    }

    #[test]
    fn fts5_operators_are_neutralised() {
        // Without quoting, `OR` and `NEAR` would change the query's meaning and
        // a bare `-` would be a NOT.
        assert_eq!(build_match(&["OR".into()]).unwrap(), "\"OR\"*");
        assert_eq!(build_match(&["a\"b".into()]).unwrap(), "\"a\"\"b\"*");
        assert_eq!(build_match(&["-".into()]), None);
    }

    #[test]
    fn punctuation_only_terms_are_dropped() {
        assert_eq!(build_match(&["***".into()]), None);
        assert_eq!(build_match(&[]), None);
    }
}
