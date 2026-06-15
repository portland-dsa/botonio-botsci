//! The Postgres-backed [`MemberStore`] / [`RosterWrite`].
//!
//! [`PgStore`] is the production implementation of the two engine cache
//! capabilities, reading and replacing rows in the `member_cache` table over the
//! bot-owned pool. The engine stays database-free: it knows only the traits and the
//! flat [`MemberRecord`]; the SQL, the row shape, and the value encoding all live
//! here.
//!
//! ## Why cache-local tokens
//!
//! The small enums ([`MigsStatus`], [`MembershipType`], [`DuesStatus`]) round-trip
//! through stable text tokens chosen *here* rather than reusing a backend's wire
//! spelling. That keeps the cache decoupled from Solidarity Tech's API: a change to
//! a wire value cannot silently reinterpret already-stored rows. [`DuesStatus`] is
//! the clearest case - it collapses eight wire values into four, so its wire
//! `TryFrom` cannot read back what we store; this module owns the lossless inverse.
//! [`MembershipType`] is owned both ways here too - its decode is a local match, not the
//! wire `TryFrom` - so a wire-spelling change can never reinterpret a stored row.
//! [`MigsStatus`] is the one deliberate reuse: its tokens are domain's own canonical
//! `as_str`/`decode` strings (the source of truth, not a wire spelling).

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use domain::MigsStatus;
use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::store::{MemberRecord, MemberStore, RosterWrite};
use engine::util::{DiscordHandle, DiscordUserId, Email};

use crate::PersistenceError;

/// The Postgres membership cache: a [`MemberStore`] + [`RosterWrite`] over one
/// `member_cache` table.
///
/// Built from the single pool the bot owns ([`new`](PgStore::new)); reads authenticate
/// as the DML-only runtime role. The [`Role`](domain::Role) a member is granted is
/// always derived from the stored `standing`, never persisted, so the role decision
/// has exactly one source.
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

// --- cache-local enum tokens (stable; independent of the ST wire spellings) ---

/// Encode a [`MigsStatus`] to its stored token. The inverse is [`migs_from_token`];
/// reuses the canonical [`as_str`](MigsStatus::as_str) spelling that
/// [`decode`](MigsStatus::decode) reads back.
fn migs_to_token(s: MigsStatus) -> &'static str {
    s.as_str()
}

/// Decode a stored `standing` token, dropping anything unrecognized to `None` (a
/// roster sweep is lenient; a bad cached value must not poison a read).
fn migs_from_token(t: &str) -> Option<MigsStatus> {
    MigsStatus::decode(Some(t)).ok()
}

/// Encode a [`MembershipType`] to its stored token. The cache owns these spellings (and
/// their inverse [`mtype_from_token`]) outright, so a change to Solidarity Tech's wire
/// spelling can never reinterpret an already-stored row.
fn mtype_to_token(t: MembershipType) -> &'static str {
    match t {
        MembershipType::Monthly => "monthly",
        MembershipType::Yearly => "yearly",
        MembershipType::OneTime => "one-time",
        MembershipType::IncomeBased => "income-based",
    }
}

/// Decode a stored `membership_type` token, dropping anything unrecognized to `None`. A
/// local match (the lossless inverse of [`mtype_to_token`]), deliberately *not* the wire
/// [`TryFrom`](MembershipType): a cache read must not depend on the API's spelling.
fn mtype_from_token(t: &str) -> Option<MembershipType> {
    match t {
        "monthly" => Some(MembershipType::Monthly),
        "yearly" => Some(MembershipType::Yearly),
        "one-time" => Some(MembershipType::OneTime),
        "income-based" => Some(MembershipType::IncomeBased),
        _ => None,
    }
}

/// Encode a [`DuesStatus`] to its stored token. These four tokens are the cache's
/// own, *not* the eight Solidarity Tech wire values: [`DuesStatus`] is lossy, so this
/// module owns both directions ([`dues_from_token`] is the inverse its wire
/// `TryFrom` cannot provide).
fn dues_to_token(d: DuesStatus) -> &'static str {
    match d {
        DuesStatus::Active => "active",
        DuesStatus::Never => "never",
        DuesStatus::Overdue => "overdue",
        DuesStatus::Cancelled => "cancelled",
    }
}

