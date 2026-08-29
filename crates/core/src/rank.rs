//! Result ordering.
//!
//! Two signals are combined for every candidate:
//!
//! * a **fuzzy** score from `nucleo` over `owner/name`, which rewards the
//!   acronym-and-initials typing people actually do (`hlxed` → `helix-editor`);
//! * an **FTS5 BM25** relevance over description, topics, and tag names, which
//!   finds repos whose name says nothing useful.
//!
//! Both are min-max normalised inside the candidate set before they are
//! weighted, because their natural scales are unrelated: nucleo emits small
//! unsigned integers, BM25 emits unbounded negative reals. See `docs/search.md`
//! for the full derivation.

use std::collections::HashMap;

use nucleo::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::model::Repo;
use crate::query::{Query, SortKey};

/// Weight applied to the normalised fuzzy score.
pub const FUZZY_WEIGHT: f32 = 0.7;
/// Weight applied to the normalised BM25 score.
pub const FTS_WEIGHT: f32 = 0.3;

/// One ranked candidate: an index into the slice handed to [`Ranker::rank`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub ix: usize,
    /// Combined score in `0.0..=1.0`. Always `0.0` for a text-free query.
    pub score: f32,
    /// Normalised fuzzy component, kept for the score breakdown in the UI.
    pub fuzzy: f32,
    /// Normalised BM25 component.
    pub fts: f32,
}

/// Reusable fuzzy matcher plus its scratch buffers.
///
/// `nucleo`'s matcher owns a slab it reuses between calls, so one `Ranker`
/// should live for the lifetime of the search view rather than being rebuilt
/// per keystroke.
pub struct Ranker {
    matcher: Matcher,
    haystack_buf: Vec<char>,
}

impl Default for Ranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Ranker {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(nucleo::Config::DEFAULT),
            haystack_buf: Vec::new(),
        }
    }

    /// Rank `repos` for `query`.
    ///
    /// `fts` maps repo id to a **positive** BM25 relevance (the store already
    /// negates SQLite's `bm25()`, which is smaller-is-better). Ids absent from
    /// the map scored nothing in the full-text index.
    ///
    /// Candidates that match neither signal are dropped when the query has
    /// text; with no text every repo survives and only the sort key applies.
    pub fn rank(&mut self, query: &Query, repos: &[Repo], fts: &HashMap<i64, f32>) -> Vec<Scored> {
        let sort = query.sort.unwrap_or(if query.has_text() {
            SortKey::Relevance
        } else {
            SortKey::Starred
        });

        let mut scored = if query.has_text() {
            self.score_text(&query.text, repos, fts)
        } else {
            (0..repos.len())
                .map(|ix| Scored {
                    ix,
                    score: 0.0,
                    fuzzy: 0.0,
                    fts: 0.0,
                })
                .collect()
        };

        sort_by(&mut scored, sort, repos);
        scored
    }

    fn score_text(&mut self, needle: &str, repos: &[Repo], fts: &HashMap<i64, f32>) -> Vec<Scored> {
        let pattern = Pattern::parse(needle, CaseMatching::Smart, Normalization::Smart);

        // Pass 1: collect raw signals and their maxima.
        let mut raw: Vec<(usize, f32, f32)> = Vec::with_capacity(repos.len());
        let mut max_fuzzy = 0f32;
        let mut max_fts = 0f32;

        for (ix, repo) in repos.iter().enumerate() {
            self.haystack_buf.clear();
            let haystack = Utf32Str::new(&repo.full_name, &mut self.haystack_buf);
            let fuzzy = pattern.score(haystack, &mut self.matcher);
            let text = fts.get(&repo.id).copied().unwrap_or(0.0).max(0.0);

            // Neither index knows about this repo: it is not a result.
            if fuzzy.is_none() && text <= 0.0 {
                continue;
            }
            let fuzzy = fuzzy.unwrap_or(0) as f32;
            max_fuzzy = max_fuzzy.max(fuzzy);
            max_fts = max_fts.max(text);
            raw.push((ix, fuzzy, text));
        }

        // Pass 2: normalise into 0..=1 and combine.
        raw.into_iter()
            .map(|(ix, fuzzy, text)| {
                let f = if max_fuzzy > 0.0 {
                    fuzzy / max_fuzzy
                } else {
                    0.0
                };
                let t = if max_fts > 0.0 { text / max_fts } else { 0.0 };
                Scored {
                    ix,
                    score: FUZZY_WEIGHT * f + FTS_WEIGHT * t,
                    fuzzy: f,
                    fts: t,
                }
            })
            .collect()
    }
}

/// Order `scored` in place. Every branch falls through to the same tie-break so
/// the list is a total order and never reshuffles between identical renders.
fn sort_by(scored: &mut [Scored], sort: SortKey, repos: &[Repo]) {
    match sort {
        SortKey::Relevance => scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| tie_break(&repos[a.ix], &repos[b.ix]))
        }),
        SortKey::Stars => scored.sort_by(|a, b| tie_break(&repos[a.ix], &repos[b.ix])),
        SortKey::Name => scored.sort_by(|a, b| {
            repos[a.ix]
                .full_name
                .to_lowercase()
                .cmp(&repos[b.ix].full_name.to_lowercase())
                .then_with(|| tie_break(&repos[a.ix], &repos[b.ix]))
        }),
        SortKey::Recent => scored.sort_by(|a, b| {
            repos[b.ix]
                .last_commit_at
                .cmp(&repos[a.ix].last_commit_at)
                .then_with(|| tie_break(&repos[a.ix], &repos[b.ix]))
        }),
        SortKey::Starred => scored.sort_by(|a, b| {
            repos[b.ix]
                .starred_at
                .cmp(&repos[a.ix].starred_at)
                .then_with(|| tie_break(&repos[a.ix], &repos[b.ix]))
        }),
    }
}

