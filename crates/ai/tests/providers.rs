//! Wire-level tests for the three backends.
//!
//! Every provider is exercised against a mock server through the `with_base_url`
//! seam: correct path, correct auth, correct body shape, a good response, the
//! one retry, and the failure that follows a second bad response.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use starlet_ai::{AiError, AiProvider, Anthropic, Ollama, OpenAi};
use starlet_core::{RepoSummary, TagSource};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A well-formed tagging reply from the model.
const GOOD: &str = r#"{"repos":[{"full_name":"a/b","tags":[{"name":"Rust","confidence":1.4},{"name":"cli","confidence":0.7}]}]}"#;

/// The apology models produce when they ignore the format instruction.
const GARBAGE: &str = "Sure! I can help with that. Which repositories did you mean?";

/// Replies with a different body per call so a retry can be scripted without
/// depending on wiremock's mock-ordering semantics.
struct Sequence {
    bodies: Vec<String>,
    calls: Arc<AtomicUsize>,
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = self.bodies[nth.min(self.bodies.len() - 1)].clone();
        ResponseTemplate::new(200).set_body_string(body)
    }
}

/// Mount `bodies` in order at `route`; returns the call counter.
async fn mount(server: &MockServer, route: &str, bodies: Vec<Value>) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(Sequence {
            bodies: bodies.iter().map(Value::to_string).collect(),
            calls: Arc::clone(&calls),
        })
        .mount(server)
        .await;
    calls
}

fn batch() -> Vec<RepoSummary> {
    vec![RepoSummary {
        full_name: "a/b".into(),
        description: Some("a thing".into()),
        topics: vec!["cli".into()],
        primary_language: Some("Rust".into()),
    }]
}

fn openai_envelope(content: &str) -> Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
}

fn anthropic_envelope(content: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": content }] })
}

fn ollama_envelope(content: &str) -> Value {
    json!({ "message": { "role": "assistant", "content": content } })
}

/// The parsed shape both good responses must produce, proving sanitisation ran.
fn assert_good(tagged: &[starlet_ai::RepoTags]) {
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].full_name, "a/b");
    let names: Vec<&str> = tagged[0].tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["rust", "cli"]);
    assert_eq!(tagged[0].tags[0].confidence, 1.0, "confidence is clamped");
    assert!(tagged[0].tags.iter().all(|t| t.source == TagSource::Ai));
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("request recording is on")
        .iter()
        .map(|r| r.body_json::<Value>().expect("request body is json"))
        .collect()
}

// ---------------------------------------------------------------- openai

#[tokio::test]
async fn openai_sends_the_documented_request_and_parses_the_reply() {
    let server = MockServer::start().await;
    mount(&server, "/v1/chat/completions", vec![openai_envelope(GOOD)]).await;

    let provider = OpenAi::new("sk-test-key", "").with_base_url(server.uri());
    assert_eq!(provider.id(), "openai");
    assert_eq!(provider.model(), "gpt-4o-mini");

    assert_good(&provider.tag(&batch()).await.expect("tagging succeeds"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/v1/chat/completions");
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer sk-test-key"
    );

    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "gpt-4o-mini");
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["response_format"]["type"], "json_object");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("a/b")
    );
}

#[tokio::test]
async fn openai_recovers_through_exactly_one_retry() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/v1/chat/completions",
        vec![openai_envelope(GARBAGE), openai_envelope(GOOD)],
    )
    .await;

    let provider = OpenAi::new("sk-test-key", "").with_base_url(server.uri());
    assert_good(&provider.tag(&batch()).await.expect("retry succeeds"));

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2, "exactly one retry");
    let retried = sent[1]["messages"][1]["content"].as_str().unwrap();
    assert!(
        retried.contains("could not be parsed"),
        "the retry restates the format demand"
    );
}

#[tokio::test]
async fn openai_gives_up_after_the_second_bad_reply() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/v1/chat/completions",
        vec![openai_envelope(GARBAGE)],
    )
    .await;

    let error = OpenAi::new("sk-test-key", "")
        .with_base_url(server.uri())
        .tag(&batch())
        .await
        .expect_err("two bad replies is an error");
    assert!(matches!(error, AiError::MalformedResponse(_)));
    assert_eq!(bodies(&server).await.len(), 2, "no third attempt");
}

#[tokio::test]
async fn openai_surfaces_an_error_status_without_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key sk-test-key"}"#),
        )
        .mount(&server)
        .await;

    let error = OpenAi::new("sk-test-key", "")
        .with_base_url(server.uri())
        .tag(&batch())
        .await
        .expect_err("401 is an error");

    match error {
        AiError::Status { code, message } => {
            assert_eq!(code, 401);
            assert!(!message.contains("sk-test-key"), "key must be redacted");
            assert!(message.contains("[redacted]"));
        }
        other => panic!("expected a status error, got {other:?}"),
    }
    assert_eq!(bodies(&server).await.len(), 1, "http failures do not retry");
}

#[tokio::test]
async fn openai_without_a_key_never_reaches_the_network() {
    let server = MockServer::start().await;
    let error = OpenAi::new("", "")
        .with_base_url(server.uri())
        .tag(&batch())
        .await
        .expect_err("a keyless hosted provider cannot run");
    assert!(matches!(error, AiError::MissingKey));
    assert!(server.received_requests().await.unwrap().is_empty());
}

// ------------------------------------------------------------- anthropic

