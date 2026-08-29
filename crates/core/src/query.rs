//! The Starlet query language.
//!
//! ```text
//! query   := item (ws+ item)*
//! item    := '-'? (field ':' value) | word
//! field   := lang | language | tag | group | owner | user | stars | is | sort
//! value   := '"' … '"' | word
//! ```
//!
//! Parsing is total: anything that is not a recognised `field:value` pair
//! becomes free text. That matters because the parser runs on every keystroke
//! — a half-typed `lang:` must not blank the result list, and a pasted URL
//! must not become a parse error.

use crate::model::Repo;

/// A parsed query: the fuzzy needle plus the structured filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Free-text tokens in the order the user typed them.
    pub terms: Vec<String>,
    /// `terms` joined by a single space. This is the fuzzy-match needle.
    pub text: String,
    pub clauses: Vec<Clause>,
    /// Explicit `sort:` override. `None` means "rank by relevance".
    pub sort: Option<SortKey>,
}

/// A filter plus its polarity. `-lang:rust` is a negated `Language` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub negated: bool,
    pub filter: Filter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Matches `primary_language` or any key of the language breakdown.
    Language(String),
    /// Matches a tag name from any source.
    Tag(String),
    Group(String),
    Owner(String),
    Stars(StarRange),
    Archived(bool),
    Fork(bool),
}

/// The field name that introduced a clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Language,
    Tag,
    Group,
    Owner,
    Stars,
    Is,
    Sort,
}

impl Field {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "lang" | "language" => Some(Field::Language),
            "tag" => Some(Field::Tag),
            "group" => Some(Field::Group),
            "owner" | "user" => Some(Field::Owner),
            "stars" => Some(Field::Stars),
            "is" => Some(Field::Is),
            "sort" => Some(Field::Sort),
            _ => None,
        }
    }

    /// The prefixes offered by the command palette and completion hints.
    pub const ALL: [&'static str; 7] = [
        "lang:", "tag:", "group:", "owner:", "stars:", "is:", "sort:",
    ];
}

/// An inclusive star-count interval. `None` on a side means unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StarRange {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

impl StarRange {
    pub fn contains(&self, n: i64) -> bool {
        self.min.is_none_or(|lo| n >= lo) && self.max.is_none_or(|hi| n <= hi)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Combined fuzzy + BM25 score. The default.
    Relevance,
    Stars,
    /// `full_name`, ascending.
    Name,
    /// `pushed_at`, newest first.
    Recent,
    /// `starred_at`, newest first.
    Starred,
}

impl SortKey {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "relevance" | "best" => Some(SortKey::Relevance),
            "stars" => Some(SortKey::Stars),
            "name" | "alpha" => Some(SortKey::Name),
            "recent" | "pushed" | "updated" => Some(SortKey::Recent),
            "starred" | "added" => Some(SortKey::Starred),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Relevance => "Relevance",
            SortKey::Stars => "Stars",
            SortKey::Name => "Name",
            SortKey::Recent => "Last commit",
            SortKey::Starred => "Recently starred",
        }
    }

    pub const ALL: [SortKey; 5] = [
        SortKey::Relevance,
        SortKey::Stars,
        SortKey::Name,
        SortKey::Recent,
        SortKey::Starred,
    ];
}

/// Parse a raw input string. Never fails; see the module docs.
pub fn parse(input: &str) -> Query {
    let mut query = Query::default();

    for raw in tokenize(input) {
        let (negated, body) = match raw.strip_prefix('-') {
            // A lone `-` is text, not a negation.
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, raw.as_str()),
        };

        match split_field(body) {
            Some((field, value)) => {
                if value.is_empty() {
                    // Mid-typing (`lang:`). Not yet a constraint, not text.
                    continue;
                }
                match apply(field, value, negated) {
                    Applied::Clause(filter) => query.clauses.push(Clause { negated, filter }),
                    Applied::Sort(key) => query.sort = Some(key),
                    // `stars:banana` — keep it visible as text rather than
                    // silently dropping what the user typed.
                    Applied::NotAValue => query.terms.push(raw.clone()),
                }
            }
            None => query.terms.push(unquote(&raw)),
        }
    }

    query.text = query.terms.join(" ");
    query
}

enum Applied {
    Clause(Filter),
    Sort(SortKey),
    NotAValue,
}