/// Decode a stored dues-status token, dropping anything unrecognized to `None`.
fn dues_from_token(t: &str) -> Option<DuesStatus> {
    match t {
        "active" => Some(DuesStatus::Active),
        "never" => Some(DuesStatus::Never),
        "overdue" => Some(DuesStatus::Overdue),
        "cancelled" => Some(DuesStatus::Cancelled),
        _ => None,
    }
}

/// One `member_cache` row, exactly as `sqlx` reads it: every column a primitive,
/// snowflakes as `i64` (a real Discord id is well under `2^63`). The mapping back to
/// the engine's [`MemberRecord`] is [`From<MemberCacheRow>`](MemberRecord).
struct MemberCacheRow {
    discord_user_id: i64,
    discord_handle: Option<String>,
    email: String,
    full_name: Option<String>,
    standing: Option<String>,
    join_date: Option<NaiveDate>,
    expires: Option<NaiveDate>,
    membership_type: Option<String>,
    monthly_dues: Option<String>,
    yearly_dues: Option<String>,
}

impl From<MemberCacheRow> for MemberRecord {
    fn from(r: MemberCacheRow) -> Self {
        MemberRecord {
            discord_user_id: Some(DiscordUserId(r.discord_user_id as u64)),
            discord_handle: r.discord_handle.map(DiscordHandle),
            email: Email(r.email),
            full_name: r.full_name,
            standing: r.standing.as_deref().and_then(migs_from_token),
            join_date: r.join_date,
            expires: r.expires,
            membership_type: r.membership_type.as_deref().and_then(mtype_from_token),
            monthly_dues: r.monthly_dues.as_deref().and_then(dues_from_token),
            yearly_dues: r.yearly_dues.as_deref().and_then(dues_from_token),
        }
    }
}

#[async_trait]
impl MemberStore for PgStore {
    type Error = PersistenceError;

    /// Look up one member by their immutable Discord snowflake. A database failure
    /// surfaces as [`PersistenceError`] rather than a silent "not found", so a
    /// transient outage can never be mistaken for an absent record.
    async fn by_discord_id(
        &self,
        id: DiscordUserId,
    ) -> Result<Option<MemberRecord>, PersistenceError> {
        let row = sqlx::query_as!(
            MemberCacheRow,
            r#"SELECT discord_user_id, discord_handle, email, full_name, standing,
                      join_date, expires, membership_type, monthly_dues, yearly_dues
               FROM member_cache WHERE discord_user_id = $1"#,
            id.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(MemberRecord::from))
    }
}

#[async_trait]
impl RosterWrite for PgStore {
    type Error = PersistenceError;