#[tokio::test]
async fn anthropic_sends_the_documented_request_and_parses_the_reply() {
    let server = MockServer::start().await;
    mount(&server, "/v1/messages", vec![anthropic_envelope(GOOD)]).await;

    let provider = Anthropic::new("sk-ant-test", "").with_base_url(server.uri());
    assert_eq!(provider.id(), "anthropic");
    assert_eq!(provider.model(), "claude-3-5-haiku-latest");

    assert_good(&provider.tag(&batch()).await.expect("tagging succeeds"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.path(), "/v1/messages");
    assert_eq!(requests[0].headers.get("x-api-key").unwrap(), "sk-ant-test");
    assert_eq!(
        requests[0].headers.get("anthropic-version").unwrap(),
        "2023-06-01"
    );
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "anthropic authenticates with x-api-key only"
    );

    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "claude-3-5-haiku-latest");
    assert!(body["max_tokens"].as_u64().unwrap() > 0);
    assert!(
        body["system"].as_str().unwrap().contains("kebab-case"),
        "the system prompt travels outside the message list"
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn anthropic_joins_multiple_text_blocks() {
    let server = MockServer::start().await;
    let split = json!({
        "content": [
            { "type": "thinking", "thinking": "hmm" },
            { "type": "text", "text": r#"{"repos":[{"full_name":"a/b","#},
            { "type": "text", "text": r#""tags":[{"name":"rust","confidence":1.0}]}]}"# },
        ]
    });
    mount(&server, "/v1/messages", vec![split]).await;

    let tagged = Anthropic::new("sk-ant-test", "")
        .with_base_url(server.uri())
        .tag(&batch())
        .await
        .expect("split replies rejoin");
    assert_eq!(tagged[0].tags[0].name, "rust");
}

#[tokio::test]
async fn anthropic_recovers_through_exactly_one_retry_then_fails() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/v1/messages",
        vec![anthropic_envelope(GARBAGE), anthropic_envelope(GOOD)],
    )
    .await;
    let provider = Anthropic::new("sk-ant-test", "").with_base_url(server.uri());
    assert_good(&provider.tag(&batch()).await.expect("retry succeeds"));
    assert_eq!(bodies(&server).await.len(), 2);

    let bad = MockServer::start().await;
    mount(&bad, "/v1/messages", vec![anthropic_envelope(GARBAGE)]).await;
    let error = Anthropic::new("sk-ant-test", "")
        .with_base_url(bad.uri())
        .tag(&batch())
        .await
        .expect_err("two bad replies is an error");
    assert!(matches!(error, AiError::MalformedResponse(_)));
    assert_eq!(bodies(&bad).await.len(), 2, "no third attempt");
}

// ---------------------------------------------------------------- ollama

#[tokio::test]
async fn ollama_sends_the_documented_request_and_parses_the_reply() {
    let server = MockServer::start().await;
    mount(&server, "/api/chat", vec![ollama_envelope(GOOD)]).await;

    let provider = Ollama::new("", "llama3.2").with_base_url(server.uri());
    assert_eq!(provider.id(), "ollama");
    assert_eq!(provider.model(), "llama3.2");
    assert_eq!(provider.estimate(500).usd, 0.0, "local runs are free");

    assert_good(&provider.tag(&batch()).await.expect("tagging succeeds"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.path(), "/api/chat");
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "a local daemon is not sent a bearer token"
    );

    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "llama3.2");
    assert_eq!(body["stream"], false);
    assert_eq!(body["format"], "json");
    assert_eq!(body["messages"][0]["role"], "system");
}

#[tokio::test]
async fn ollama_recovers_through_exactly_one_retry_then_fails() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/api/chat",
        vec![ollama_envelope(GARBAGE), ollama_envelope(GOOD)],
    )
    .await;
    let provider = Ollama::new("", "").with_base_url(server.uri());
    assert_good(&provider.tag(&batch()).await.expect("retry succeeds"));
    assert_eq!(bodies(&server).await.len(), 2);

    let bad = MockServer::start().await;
    mount(&bad, "/api/chat", vec![ollama_envelope(GARBAGE)]).await;
    let error = Ollama::new("", "")
        .with_base_url(bad.uri())
        .tag(&batch())
        .await
        .expect_err("two bad replies is an error");
    assert!(matches!(error, AiError::MalformedResponse(_)));
    assert_eq!(bodies(&bad).await.len(), 2, "no third attempt");
}

#[tokio::test]
async fn ollama_forwards_a_token_when_one_is_configured() {
    let server = MockServer::start().await;
    mount(&server, "/api/chat", vec![ollama_envelope(GOOD)]).await;

    Ollama::new("proxy-token", "")
        .with_base_url(server.uri())
        .tag(&batch())
        .await
        .expect("tagging succeeds");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer proxy-token"
    );
}

// ------------------------------------------------------------- grouping

#[tokio::test]
async fn grouping_round_trips_through_the_same_seam() {
    let server = MockServer::start().await;
    let reply = r#"Here you go:
```json
{"groups":[{"name":"Rust CLI Tooling","summary":"Command line tools.","members":["a/b","c/d"]}]}
```"#;
    mount(
        &server,
        "/v1/chat/completions",
        vec![openai_envelope(reply)],
    )
    .await;

    let groups = OpenAi::new("sk-test-key", "")
        .with_base_url(server.uri())
        .group(&[starlet_ai::RepoWithTags {
            full_name: "a/b".into(),
            description: Some("a thing".into()),
            tags: vec!["rust".into()],
        }])
        .await
        .expect("grouping succeeds");

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Rust CLI Tooling");
    assert_eq!(groups[0].members, ["a/b", "c/d"]);
    assert_eq!(groups[0].source, TagSource::Ai);
}