/// Stars descending, then name ascending. Name is unique per GitHub account,
/// so this makes every comparison decisive.
fn tie_break(a: &Repo, b: &Repo) -> std::cmp::Ordering {
    b.stargazers
        .cmp(&a.stargazers)
        .then_with(|| a.full_name.cmp(&b.full_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query;
    use chrono::{TimeZone, Utc};

    fn repo(id: i64, full_name: &str, stars: i64) -> Repo {
        let (owner, name) = full_name.split_once('/').unwrap();
        Repo {
            id,
            full_name: full_name.into(),
            owner: owner.into(),
            name: name.into(),
            stargazers: stars,
            starred_at: Utc.timestamp_opt(1_700_000_000 + id, 0).single(),
            ..Default::default()
        }
    }

    fn corpus() -> Vec<Repo> {
        vec![
            repo(1, "helix-editor/helix", 30_000),
            repo(2, "neovim/neovim", 80_000),
            repo(3, "zed-industries/zed", 50_000),
            repo(4, "microsoft/vscode", 160_000),
            repo(5, "helix-toolkit/helix-toolkit", 4_000),
        ]
    }

    fn names(scored: &[Scored], repos: &[Repo]) -> Vec<String> {
        scored
            .iter()
            .map(|s| repos[s.ix].full_name.clone())
            .collect()
    }

    #[test]
    fn exact_prefix_outranks_a_looser_fuzzy_hit() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let out = ranker.rank(&query::parse("helix"), &repos, &HashMap::new());
        assert_eq!(
            names(&out, &repos),
            ["helix-editor/helix", "helix-toolkit/helix-toolkit"]
        );
    }

    #[test]
    fn non_matching_repos_are_dropped() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let out = ranker.rank(&query::parse("zed"), &repos, &HashMap::new());
        assert_eq!(names(&out, &repos), ["zed-industries/zed"]);
    }

    #[test]
    fn fts_only_hit_survives_with_no_name_match() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        // Nothing in the corpus is named "modal", but pretend the description
        // index found neovim.
        let fts = HashMap::from([(2i64, 8.0f32)]);
        let out = ranker.rank(&query::parse("modal"), &repos, &fts);
        assert_eq!(names(&out, &repos), ["neovim/neovim"]);
        assert_eq!(out[0].fuzzy, 0.0);
        assert_eq!(out[0].fts, 1.0);
        assert!((out[0].score - FTS_WEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn score_is_the_weighted_sum_of_its_components() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let fts = HashMap::from([(1i64, 3.0f32), (5i64, 10.0f32)]);
        let out = ranker.rank(&query::parse("helix"), &repos, &fts);
        assert!(!out.is_empty());
        for s in &out {
            let expected = FUZZY_WEIGHT * s.fuzzy + FTS_WEIGHT * s.fts;
            assert!((s.score - expected).abs() < 1e-6, "{s:?}");
        }
    }

    #[test]
    fn a_name_hit_outweighs_a_description_hit() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        // "zed" matches one repo by name and nothing else; hand neovim the
        // top description score. 0.7 * 1.0 must beat 0.3 * 1.0.
        let fts = HashMap::from([(2i64, 10.0f32)]);
        let out = ranker.rank(&query::parse("zed"), &repos, &fts);
        assert_eq!(names(&out, &repos), ["zed-industries/zed", "neovim/neovim"]);
        assert!((out[0].score - FUZZY_WEIGHT).abs() < 1e-6);
        assert!((out[1].score - FTS_WEIGHT).abs() < 1e-6);
    }

    #[test]
    fn tie_break_is_stars_then_name() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let out = ranker.rank(&query::parse("sort:stars"), &repos, &HashMap::new());
        assert_eq!(
            names(&out, &repos),
            [
                "microsoft/vscode",
                "neovim/neovim",
                "zed-industries/zed",
                "helix-editor/helix",
                "helix-toolkit/helix-toolkit",
            ]
        );
    }

    #[test]
    fn empty_query_browses_by_recently_starred() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let out = ranker.rank(&query::parse(""), &repos, &HashMap::new());
        assert_eq!(out.len(), repos.len());
        assert_eq!(names(&out, &repos)[0], "helix-toolkit/helix-toolkit");
    }

    #[test]
    fn explicit_sort_overrides_relevance() {
        let repos = corpus();
        let mut ranker = Ranker::new();
        let out = ranker.rank(&query::parse("helix sort:name"), &repos, &HashMap::new());
        assert_eq!(
            names(&out, &repos),
            ["helix-editor/helix", "helix-toolkit/helix-toolkit"]
        );
    }
}
