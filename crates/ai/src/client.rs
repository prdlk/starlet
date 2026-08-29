//! HTTP plumbing shared by the three providers: key hygiene, status handling,
//! and the single-retry loop.

use std::fmt;

use crate::provider::{AiError, Result};

/// An API key that cannot leak through `Debug`.
///
/// Providers derive `Debug` for their own diagnostics; wrapping the key here
/// means nobody has to remember to hand-write that impl, and a future field
/// added to a provider struct cannot silently start printing the secret.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct ApiKey(String);

impl ApiKey {
    pub(crate) fn new(raw: impl Into<String>) -> Self {
        Self(raw.into().trim().to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Fail before building a request so a misconfigured provider costs no
    /// round trip and produces no chance of a leak.
    pub(crate) fn require(&self) -> Result<&str> {
        if self.0.is_empty() {
            return Err(AiError::MissingKey);
        }
        Ok(&self.0)
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_empty() {
            "ApiKey(unset)"
        } else {
            "ApiKey(redacted)"
        })
    }
}

/// Error bodies are user-visible; a provider that echoes the request back would
/// otherwise paste the key into the UI and the logs.
const MAX_ERROR_BODY: usize = 512;

/// Turn a response into its body text, or into [`AiError::Status`].
///
/// The key is scrubbed from the error body because some gateways echo the
/// `Authorization` header back in their 401 payload.
pub(crate) async fn body_or_status(
    response: reqwest::Response,
    key: &ApiKey,
) -> Result<String> {
    let status = response.status();
    let body = response.text().await?;
    if status.is_success() {
        return Ok(body);
    }
    Err(AiError::Status {
        code: status.as_u16(),
        message: truncate(&redact(&body, key), MAX_ERROR_BODY),
    })
}

fn redact(body: &str, key: &ApiKey) -> String {
    if key.is_empty() {
        return body.to_string();
    }
    body.replace(key.as_str(), "[redacted]")
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        None => trimmed.to_string(),
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
    }
}

/// Send, parse, and on a *parse* failure send exactly once more.
///
/// `send` receives `true` on the retry so the provider can append
/// [`crate::prompt::RETRY_SUFFIX`]. Transport and HTTP-status failures
/// propagate immediately: re-sending a request that a 429 or a 500 rejected
/// would be a retry policy, and that belongs to the caller, not here.
pub(crate) async fn parse_with_one_retry<T, F, Fut>(
    provider: &'static str,
    mut send: F,
    parse: fn(&str) -> Result<T>,
) -> Result<T>
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let first = send(false).await?;
    match parse(&first) {
        Ok(parsed) => Ok(parsed),
        Err(first_error) => {
            tracing::warn!(
                provider,
                error = %first_error,
                "model output did not parse; retrying once with a stricter instruction"
            );
            let second = send(true).await?;
            parse(&second).inspect_err(|second_error| {
                tracing::warn!(provider, error = %second_error, "retry also failed to parse");
            })
        }
    }
}

/// Pull the model's text out of a decoded response envelope.
///
/// An envelope that does not match the provider's documented shape is a hard
/// error rather than a retry: the model never saw our prompt, so asking it
/// again more firmly cannot help.
pub(crate) fn envelope_field(
    value: &serde_json::Value,
    pointer: &str,
    provider: &'static str,
) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            AiError::MalformedResponse(format!(
                "{provider} response has no text at `{pointer}`"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_key() {
        let key = ApiKey::new("sk-super-secret");
        assert_eq!(format!("{key:?}"), "ApiKey(redacted)");
        assert_eq!(format!("{:?}", ApiKey::default()), "ApiKey(unset)");
    }

    #[test]
    fn redaction_scrubs_an_echoed_key() {
        let key = ApiKey::new("sk-super-secret");
        let body = r#"{"error":"invalid key sk-super-secret"}"#;
        let scrubbed = redact(body, &key);
        assert!(!scrubbed.contains("sk-super-secret"));
        assert!(scrubbed.contains("[redacted]"));
    }

    #[test]
    fn truncation_is_char_safe() {
        let body = "é".repeat(600);
        let cut = truncate(&body, MAX_ERROR_BODY);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), MAX_ERROR_BODY + 1);
    }

    #[test]
    fn missing_key_is_caught_before_any_request() {
        assert!(matches!(
            ApiKey::new("   ").require(),
            Err(AiError::MissingKey)
        ));
    }
}
