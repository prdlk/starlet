//! Turning whatever the model actually said into typed values.
//!
//! Models append apologies, wrap answers in fences, and occasionally emit a
//! bare array where an object was asked for. The split enforced here:
//!
//! * **Structure is rejected.** A missing `repos` key or an entry with no
//!   `full_name` means the model did not answer the question, so the caller
//!   gets one retry and then an error.
//! * **Values are sanitised.** An out-of-range confidence or a seventh tag is
//!   still a usable answer, so it is clamped or dropped rather than thrown
//!   away along with the other twenty-four repos in the batch.

use starlet_core::{Group, RepoTag, TagSource};

use crate::provider::{AiError, RepoTags, Result};

/// A model that omits its own confidence is treated as unsure rather than
/// certain; a wrong tag at 1.0 outranks correct tags in the UI.
const ASSUMED_CONFIDENCE: f32 = 0.5;

/// Matches the 3-6 tags demanded by [`crate::prompt::TAG_SYSTEM`]. Extra tags
/// past this point are noise, not signal.
const MAX_TAGS_PER_REPO: usize = 6;

/// Parse the tagging response.
///
/// Every returned [`RepoTag`] carries [`TagSource::Ai`].
pub fn parse_tags(raw: &str) -> Result<Vec<RepoTags>> {
    let value = extract_json(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| malformed("expected a JSON object at the top level"))?;
    let repos = object
        .get("repos")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| malformed("response is missing the `repos` array"))?;

    let mut out = Vec::with_capacity(repos.len());
    for entry in repos {
        let full_name = entry
            .get("full_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| malformed("a `repos` entry is missing `full_name`"))?;

        let tags = sanitise_tags(entry.get("tags"));
        if tags.is_empty() {
            // Nothing survived sanitisation; the store has nothing to write, so
            // dropping the repo is the same as never having tagged it.
            continue;
        }
        out.push(RepoTags {
            full_name: full_name.to_string(),
            tags,
        });
    }
    Ok(out)
}

/// Parse the grouping response.
///
/// Every returned [`Group`] carries [`TagSource::Ai`]. Groups whose members all
/// fell away are dropped: an empty sidebar entry is worse than no entry.
pub fn parse_groups(raw: &str) -> Result<Vec<Group>> {
    let value = extract_json(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| malformed("expected a JSON object at the top level"))?;
    let groups = object
        .get("groups")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| malformed("response is missing the `groups` array"))?;

    let mut out = Vec::with_capacity(groups.len());
    for entry in groups {
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| malformed("a `groups` entry is missing `name`"))?;

        let summary = entry
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();

        let mut members: Vec<String> = Vec::new();
        if let Some(list) = entry.get("members").and_then(serde_json::Value::as_array) {
            for member in list {
                let Some(full_name) = member.as_str().map(str::trim).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                if !members.iter().any(|m| m == full_name) {
                    members.push(full_name.to_string());
                }
            }
        }
        if members.is_empty() {
            continue;
        }

        out.push(Group {
            name: name.to_string(),
            summary,
            source: TagSource::Ai,
            members,
        });
    }
    Ok(out)
}

/// Normalise one repo's tag list: lowercase, kebab-case, deduplicated, clamped,
/// and capped. Malformed individual tags are skipped, not fatal.
fn sanitise_tags(raw: Option<&serde_json::Value>) -> Vec<RepoTag> {
    let Some(list) = raw.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    let mut out: Vec<RepoTag> = Vec::with_capacity(list.len().min(MAX_TAGS_PER_REPO));
    for item in list {
        // Models drift between `{"name":..,"confidence":..}` and a bare string.
        // Both are usable; the bare form just loses the confidence signal.
        let (raw_name, confidence) = match item {
            serde_json::Value::String(s) => (s.as_str(), ASSUMED_CONFIDENCE),
            serde_json::Value::Object(_) => {
                let Some(name) = item.get("name").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let confidence = item
                    .get("confidence")
                    .and_then(serde_json::Value::as_f64)
                    .map_or(ASSUMED_CONFIDENCE, |c| c as f32);
                (name, confidence)
            }
            _ => continue,
        };

        let name = normalise_tag(raw_name);
        if name.is_empty() {
            continue;
        }
        if out.iter().any(|t| t.name == name) {
            continue;
        }
        out.push(RepoTag {
            name,
            source: TagSource::Ai,
            // NaN loses every comparison, so `clamp` would panic on it; map it
            // to the unsure default instead.
            confidence: if confidence.is_nan() {
                ASSUMED_CONFIDENCE
            } else {
                confidence.clamp(0.0, 1.0)
            },
        });
        if out.len() == MAX_TAGS_PER_REPO {
            break;
        }
    }
    out
}

