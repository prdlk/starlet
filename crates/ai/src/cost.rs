//! Pre-flight price estimation.
//!
//! # This is an upper bound, not a quote
//!
//! Two approximations stack, both deliberately pessimistic:
//!
//! 1. **Tokens from characters.** One token per four characters is the standard
//!    rule of thumb for English and it over-counts for the JSON-heavy payloads
//!    we send, where punctuation and repeated keys tokenise densely.
//! 2. **A fixed per-repo size.** [`AiProvider::estimate`] is given a repo
//!    *count*, not the payload, because the UI quotes a price before the
//!    batches are built. The constants below assume a repo at the larger end of
//!    what GitHub returns and assume every repo comes back with the maximum six
//!    tags.
//!
//! The number shown to the user is therefore a ceiling: real runs land under
//! it. Prices are public list prices in USD per million tokens and drift, so an
//! unknown model falls back to the most expensive plausible sibling rather than
//! to zero.
//!
//! [`AiProvider::estimate`]: crate::provider::AiProvider::estimate

use crate::analysis::BATCH_SIZE;
use crate::prompt::{GROUP_SYSTEM, TAG_SYSTEM};
use crate::provider::CostEstimate;

/// USD per million tokens for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input_per_m: f64,
    pub output_per_m: f64,
}

impl Price {
    pub const FREE: Self = Self {
        input_per_m: 0.0,
        output_per_m: 0.0,
    };
}

/// The character-to-token ratio. See the module docs.
const CHARS_PER_TOKEN: u64 = 4;

/// Assumed serialised size of one `RepoSummary` (name, description, topics,
/// language, plus JSON punctuation).
const TAG_INPUT_CHARS_PER_REPO: u64 = 320;

/// Assumed serialised size of one `RepoWithTags` in the grouping pass: no
/// topics, but a flattened tag list.
const GROUP_INPUT_CHARS_PER_REPO: u64 = 200;

/// Six tags plus the surrounding JSON, per repo.
const TAG_OUTPUT_TOKENS_PER_REPO: u64 = 48;

/// A repo's share of the group listing it lands in.
const GROUP_OUTPUT_TOKENS_PER_REPO: u64 = 12;

/// Prefix-matched OpenAI list prices, most specific first: the table is scanned
/// in order and the first prefix match wins, so `gpt-4o-mini` must precede
/// `gpt-4o`.
const OPENAI_PRICES: &[(&str, Price)] = &[
    ("gpt-4.1-nano", price(0.10, 0.40)),
    ("gpt-4.1-mini", price(0.40, 1.60)),
    ("gpt-4.1", price(2.00, 8.00)),
    ("gpt-4o-mini", price(0.15, 0.60)),
    ("gpt-4o", price(2.50, 10.00)),
    ("o4-mini", price(1.10, 4.40)),
    ("o3-mini", price(1.10, 4.40)),
    ("o3", price(2.00, 8.00)),
];

/// Charged as `gpt-4o` when the model is unrecognised.
const OPENAI_FALLBACK: Price = price(2.50, 10.00);

/// Prefix-matched Anthropic list prices, most specific first.
const ANTHROPIC_PRICES: &[(&str, Price)] = &[
    ("claude-3-5-haiku", price(0.80, 4.00)),
    ("claude-3-haiku", price(0.25, 1.25)),
    ("claude-haiku-4", price(1.00, 5.00)),
    ("claude-3-5-sonnet", price(3.00, 15.00)),
    ("claude-3-7-sonnet", price(3.00, 15.00)),
    ("claude-sonnet-4", price(3.00, 15.00)),
    ("claude-3-opus", price(15.00, 75.00)),
    ("claude-opus-4", price(15.00, 75.00)),
];

/// Charged as a Sonnet when the model is unrecognised.
const ANTHROPIC_FALLBACK: Price = price(3.00, 15.00);

const fn price(input_per_m: f64, output_per_m: f64) -> Price {
    Price {
        input_per_m,
        output_per_m,
    }
}

pub(crate) fn openai_price(model: &str) -> Price {
    lookup(OPENAI_PRICES, model).unwrap_or(OPENAI_FALLBACK)
}

pub(crate) fn anthropic_price(model: &str) -> Price {
    lookup(ANTHROPIC_PRICES, model).unwrap_or(ANTHROPIC_FALLBACK)
}

fn lookup(table: &[(&str, Price)], model: &str) -> Option<Price> {
    let model = model.trim().to_lowercase();
    table
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, price)| *price)
}

/// Token counts for a full run over `repos` repositories: every tagging batch
/// plus the single grouping pass.
fn tokens(repos: usize) -> (u64, u64) {
    let repos = repos as u64;
    if repos == 0 {
        return (0, 0);
    }
    let batches = repos.div_ceil(BATCH_SIZE as u64);

    // The system prompt is re-sent with every batch, so it is billed per batch.
    let tag_overhead = batches * (TAG_SYSTEM.len() as u64).div_ceil(CHARS_PER_TOKEN);
    let group_overhead = (GROUP_SYSTEM.len() as u64).div_ceil(CHARS_PER_TOKEN);

    let input = tag_overhead
        + group_overhead
        + repos * (TAG_INPUT_CHARS_PER_REPO + GROUP_INPUT_CHARS_PER_REPO) / CHARS_PER_TOKEN;
    let output = repos * (TAG_OUTPUT_TOKENS_PER_REPO + GROUP_OUTPUT_TOKENS_PER_REPO);
    (input, output)
}

/// Combine the token model with a price table.
pub(crate) fn estimate(repos: usize, price: Price) -> CostEstimate {
    let (input_tokens, output_tokens) = tokens(repos);
    let usd = (input_tokens as f64 * price.input_per_m + output_tokens as f64 * price.output_per_m)
        / 1_000_000.0;
    CostEstimate {
        input_tokens,
        output_tokens,
        usd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_run_costs_nothing() {
        let e = estimate(0, OPENAI_FALLBACK);
        assert_eq!(e, CostEstimate::default());
    }

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(openai_price("gpt-4o-mini-2024-07-18").input_per_m, 0.15);
        assert_eq!(openai_price("gpt-4o-2024-11-20").input_per_m, 2.50);
        assert_eq!(anthropic_price("claude-3-5-haiku-latest").input_per_m, 0.80);
    }

    #[test]
    fn unknown_models_fall_back_to_the_expensive_sibling() {
        assert_eq!(openai_price("gpt-9-turbo"), OPENAI_FALLBACK);
        assert_eq!(anthropic_price("claude-99"), ANTHROPIC_FALLBACK);
    }

    #[test]
    fn a_free_price_table_yields_zero_usd_but_real_token_counts() {
        let e = estimate(100, Price::FREE);
        assert_eq!(e.usd, 0.0);
        assert!(e.input_tokens > 0 && e.output_tokens > 0);
    }

    #[test]
    fn cost_grows_with_the_library() {
        let small = estimate(10, OPENAI_FALLBACK);
        let large = estimate(1000, OPENAI_FALLBACK);
        assert!(large.usd > small.usd);
    }
}