fn apply(field: Field, value: &str, negated: bool) -> Applied {
    let value = unquote(value);
    match field {
        Field::Language => Applied::Clause(Filter::Language(value)),
        Field::Tag => Applied::Clause(Filter::Tag(value)),
        Field::Group => Applied::Clause(Filter::Group(value)),
        Field::Owner => Applied::Clause(Filter::Owner(value)),
        Field::Stars => match parse_star_range(&value) {
            Some(range) => Applied::Clause(Filter::Stars(range)),
            None => Applied::NotAValue,
        },
        Field::Is => match value.to_ascii_lowercase().as_str() {
            "archived" => Applied::Clause(Filter::Archived(true)),
            // `is:active` and `-is:archived` mean the same thing; the outer
            // negation flips whichever polarity we produce here.
            "active" => Applied::Clause(Filter::Archived(negated)),
            "fork" => Applied::Clause(Filter::Fork(true)),
            "source" => Applied::Clause(Filter::Fork(negated)),
            _ => Applied::NotAValue,
        },
        Field::Sort => match SortKey::parse(&value.to_ascii_lowercase()) {
            Some(key) => Applied::Sort(key),
            None => Applied::NotAValue,
        },
    }
}

/// Split on the first `:` and resolve the left side to a known field.
fn split_field(body: &str) -> Option<(Field, &str)> {
    let (name, value) = body.split_once(':')?;
    let field = Field::parse(&name.to_ascii_lowercase())?;
    Some((field, value))
}

/// Split on whitespace, keeping double-quoted runs together.
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut has_content = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => {
                in_quotes = !in_quotes;
                has_content = true;
                current.push(ch);
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_content {
                    out.push(std::mem::take(&mut current));
                    has_content = false;
                }
            }
            c => {
                current.push(c);
                has_content = true;
            }
        }
    }
    if has_content {
        out.push(current);
    }
    out.retain(|t| !t.is_empty() && t != "\"" && t != "\"\"");
    out
}

/// Strip a surrounding pair of double quotes, if present.
fn unquote(s: &str) -> String {
    let trimmed = s.strip_prefix('"').unwrap_or(s);
    let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
    trimmed.to_string()
}

/// `>1000`, `>=1k`, `<50`, `100..500`, `42`.
fn parse_star_range(value: &str) -> Option<StarRange> {
    let v = value.trim();
    if let Some((lo, hi)) = v.split_once("..") {
        let min = if lo.is_empty() {
            None
        } else {
            Some(parse_count(lo)?)
        };
        let max = if hi.is_empty() {
            None
        } else {
            Some(parse_count(hi)?)
        };
        if min.is_none() && max.is_none() {
            return None;
        }
        return Some(StarRange { min, max });
    }
    if let Some(rest) = v.strip_prefix(">=") {
        return Some(StarRange {
            min: Some(parse_count(rest)?),
            max: None,
        });
    }
    if let Some(rest) = v.strip_prefix("<=") {
        return Some(StarRange {
            min: None,
            max: Some(parse_count(rest)?),
        });
    }
    if let Some(rest) = v.strip_prefix('>') {
        return Some(StarRange {
            min: Some(parse_count(rest)?.saturating_add(1)),
            max: None,
        });
    }
    if let Some(rest) = v.strip_prefix('<') {
        return Some(StarRange {
            min: None,
            max: Some(parse_count(rest)?.saturating_sub(1)),
        });
    }
    let n = parse_count(v)?;
    Some(StarRange {
        min: Some(n),
        max: Some(n),
    })
}

/// A non-negative integer, optionally with a `k` or `m` magnitude suffix.
fn parse_count(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (digits, scale) = match s.as_bytes()[s.len() - 1] {
        b'k' | b'K' => (&s[..s.len() - 1], 1_000f64),
        b'm' | b'M' => (&s[..s.len() - 1], 1_000_000f64),
        _ => (s, 1f64),
    };
    if digits.is_empty() {
        return None;
    }
    if scale == 1f64 {
        return digits.parse::<i64>().ok().filter(|n| *n >= 0);
    }
    let n: f64 = digits.parse().ok()?;
    if !n.is_finite() || n < 0.0 || n > 1e12 {
        return None;
    }
    Some((n * scale) as i64)
}

impl Query {
    /// True when the query carries no constraint at all.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.clauses.is_empty() && self.sort.is_none()
    }

    /// True when there is a fuzzy needle to score against.
    pub fn has_text(&self) -> bool {
        !self.text.is_empty()
    }

    /// Apply every clause. Clauses are ANDed; a negated clause must not match.
    pub fn matches(&self, repo: &Repo) -> bool {
        self.clauses
            .iter()
            .all(|clause| clause.filter.matches(repo) != clause.negated)
    }
}

