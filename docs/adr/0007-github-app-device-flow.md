# 7. GitHub App device flow, token in the OS keychain

Status: accepted

## Context

A desktop application cannot keep a client secret. Embedding a browser to run
a redirect-based OAuth flow is a large dependency for one screen.

## Decision

Register Starlet as a **GitHub App** and use the **device flow**. The dialog
shows the user code with a copy button and a button that opens the verification
page; polling runs on the I/O runtime with the interval GitHub returns, honouring
`slow_down`, and stops when the grant expires.

The token goes straight from the poll result into the OS keychain through the
`keyring` crate. It is never written to SQLite, never written to a config file,
never logged, and `GitHub`'s `Debug` implementation omits it.

The client id is read from `STARLET_GITHUB_CLIENT_ID` at build time, with a
runtime environment override so a user can point a self-registered App at their
own build without recompiling.

## Consequences

* A GitHub App's permissions are scoped by installation rather than by classic
  OAuth scopes: `Starring: read` plus `read:user`.
* A locked or unavailable keychain reads as "signed out" rather than as an
  error. That is the only useful response — the app then behaves exactly as it
  does before a first sign-in.
* BYOK provider keys use the same keychain service with a per-provider account
  (`ai:openai`, `ai:anthropic`), so switching providers in Settings swaps the
  key rather than writing one provider's key under another's name.
* Without a client id the app runs read-only against whatever is already in the
  local database, and sign-in reports that the id is missing instead of failing
  obscurely.
