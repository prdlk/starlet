//! GitHub authentication and the star sync engine.
//!
//! Everything that talks to github.com lives here. The layers above see only
//! `starlet_core` types and a stream of [`SyncEvent`].

pub mod auth;
pub mod client;
pub mod engine;
pub mod wire;

pub use auth::{DeviceFlow, DeviceGrant, PollOutcome, ProviderKeyStore, TokenStore, client_id};
pub use client::{Conditional, GitHub, RateLimit, StarredPage};
pub use engine::{SyncEngine, SyncEvent, SyncMode, SyncPhase, SyncSummary};
pub use wire::Viewer;

use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Auth(String),
    #[error("keychain unavailable: {0}")]
    Keychain(String),
    #[error("rate limited by GitHub")]
    RateLimited {
        retry_after_secs: Option<u64>,
        reset_at: Option<DateTime<Utc>>,
    },
    #[error("not found")]
    NotFound,
    #[error("GitHub returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error("database: {0}")]
    Store(#[from] starlet_store::StoreError),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T, E = SyncError> = std::result::Result<T, E>;

impl SyncError {
    /// True when signing in again is the fix. The UI uses this to decide
    /// between "retry" and "sign in".
    pub fn needs_reauth(&self) -> bool {
        matches!(self, SyncError::Auth(_))
    }
}
