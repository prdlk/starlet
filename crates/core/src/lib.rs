//! Domain model, query language, and search ranking for Starlet.
//!
//! This crate is deliberately free of I/O, GPUI, and SQL: everything here is a
//! pure function of its inputs so the query parser and the ranking formula can
//! be tested without a database or a window.

pub mod model;
pub mod query;
pub mod rank;

pub use model::{Contributor, Group, LanguageBytes, Repo, RepoSummary, RepoTag, TagSource};
pub use query::{Clause, Field, Filter, Query, SortKey, StarRange, parse};
pub use rank::{FTS_WEIGHT, FUZZY_WEIGHT, Ranker, Scored};
