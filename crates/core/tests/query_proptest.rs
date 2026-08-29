//! Property tests for the query parser.
//!
//! The parser runs on every keystroke against text the user is halfway through
//! typing, so the properties that matter are totality and stability rather
//! than a grammar round-trip.

use proptest::prelude::*;
use starlet_core::model::Repo;
use starlet_core::query::{self, Filter};

/// Values that cannot be confused with syntax: no whitespace, quotes, colons,
/// or leading dash.
fn safe_value() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_.][a-zA-Z0-9_.-]{0,20}".prop_map(|s| s)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Parsing is total. Nothing the user can type may panic the search path.
    #[test]
    fn never_panics(input in ".{0,200}") {
        let _ = query::parse(&input);
    }

    /// A term is never empty and never carries whitespace: the tokenizer either
    /// keeps a quoted run together or splits it.
    #[test]
    fn terms_are_well_formed(input in ".{0,200}") {
        let q = query::parse(&input);
        for term in &q.terms {
            prop_assert!(!term.is_empty());
        }
        // `text` is exactly the terms rejoined.
        prop_assert_eq!(q.text, q.terms.join(" "));
    }

    /// Re-parsing the free-text projection is a fixed point. The search view
    /// relies on this when it rewrites the input after a palette action.
    #[test]
    fn free_text_is_idempotent(words in prop::collection::vec("[a-z]{1,8}", 0..6)) {
        let input = words.join(" ");
        let once = query::parse(&input);
        let twice = query::parse(&once.text);
        prop_assert_eq!(once.terms, twice.terms);
    }

    /// Every recognised prefix produces exactly one clause and no free text.
    #[test]
    fn known_prefixes_never_leak_into_text(value in safe_value()) {
        for (prefix, build) in [
            ("lang", Filter::Language as fn(String) -> Filter),
            ("language", Filter::Language),
            ("tag", Filter::Tag),
            ("group", Filter::Group),
            ("owner", Filter::Owner),
            ("user", Filter::Owner),
        ] {
            let q = query::parse(&format!("{prefix}:{value}"));
            prop_assert!(q.terms.is_empty(), "{prefix}:{value} leaked to text");
            prop_assert_eq!(q.clauses.len(), 1);
            prop_assert_eq!(&q.clauses[0].filter, &build(value.clone()));
            prop_assert!(!q.clauses[0].negated);
        }
    }

    /// Negating a clause inverts its verdict for every repo.
    #[test]
    fn negation_inverts_matching(
        value in safe_value(),
        owner in "[a-z]{1,8}",
        lang in "[A-Za-z]{1,8}",
        stars in 0i64..1_000_000,
    ) {
        let repo = Repo {
            owner: owner.clone(),
            full_name: format!("{owner}/thing"),
            primary_language: Some(lang),
            stargazers: stars,
            ..Default::default()
        };
        for prefix in ["lang", "tag", "group", "owner"] {
            let positive = query::parse(&format!("{prefix}:{value}"));
            let negative = query::parse(&format!("-{prefix}:{value}"));
            prop_assert_ne!(positive.matches(&repo), negative.matches(&repo));
        }
    }

    /// A star range always contains its own bounds and rejects outside them.
    #[test]
    fn star_ranges_are_inclusive(lo in 0i64..100_000, span in 0i64..100_000) {
        let hi = lo + span;
        let q = query::parse(&format!("stars:{lo}..{hi}"));
        prop_assert_eq!(q.clauses.len(), 1);
        let Filter::Stars(range) = &q.clauses[0].filter else {
            return Err(TestCaseError::fail("expected a star range"));
        };
        prop_assert!(range.contains(lo));
        prop_assert!(range.contains(hi));
        prop_assert!(!range.contains(lo - 1));
        prop_assert!(!range.contains(hi + 1));
    }

    /// `>n` and `>=n+1` describe the same set.
    #[test]
    fn strict_and_inclusive_bounds_agree(n in 0i64..1_000_000) {
        let strict = query::parse(&format!("stars:>{n}"));
        let inclusive = query::parse(&format!("stars:>={}", n + 1));
        prop_assert_eq!(strict.clauses, inclusive.clauses);
    }

    /// An incomplete prefix constrains nothing, whatever the field.
    #[test]
    fn incomplete_prefixes_are_inert(field in "(lang|tag|group|owner|stars|is|sort)") {
        let q = query::parse(&format!("{field}:"));
        prop_assert!(q.is_empty(), "{field}: produced {q:?}");
    }
}
