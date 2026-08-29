//! The two application globals: the I/O backend and the sign-in session.
//!
//! Everything else in the interface is an `Entity<T>`. These two are globals
//! because they have exactly one instance per process and every view needs
//! them: a store handle and a Tokio runtime, and who is signed in.
//!
//! # Why a second runtime
//!
//! `sqlx` and `reqwest` require a Tokio reactor; GPUI runs its own executor on
//! the platform's main loop. Rather than pretend one can host the other, the
//! app owns a multi-threaded Tokio runtime for I/O and bridges results back
//! with a oneshot channel that GPUI's executor awaits. Nothing blocks a frame.

use std::sync::Arc;

use gpui::{App, Global};
use starlet_store::Store;
use starlet_sync::{GitHub, TokenStore, Viewer};
use tokio::sync::oneshot;

/// Store plus the Tokio runtime that drives it.
pub struct Backend {
    runtime: Arc<tokio::runtime::Runtime>,
    store: Store,
}

impl Global for Backend {}

impl Backend {
    pub fn new(store: Store, runtime: Arc<tokio::runtime::Runtime>) -> Self {
        Self { runtime, store }
    }

    pub fn global(cx: &App) -> &Backend {
        cx.global::<Backend>()
    }

    pub fn store(&self) -> Store {
        self.store.clone()
    }

    /// Run `future` on the I/O runtime and hand the result back through a
    /// channel a GPUI task can await.
    ///
    /// Dropping the receiver detaches the work rather than cancelling it, which
    /// is correct for writes: a closed sheet must not roll back a tag edit.
    pub fn spawn<F, T>(&self, future: F) -> oneshot::Receiver<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.runtime.spawn(async move {
            let value = future.await;
            let _ = tx.send(value);
        });
        rx
    }

    /// Run a blocking call — the keychain APIs are synchronous — off the UI
    /// thread.
    pub fn spawn_blocking<F, T>(&self, f: F) -> oneshot::Receiver<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.runtime.spawn_blocking(move || {
            let _ = tx.send(f());
        });
        rx
    }
}

/// Where the user is in the sign-in lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStatus {
    /// No token in the keychain, or the user signed out.
    SignedOut,
    /// A device grant is outstanding.
    Pending {
        user_code: String,
        verification_uri: String,
    },
    /// A token is held and the viewer has been (or is being) identified.
    SignedIn,
    /// The last attempt failed; the message is safe to show.
    Failed(String),
}

impl AuthStatus {
    pub fn is_signed_in(&self) -> bool {
        matches!(self, AuthStatus::SignedIn)
    }
}

/// Who is signed in, and the client that speaks for them.
pub struct Session {
    pub status: AuthStatus,
    /// `None` until a token is available. Cloning is cheap and shares the
    /// rate-limit accounting.
    pub github: Option<GitHub>,
    pub viewer: Option<Viewer>,
}

impl Global for Session {}

impl Session {
    /// Build the session from whatever the keychain already holds.
    ///
    /// Reading the keychain at startup is a blocking call, but it is one
    /// round-trip to a local daemon and it decides the first frame's layout.
    pub fn restore() -> Self {
        match TokenStore::load() {
            Some(token) => match GitHub::new(token) {
                Ok(github) => Session {
                    status: AuthStatus::SignedIn,
                    github: Some(github),
                    viewer: None,
                },
                Err(err) => Session {
                    status: AuthStatus::Failed(err.to_string()),
                    github: None,
                    viewer: None,
                },
            },
            None => Session::signed_out(),
        }
    }

    pub fn signed_out() -> Self {
        Session {
            status: AuthStatus::SignedOut,
            github: None,
            viewer: None,
        }
    }

    pub fn global(cx: &App) -> &Session {
        cx.global::<Session>()
    }

    pub fn is_signed_in(cx: &App) -> bool {
        cx.global::<Session>().status.is_signed_in()
    }

    /// The client, if one exists. Sync and detail fetches go through this.
    pub fn client(cx: &App) -> Option<GitHub> {
        cx.global::<Session>().github.clone()
    }
}
