//! Discord backend: read guild members and set their status roles via the
//! Discord REST API.
//!
//! Provides [`DiscordClient`] (with a `mockall` mock under `#[cfg(test)]`) and
//! the live [`DiscordHttp`], built on `serenity::http::Http` only - no gateway,
//! no cache, no event handler. The gateway lives in the bot binary alone; this
//! shared layer is a pure REST write path, so guild state is only ever mutated
//! here and never from a second place. Every member ends up in exactly one of the
//! three [`Role`]s; this module owns their names, env-var overrides, and priority
//! order, and resolves the concrete role ids once at construction.
//!
//! The `live-discord` cargo feature gates the integration tests that hit a real
//! guild so the default `cargo test` stays offline.

mod client;
mod error;
mod http;
mod roles;

pub use client::DiscordClient;
pub use error::DiscordError;
pub use http::{DiscordHttp, resolve_managed_roles};
pub use roles::{ManagedRole, MemberRoles, Role};

#[cfg(feature = "mock")]
pub use client::MockDiscordClient;
