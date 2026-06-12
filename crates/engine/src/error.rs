//! The engine's typed error tree.
//!
//! The card read path surfaces a single typed [`Error`] rather than an
//! `anyhow::Result`, so a caller (the bot) can match on what went wrong without
//! downcasting. Each backend keeps its own `thiserror` enum, carried verbatim
//! here via `#[from]`, alongside the shared I/O and JSON failures.
//!
//! Backend calls in the engine surface the *concrete* `<Backend>Error` (not
//! `backends::Error`, which only `Clients::from_env` produces), so each backend
//! arm takes that directly and `?` lifts it with no annotation.

use crate::backends::discord::DiscordError;
use crate::backends::solidarity_tech::SolidarityTechError;

/// The engine's result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything the engine can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // --- backend failures: each backend keeps its own typed error ---
    /// Boxed because `serenity::Error` is large (~130 bytes); keeping it inline
    /// would bloat every `crate::Result` (`clippy::result_large_err`). The manual
    /// `From<DiscordError>` below preserves `?` ergonomics at the call sites.
    #[error(transparent)]
    Discord(Box<DiscordError>),
    #[error(transparent)]
    SolidarityTech(#[from] SolidarityTechError),

    // --- shared infrastructure ---
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

// `DiscordError` is held boxed (see the `Discord` variant), so its `From` is
// written by hand rather than derived with `#[from]`.
impl From<DiscordError> for Error {
    fn from(e: DiscordError) -> Self {
        Error::Discord(Box::new(e))
    }
}