impl Filter {
    pub fn matches(&self, repo: &Repo) -> bool {
        match self {
            Filter::Language(lang) => {
                repo.primary_language
                    .as_deref()
                    .is_some_and(|l| l.eq_ignore_ascii_case(lang))
                    || repo.languages.keys().any(|l| l.eq_ignore_ascii_case(lang))
            }
            Filter::Tag(tag) => {
                repo.has_tag(tag) || repo.topics.iter().any(|t| t.eq_ignore_ascii_case(tag))
            }
            Filter::Group(group) => repo.in_group(group),
            Filter::Owner(owner) => repo.owner.eq_ignore_ascii_case(owner),
            Filter::Stars(range) => range.contains(repo.stargazers),
            Filter::Archived(want) => repo.archived == *want,
            Filter::Fork(want) => repo.fork == *want,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(input: &str) -> Vec<Filter> {
        parse(input).clauses.into_iter().map(|c| c.filter).collect()
    }

    #[test]
    fn plain_text_becomes_terms() {
        let q = parse("  rust   http  client ");
        assert_eq!(q.terms, ["rust", "http", "client"]);
        assert_eq!(q.text, "rust http client");
        assert!(q.clauses.is_empty());
    }

    #[test]
    fn recognises_every_prefix() {
        let q = parse("lang:rust tag:cli group:tools owner:helix-editor stars:>1000");
        assert_eq!(
            q.clauses
                .iter()
                .map(|c| c.filter.clone())
                .collect::<Vec<_>>(),
            vec![
                Filter::Language("rust".into()),
                Filter::Tag("cli".into()),
                Filter::Group("tools".into()),
                Filter::Owner("helix-editor".into()),
                Filter::Stars(StarRange {
                    min: Some(1001),
                    max: None
                }),
            ]
        );
        assert!(q.terms.is_empty());
    }

    #[test]
    fn language_alias_and_case() {
        assert_eq!(
            filters("LANGUAGE:Rust"),
            vec![Filter::Language("Rust".into())]
        );
        assert_eq!(filters("user:foo"), vec![Filter::Owner("foo".into())]);
    }

    #[test]
    fn star_ranges() {
        assert_eq!(
            filters("stars:>=1k"),
            vec![Filter::Stars(StarRange {
                min: Some(1000),
                max: None
            })]
        );
        assert_eq!(
            filters("stars:<50"),
            vec![Filter::Stars(StarRange {
                min: None,
                max: Some(49)
            })]
        );
        assert_eq!(
            filters("stars:100..500"),
            vec![Filter::Stars(StarRange {
                min: Some(100),
                max: Some(500)
            })]
        );
        assert_eq!(
            filters("stars:1.5k"),
            vec![Filter::Stars(StarRange {
                min: Some(1500),
                max: Some(1500)
            })]
        );
    }

    #[test]
    fn unparseable_value_stays_text() {
        let q = parse("stars:banana");
        assert!(q.clauses.is_empty());
        assert_eq!(q.terms, ["stars:banana"]);
    }

    #[test]
    fn incomplete_prefix_is_inert() {
        let q = parse("lang:");
        assert!(q.is_empty());
        assert_eq!(q.text, "");
    }

    #[test]
    fn negation() {
        let q = parse("-lang:rust -is:fork");
        assert!(q.clauses.iter().all(|c| c.negated));
        assert_eq!(
            q.clauses[1].filter,
            Filter::Fork(true),
            "polarity lives on the clause, not the filter"
        );
    }

    #[test]
    fn is_active_is_the_inverse_of_archived() {
        assert_eq!(filters("is:active"), vec![Filter::Archived(false)]);
        assert_eq!(filters("is:archived"), vec![Filter::Archived(true)]);
        // `-is:active` must mean archived: the clause negation flips the
        // polarity we produced, so the filter itself is emitted as `true`.
        let q = parse("-is:active");
        assert_eq!(q.clauses[0].filter, Filter::Archived(true));
        assert!(q.clauses[0].negated);
    }

    #[test]
    fn quoted_values_keep_spaces() {
        let q = parse("tag:\"machine learning\" hello world");
        assert_eq!(
            filters("tag:\"machine learning\""),
            vec![Filter::Tag("machine learning".into())]
        );
        assert_eq!(q.terms, ["hello", "world"]);
    }

    #[test]
    fn lone_dash_is_text() {
        assert_eq!(parse("-").terms, ["-"]);
    }

    #[test]
    fn sort_is_not_a_filter() {
        let q = parse("sort:stars rust");
        assert_eq!(q.sort, Some(SortKey::Stars));
        assert_eq!(q.terms, ["rust"]);
        assert!(q.clauses.is_empty());
    }

    #[test]
    fn matching_is_and_across_clauses() {
        let repo = Repo {
            owner: "helix-editor".into(),
            full_name: "helix-editor/helix".into(),
            primary_language: Some("Rust".into()),
            stargazers: 30_000,
            topics: vec!["editor".into()],
            ..Default::default()
        };
        assert!(parse("lang:rust owner:helix-editor stars:>1000").matches(&repo));
        assert!(!parse("lang:go").matches(&repo));
        assert!(parse("-lang:go").matches(&repo));
        assert!(parse("tag:editor").matches(&repo), "topics count as tags");
        assert!(!parse("stars:<100").matches(&repo));
    }
}
