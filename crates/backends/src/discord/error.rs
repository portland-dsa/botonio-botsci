//! The Discord backend's error type.

/// Everything that can go wrong in a [`DiscordClient`](super::DiscordClient) call.
#[derive(Debug, thiserror::Error)]
pub enum DiscordError {
    /// A serenity HTTP or model error - typically a network failure or a
    /// non-success Discord API response.
    #[error("serenity error: {0}")]
    Serenity(#[from] serenity::Error),
    /// A required environment variable was absent at startup; the value names it.
    #[error("missing env var: {0}")]
    MissingEnv(&'static str),
    /// An environment variable was set but its value is not a `u64` snowflake.
    #[error("env var {0} is not a valid u64: {1}")]
    BadEnv(&'static str, std::num::ParseIntError),
    /// No guild role matched [`Role::as_str`](super::Role::as_str) and no
    /// `env_var` override was set; check the guild's role names or supply the
    /// override.
    #[error("role {0:?} not found by name on guild")]
    RoleNotFound(&'static str),
    /// A Manual Override marker write was attempted but no override role is configured.
    #[error("no Manual Override role is configured")]
    OverrideRoleUnconfigured,
}