    /// Atomically replace the whole cache with `records`: a `DELETE` of every row and a
    /// re-insert inside one transaction, so a reader sees either the old roster in full
    /// or the new one in full, never a partial sweep.
    ///
    /// An empty roster (no storable records) is a no-op that preserves the current cache
    /// rather than deleting it: a sweep resolving to zero members is treated as an
    /// upstream glitch, not a real membership of zero.
    ///
    /// Records are deduplicated by `discord_user_id`, **first-wins**, exactly mirroring
    /// the in-memory [`Index`](engine::store::Index) (`entry().or_insert`); records with
    /// no id are dropped (they can never be looked up). This is done in Rust rather than
    /// letting a duplicate id reach the `PRIMARY KEY` and abort the transaction: two
    /// Solidarity Tech members sharing a Discord id must not fail the
    /// whole roster load and restart-loop the bot - the lenient-sweep invariant. It also
    /// keeps the two stores genuinely equivalent.
    ///
    /// The kept rows go in as a single `UNNEST` batch - one round-trip rather than one
    /// per member - which also keeps the write transaction (and its locks) short.
    ///
    /// `DELETE`, not `TRUNCATE`, on purpose: the runtime role is granted only DML
    /// (`SELECT`/`INSERT`/`UPDATE`/`DELETE`) and holds no `TRUNCATE` privilege, and a
    /// row-level `DELETE` lets card reads continue under MVCC during a refresh instead
    /// of blocking on the `ACCESS EXCLUSIVE` lock `TRUNCATE` would take.
    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), PersistenceError> {
        // Encode the deduplicated records into stored rows. Dedup is by discord_user_id,
        // first-wins (mirrors Index); a record with no id is dropped (it can never be
        // looked up). Each row's fields are encoded together in one struct literal, so the
        // column mapping lives in one place rather than spread across parallel pushes.
        let mut seen = std::collections::HashSet::new();
        let mut rows: Vec<MemberCacheRow> = Vec::with_capacity(records.len());
        for r in records {
            let Some(id) = r.discord_user_id else {
                continue;
            };
            if !seen.insert(id.0) {
                continue; // first-wins, like Index::insert
            }
            rows.push(MemberCacheRow {
                discord_user_id: id.0 as i64,
                discord_handle: r.discord_handle.map(|h| h.0),
                email: r.email.0,
                full_name: r.full_name,
                standing: r.standing.map(|s| migs_to_token(s).to_owned()),
                join_date: r.join_date,
                expires: r.expires,
                membership_type: r.membership_type.map(|t| mtype_to_token(t).to_owned()),
                monthly_dues: r.monthly_dues.map(|d| dues_to_token(d).to_owned()),
                yearly_dues: r.yearly_dues.map(|d| dues_to_token(d).to_owned()),
            });
        }

        // Never wipe a populated cache for an empty roster: a sweep that resolves to zero
        // storable members is almost always an upstream glitch, not a real membership of
        // zero. Skip the destructive DELETE and keep the last good rows. The bot's refresh
        // path also screens an empty sweep; this is the store-level
        // backstop that holds for any caller, and mirrors InMemoryStore.
        if rows.is_empty() {
            return Ok(());
        }

        // Transpose the rows into one owned array per column for the single-round-trip
        // UNNEST insert (sqlx binds each column as its own array). Consumes `rows` so the
        // strings move rather than clone.
        let cap = rows.len();
        let mut ids: Vec<i64> = Vec::with_capacity(cap);
        let mut handles: Vec<Option<String>> = Vec::with_capacity(cap);
        let mut emails: Vec<String> = Vec::with_capacity(cap);
        let mut full_names: Vec<Option<String>> = Vec::with_capacity(cap);
        let mut standings: Vec<Option<String>> = Vec::with_capacity(cap);
        let mut join_dates: Vec<Option<NaiveDate>> = Vec::with_capacity(cap);
        let mut expiries: Vec<Option<NaiveDate>> = Vec::with_capacity(cap);
        let mut membership_types: Vec<Option<String>> = Vec::with_capacity(cap);
        let mut monthly_dues: Vec<Option<String>> = Vec::with_capacity(cap);
        let mut yearly_dues: Vec<Option<String>> = Vec::with_capacity(cap);
        for row in rows {
            ids.push(row.discord_user_id);
            handles.push(row.discord_handle);
            emails.push(row.email);
            full_names.push(row.full_name);
            standings.push(row.standing);
            join_dates.push(row.join_date);
            expiries.push(row.expires);
            membership_types.push(row.membership_type);
            monthly_dues.push(row.monthly_dues);
            yearly_dues.push(row.yearly_dues);
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query!("DELETE FROM member_cache")
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            r#"INSERT INTO member_cache
               (discord_user_id, discord_handle, email, full_name, standing,
                join_date, expires, membership_type, monthly_dues, yearly_dues)
               SELECT * FROM UNNEST(
                   $1::bigint[], $2::text[], $3::text[], $4::text[], $5::text[],
                   $6::date[], $7::date[], $8::text[], $9::text[], $10::text[])"#,
            &ids,
            &handles as &[Option<String>],
            &emails,
            &full_names as &[Option<String>],
            &standings as &[Option<String>],
            &join_dates as &[Option<NaiveDate>],
            &expiries as &[Option<NaiveDate>],
            &membership_types as &[Option<String>],
            &monthly_dues as &[Option<String>],
            &yearly_dues as &[Option<String>],
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