/// Lowercase, trim, and collapse internal whitespace to hyphens so that
/// "Static Site Generator" and "static-site-generator" are one tag.
fn normalise_tag(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    if lowered.contains(char::is_whitespace) {
        lowered.split_whitespace().collect::<Vec<_>>().join("-")
    } else {
        lowered
    }
}

fn malformed(reason: impl Into<String>) -> AiError {
    AiError::MalformedResponse(reason.into())
}

/// Find the first balanced, *valid* JSON value in `raw` and decode it.
///
/// This is why the module exists. Prose before and after the payload, a fenced
/// block tagged `json`, or a fence with no language all reduce to one problem:
/// locate the value. Scanning for a balanced brace while tracking string
/// literals and backslash escapes is the only approach that survives a `}`
/// inside a description string, which both `rfind('}')` and any regex get
/// wrong. Candidates that balance but do not decode (a `[like this]` aside in
/// the prose) are skipped, so the search is self-correcting.
fn extract_json(raw: &str) -> Result<serde_json::Value> {
    let bytes = raw.as_bytes();
    let mut saw_opening = false;

    for (start, &b) in bytes.iter().enumerate() {
        if b != b'{' && b != b'[' {
            continue;
        }
        saw_opening = true;
        let Some(end) = balanced_end(bytes, start) else {
            // Unbalanced from here to EOF. A later opener can still balance, so
            // keep scanning rather than declaring truncation now.
            continue;
        };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw[start..=end]) {
            return Ok(value);
        }
    }

    Err(malformed(if saw_opening {
        "response contains no complete JSON value (truncated?)"
    } else {
        "response contains no JSON value"
    }))
}

