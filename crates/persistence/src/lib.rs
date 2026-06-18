#![forbid(unsafe_code)]
//! The bot's local persistence: a Postgres-backed [`MemberStore`] / [`RosterWrite`]
//! ([`PgStore`]) plus the embedded schema migrations ([`run_migrations`]). Keeps
//! every sqlx dependency out of `engine`, which stays the network- and
//! database-free card-read path.
//!
//! [`MemberStore`]: engine::store::MemberStore
//! [`RosterWrite`]: engine::store::RosterWrite

mod error;
mod migrate;
mod pg_store;

pub use error::PersistenceError;
pub use migrate::{connect_and_migrate, run_migrations};
pub use pg_store::PgStore;
