//! The persistence crate's typed error.
//!
//! Wraps the two `sqlx` failure shapes - a query/connection error and a migration
//! error - without flattening either, so the bot can match on what went wrong.

/// Anything the local Postgres store can fail with.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// A query or connection failure from `sqlx`.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// A schema-migration failure (the `migrate` phase).
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A stored text token did not decode to a known enum value - a corrupt or
    /// out-of-date row, surfaced rather than silently coerced to a default.
    #[error("corrupt stored token: {0}")]
    BadToken(String),
}