/// Index of the delimiter closing the one opened at `start`, or `None` if the
/// input ends first. String literals are skipped wholesale so braces and
/// brackets inside them never move the depth counter.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tags: &[RepoTag]) -> Vec<&str> {
        tags.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn parses_a_bare_object() {
        let out = parse_tags(
            r#"{"repos":[{"full_name":"a/b","tags":[{"name":"rust","confidence":0.9}]}]}"#,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].full_name, "a/b");
        assert_eq!(out[0].tags[0].source, TagSource::Ai);
        assert!((out[0].tags[0].confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn parses_a_json_fenced_block() {
        let raw = "```json\n{\"repos\":[{\"full_name\":\"a/b\",\"tags\":[\"rust\"]}]}\n```";
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["rust"]);
    }

    #[test]
    fn parses_an_unlabelled_fenced_block() {
        let raw = "```\n{\"repos\":[{\"full_name\":\"a/b\",\"tags\":[\"rust\"]}]}\n```";
        assert_eq!(parse_tags(raw).unwrap().len(), 1);
    }

    #[test]
    fn parses_json_wrapped_in_prose() {
        let raw = "Sure! Here [is] the result you asked for:\n\
                   {\"repos\":[{\"full_name\":\"a/b\",\"tags\":[\"rust\"]}]}\n\
                   Let me know if you need more.";
        let out = parse_tags(raw).unwrap();
        assert_eq!(out[0].full_name, "a/b");
    }

    #[test]
    fn brace_inside_a_string_literal_does_not_end_the_object() {
        // The naive `rfind('}')`/regex failure case: a closing brace inside a
        // string, followed by real content that must still be parsed.
        let raw = r#"{"repos":[{"full_name":"a/b","tags":[{"name":"templating}","confidence":1.0},{"name":"rust","confidence":0.5}]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["templating}", "rust"]);
    }

    #[test]
    fn escaped_quotes_and_unicode_survive() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":[{"name":"say \"hi\"","confidence":1},{"name":"日本語","confidence":1},{"name":"emoji-🚀","confidence":1}]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["say-\"hi\"", "日本語", "emoji-🚀"]);
    }

    #[test]
    fn rejects_truncated_json() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":[{"name":"rust""#;
        assert!(matches!(
            parse_tags(raw),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn rejects_a_top_level_array() {
        let raw = r#"[{"full_name":"a/b","tags":["rust"]}]"#;
        assert!(matches!(
            parse_tags(raw),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn rejects_an_object_wrapped_in_an_array() {
        let raw = r#"[{"repos":[{"full_name":"a/b","tags":["rust"]}]}]"#;
        assert!(matches!(
            parse_tags(raw),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn rejects_a_missing_repos_key() {
        assert!(matches!(
            parse_tags(r#"{"results":[]}"#),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn rejects_an_entry_without_full_name() {
        let raw = r#"{"repos":[{"tags":[{"name":"rust","confidence":1.0}]}]}"#;
        assert!(matches!(
            parse_tags(raw),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn rejects_an_entry_with_a_blank_full_name() {
        let raw = r#"{"repos":[{"full_name":"   ","tags":["rust"]}]}"#;
        assert!(matches!(
            parse_tags(raw),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn clamps_confidence_into_range() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":[
            {"name":"high","confidence":7.5},
            {"name":"low","confidence":-3},
            {"name":"missing"}
        ]}]}"#;
        let tags = parse_tags(raw).unwrap().remove(0).tags;
        assert_eq!(tags[0].confidence, 1.0);
        assert_eq!(tags[1].confidence, 0.0);
        assert_eq!(tags[2].confidence, ASSUMED_CONFIDENCE);
    }

    #[test]
    fn lowercases_trims_and_kebabs_tag_names() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":["  Rust  ","Static Site Generator"]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["rust", "static-site-generator"]);
    }

    #[test]
    fn drops_empty_tag_names() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":["","   ",{"name":"  "},"rust"]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["rust"]);
    }

    #[test]
    fn drops_duplicate_tags_case_insensitively() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":["rust","Rust"," RUST ","cli"]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["rust", "cli"]);
    }

    #[test]
    fn caps_at_six_tags() {
        let raw = r#"{"repos":[{"full_name":"a/b","tags":["a","b","c","d","e","f","g","h"]}]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(names(&out[0].tags), ["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn drops_repos_with_no_surviving_tags() {
        let raw = r#"{"repos":[
            {"full_name":"a/b","tags":[]},
            {"full_name":"c/d"},
            {"full_name":"e/f","tags":["rust"]}
        ]}"#;
        let out = parse_tags(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].full_name, "e/f");
    }

    #[test]
    fn parses_groups() {
        let raw = r#"Here you go:
        ```json
        {"groups":[{"name":" Rust CLI Tooling ","summary":" Command line tools. ","members":["a/b","a/b","  ","c/d"]}]}
        ```"#;
        let groups = parse_groups(raw).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Rust CLI Tooling");
        assert_eq!(groups[0].summary, "Command line tools.");
        assert_eq!(groups[0].members, ["a/b", "c/d"]);
        assert_eq!(groups[0].source, TagSource::Ai);
    }

    #[test]
    fn rejects_groups_without_the_groups_key_or_a_name() {
        assert!(matches!(
            parse_groups(r#"{"clusters":[]}"#),
            Err(AiError::MalformedResponse(_))
        ));
        assert!(matches!(
            parse_groups(r#"{"groups":[{"summary":"x","members":["a/b"]}]}"#),
            Err(AiError::MalformedResponse(_))
        ));
    }

    #[test]
    fn drops_groups_with_no_members() {
        let raw = r#"{"groups":[{"name":"Empty","summary":"","members":[]},{"name":"Real","members":["a/b"]}]}"#;
        let groups = parse_groups(raw).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Real");
    }

    #[test]
    fn balanced_end_respects_escapes() {
        let s = r#"{"a":"\\"} tail"#;
        let end = balanced_end(s.as_bytes(), 0).unwrap();
        assert_eq!(&s[..=end], r#"{"a":"\\"}"#);
    }
}
