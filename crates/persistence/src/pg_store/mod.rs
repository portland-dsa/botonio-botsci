//! The Postgres-backed implementation of the engine's `store` traits.
//!
//! [`PgStore`] is the production store: it reads and writes the bot's tables over the
//! bot-owned pool, so the engine stays database-free - it knows only the traits and the flat
//! value types, while the SQL, the row shapes, and the value encodings all live here.
//!
//! This hub owns the [`PgStore`] handle (the pool and its lifecycle/health methods); the
//! per-trait impls are split by concern across the leaf modules, each reaching the hub's
//! private `pool` field:
//!
//! - [`member`] - the `member_cache` roster: the row shape, the cache-local enum tokens, and
//!   the [`MemberStore`](engine::store::MemberStore)/[`RosterWrite`](engine::store::RosterWrite)/[`IdentityWrite`](engine::store::IdentityWrite) impls.
//! - [`config`] - the `guild_config` row and the channel-permission snapshots.
//! - [`moderation`] - the manual override, the grace override, and the bulk-verify session.
//! - [`reminders`] - the dues-reminder cycle state, opt-out, and message templates.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::PersistenceError;

mod config;
mod member;
mod moderation;
mod reminders;

/// The Postgres-backed store: the production implementation of the engine's `store` traits
/// over the bot's tables.
///
/// Built from the single pool the bot owns ([`new`](PgStore::new)); reads and writes
/// authenticate as the DML-only runtime role. The [`Role`](domain::Role) a member is granted
/// is always derived from the stored `standing`, never persisted, so the role decision has
/// exactly one source.
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Build over an existing pool. The bot owns the one pool and shares it; the
    /// store holds a clone (`PgPool` is an `Arc` internally, so this is cheap). Used by
    /// the conformance suite, which is handed a pool by the test harness.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect the runtime pool from `dsn` and wrap it in a store - the production
    /// constructor, so every sqlx detail (the pool tuning included) stays in this crate.
    ///
    /// Keeps a couple of connections warm so a card read or the liveness probe never pays
    /// connection-setup latency, and gives headroom over sqlx's default of 10 so a burst
    /// of reads plus the refresh transaction can't starve the bounded liveness probe (a
    /// starved probe would skip its watchdog ping and trip a restart).
    pub async fn connect(dsn: &str) -> Result<Self, PersistenceError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .min_connections(2)
            .connect(dsn)
            .await?;
        Ok(Self { pool })
    }

    /// A clone of the underlying pool, for a sibling capability (the [`Auditor`](crate::Auditor))
    /// that shares the one bot-owned connection pool. `PgPool` is internally an
    /// `Arc`, so the clone is cheap.
    pub fn pool_handle(&self) -> PgPool {
        self.pool.clone()
    }

    /// A cheap liveness probe: confirm the runtime role can still read `member_cache`. The
    /// bot's watchdog calls this so the front-end never issues raw SQL of its own. It probes
    /// the actual table rather than a bare `SELECT 1` so the check fails closed on a grant
    /// or schema regression that would otherwise leave the watchdog green while every card
    /// lookup fails. An empty table still answers (`fetch_optional` -> `None`).
    pub async fn ping(&self) -> Result<(), PersistenceError> {
        sqlx::query!("SELECT 1 AS one FROM member_cache LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(())
    }

    /// Whether the cache holds any member rows. The bot checks this at startup: if a fresh
    /// roster could not be loaded (a failed or empty sweep) and the durable cache is also
    /// empty, there is nothing to serve and coming up would be useless.
    pub async fn is_populated(&self) -> Result<bool, PersistenceError> {
        let populated =
            sqlx::query_scalar!(r#"SELECT EXISTS(SELECT 1 FROM member_cache) AS "exists!""#)
                .fetch_one(&self.pool)
                .await?;
        Ok(populated)
    }
}
