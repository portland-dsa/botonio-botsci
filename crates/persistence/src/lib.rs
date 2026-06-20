#![forbid(unsafe_code)]
//! The bot's local persistence: a Postgres-backed [`MemberStore`] / [`RosterWrite`]
//! ([`PgStore`]), the append-only audit writer ([`Auditor`]), plus the embedded
//! schema migrations ([`run_migrations`]). Keeps every sqlx dependency out of
//! `engine`, which stays the network- and database-free card-read path.
//!
//! [`MemberStore`]: engine::store::MemberStore
//! [`RosterWrite`]: engine::store::RosterWrite

mod auditor;
mod error;
mod migrate;
mod pg_store;

pub use auditor::Auditor;
pub use error::PersistenceError;
pub use migrate::{connect_and_migrate, run_migrations};
pub use pg_store::PgStore;
