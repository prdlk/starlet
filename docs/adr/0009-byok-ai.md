# 9. BYOK AI: one provider trait, strict JSON, one retry

Status: accepted

## Context

Tagging and grouping are useful but optional, cost the user money, and depend
on a model returning parseable JSON — which models intermittently fail to do,
usually by wrapping it in a code fence or prefacing it with a sentence.

## Decision

**A trait, three implementations.** `AiProvider` has `tag`, `group`,
`estimate`, `id`, and `model`. OpenAI, Anthropic, and Ollama implement it. Each
takes a base URL so it can be driven by `wiremock` in tests.

**The user supplies the key.** Starlet ships no key and has no hosted
component. Ollama needs no key at all and reports zero cost.

**Parse strictly, sanitise generously.** The parser scans for the first `{` and
finds its balanced closing brace while respecting string literals and escapes —
not a regex, and not `rfind('}')`, which fails the moment a description
contains a brace. Structural problems (truncated JSON, wrong root type, a
missing key) are errors. Value problems are repaired: confidences are clamped,
tag names lowercased and de-duplicated, more than six tags truncated, repos with
no surviving tags dropped.

**Exactly one retry, only on a parse failure**, re-issuing the request with an
appended instruction to reply with JSON only. HTTP failures are not retried.

**Never on launch.** The run starts when the user clicks Analyze, after seeing
a cost estimate, and can be stopped between batches.

## Consequences

* Batches of 25 repositories are written to the store as they arrive, so a
  stopped run keeps the work it already did.
* One failing batch does not abort the run; it is logged and the run continues.
  Total failure of every batch is an error.
* The cost figure is an upper-bound estimate from a per-model price table and a
  characters-divided-by-four token approximation. It is labelled as an estimate
  in the dialog, not as a quote.
* AI tags render in muted text with a keep affordance. Promoting one makes it a
  user tag, which no later run can overwrite.
