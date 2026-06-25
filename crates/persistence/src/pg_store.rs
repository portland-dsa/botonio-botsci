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
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, MigsStatus};
use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::channels::{ChannelSnapshot, SNAPSHOT_FORMAT_VERSION, SnapshotMeta};
use engine::reminders::{Milestone, ReminderTemplateKind};
use engine::store::{
    BulkQueueEntry, BulkQueueKind, BulkScope, BulkSession, BulkSessionStore, BulkStatus,
    ChannelSnapshotStore, ConfigStore, GraceOverride, GraceStore, GuildConfig, IdentityWrite,
    MemberRecord, MemberStore, MissCounts, MissState, OptOutSource, OverrideLog, OverrideRecord,
    ReminderCycleState, ReminderStore, ReminderTemplates, RosterWrite, dedup_records,
};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

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

    /// A clone of the underlying pool, for a sibling capability (the [`Auditor`])
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
    st_user_id: String,
    discord_user_id: Option<i64>,
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
            st_user_id: StUserId(r.st_user_id),
            discord_user_id: r.discord_user_id.map(|i| DiscordUserId(i as u64)),
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
            r#"SELECT st_user_id, discord_user_id, discord_handle, email, full_name,
                      standing, join_date, expires, membership_type, monthly_dues, yearly_dues
               FROM member_cache WHERE discord_user_id = $1"#,
            id.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(MemberRecord::from))
    }

    /// Look up one member by their current Discord handle - the repair fallback when an
    /// id lookup misses. Reads the `discord_handle` index.
    async fn by_handle(
        &self,
        handle: &DiscordHandle,
    ) -> Result<Option<MemberRecord>, PersistenceError> {
        let row = sqlx::query_as!(
            MemberCacheRow,
            r#"SELECT st_user_id, discord_user_id, discord_handle, email, full_name,
                      standing, join_date, expires, membership_type, monthly_dues, yearly_dues
               FROM member_cache WHERE discord_handle = $1"#,
            handle.0
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(MemberRecord::from))
    }

    /// Every record currently in the cache, in unspecified order. The reminder planner
    /// iterates the whole cached roster once per sweep to build the plan; reading all rows
    /// in one query rather than paging avoids multiple round-trips at sweep time.
    async fn all_records(&self) -> Result<Vec<MemberRecord>, PersistenceError> {
        let rows = sqlx::query_as!(
            MemberCacheRow,
            r#"SELECT st_user_id, discord_user_id, discord_handle, email, full_name,
                      standing, join_date, expires, membership_type, monthly_dues, yearly_dues
               FROM member_cache"#
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(MemberRecord::from).collect())
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
    /// Records are deduplicated by `st_user_id`, **first-wins**, exactly mirroring the
    /// in-memory [`Index`](engine::store::Index). Records that also carry a Discord id are
    /// further checked: a later record claiming an id already taken by an earlier one is
    /// dropped (first-wins on id too), keeping `by_discord_id` unambiguous. Records with
    /// no Discord id are retained - they are exactly who a verify backfill repairs - and
    /// are found afterwards by `by_handle`. This is done in Rust rather than letting a
    /// duplicate id reach the index and abort the transaction: two Solidarity Tech members
    /// sharing a Discord id must not fail the whole roster load - the lenient-sweep
    /// invariant. It also keeps the two stores genuinely equivalent.
    ///
    /// The kept rows go in as a single `UNNEST` batch - one round-trip rather than one
    /// per member - which also keeps the write transaction (and its locks) short.
    ///
    /// `DELETE`, not `TRUNCATE`, on purpose: the runtime role is granted only DML
    /// (`SELECT`/`INSERT`/`UPDATE`/`DELETE`) and holds no `TRUNCATE` privilege, and a
    /// row-level `DELETE` lets card reads continue under MVCC during a refresh instead
    /// of blocking on the `ACCESS EXCLUSIVE` lock `TRUNCATE` would take.
    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), PersistenceError> {
        // Encode the deduplicated records into stored rows. Dedup runs through
        // [`dedup_records`] - the rule the in-memory `Index` shares - so the two stores
        // keep the same row on a collision; records with no Discord id are retained, since
        // they are exactly who the verify backfill repairs. Each row's fields are encoded
        // together in one struct literal, so the column mapping lives in one place rather
        // than spread across parallel pushes.
        let deduped = dedup_records(records);
        let mut rows: Vec<MemberCacheRow> = Vec::with_capacity(deduped.len());
        for r in deduped {
            rows.push(MemberCacheRow {
                st_user_id: r.st_user_id.0,
                discord_user_id: r.discord_user_id.map(|id| id.0 as i64),
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
        let mut st_ids: Vec<String> = Vec::with_capacity(cap);
        let mut ids: Vec<Option<i64>> = Vec::with_capacity(cap);
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
            st_ids.push(row.st_user_id);
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
               (st_user_id, discord_user_id, discord_handle, email, full_name, standing,
                join_date, expires, membership_type, monthly_dues, yearly_dues)
               SELECT * FROM UNNEST(
                   $1::text[], $2::bigint[], $3::text[], $4::text[], $5::text[], $6::text[],
                   $7::date[], $8::date[], $9::text[], $10::text[], $11::text[])"#,
            &st_ids,
            &ids as &[Option<i64>],
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

#[async_trait]
impl IdentityWrite for PgStore {
    type Error = PersistenceError;

    /// Repair one row's Discord identity in place, keyed by the Solidarity Tech id - the
    /// only columns the runtime role may `UPDATE`. A member not in the cache updates no
    /// rows; that stays a success (the role was already granted and the next sweep will
    /// cache the member), but it is logged, because a zero-row repair otherwise looks
    /// identical to a successful one - it means the roster sweep has not yet stored this
    /// member, and the write-through silently landed nowhere.
    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), PersistenceError> {
        let affected = sqlx::query!(
            r#"UPDATE member_cache
               SET discord_user_id = $1, discord_handle = $2
               WHERE st_user_id = $3"#,
            discord_id.0 as i64,
            handle.0,
            st_user_id.0,
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        if affected == 0 {
            tracing::warn!(
                st_user_id = %st_user_id.0,
                "link_identity matched no cached row; the member is not yet in the cache, so the identity repair will land on the next roster sweep"
            );
        }
        Ok(())
    }

    /// Clear the cached Discord identity from the row holding `discord_id`, so a later
    /// verify misses by both id and handle. A `discord_id` no row holds updates nothing,
    /// which stays a success - there is nothing to unlink.
    async fn unlink_by_discord_id(
        &self,
        discord_id: DiscordUserId,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"UPDATE member_cache SET discord_user_id = NULL, discord_handle = NULL
               WHERE discord_user_id = $1"#,
            discord_id.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl OverrideLog for PgStore {
    type Error = PersistenceError;

    /// Stamp a hand approval. `ON CONFLICT DO NOTHING` makes it insert-once: a re-stamp
    /// for the same subject leaves the original `approved_by`/`approved_at` untouched.
    async fn stamp_override(
        &self,
        subject: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO manual_override (discord_user_id, approved_by, note)
               VALUES ($1, $2, $3) ON CONFLICT (discord_user_id) DO NOTHING"#,
            subject.0 as i64,
            approver.0 as i64,
            note,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The override stamp for `subject`, or `None`. Read-only `SELECT`, which the
    /// runtime role already holds on `manual_override`.
    async fn get_override(
        &self,
        subject: DiscordUserId,
    ) -> Result<Option<OverrideRecord>, PersistenceError> {
        let row = sqlx::query!(
            "SELECT approved_by, approved_at, note FROM manual_override WHERE discord_user_id = $1",
            subject.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| OverrideRecord {
            approved_by: DiscordUserId(r.approved_by as u64),
            approved_at: r.approved_at,
            note: r.note,
        }))
    }

    async fn delete_override(&self, subject: DiscordUserId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM manual_override WHERE discord_user_id = $1",
            subject.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// One `guild_config` row as `sqlx` reads it: every snowflake an `Option<i64>`.
struct GuildConfigRow {
    moderator_role_id: Option<i64>,
    member_role_id: Option<i64>,
    dues_expired_role_id: Option<i64>,
    unverified_role_id: Option<i64>,
    manual_override_role_id: Option<i64>,
    mod_approval_channel_id: Option<i64>,
    unverified_channel_id: Option<i64>,
    dues_expired_channel_id: Option<i64>,
    verification_log_channel_id: Option<i64>,
    dues_reminder_channel_id: Option<i64>,
    dues_signup_url: Option<String>,
    reminders_enabled: bool,
    scan_enabled: bool,
}

impl From<GuildConfigRow> for GuildConfig {
    fn from(r: GuildConfigRow) -> Self {
        let role = |v: Option<i64>| v.map(|i| DiscordRoleId(i as u64));
        let chan = |v: Option<i64>| v.map(|i| DiscordChannelId(i as u64));
        GuildConfig {
            moderator_role: role(r.moderator_role_id),
            member_role: role(r.member_role_id),
            dues_expired_role: role(r.dues_expired_role_id),
            unverified_role: role(r.unverified_role_id),
            manual_override_role: role(r.manual_override_role_id),
            mod_approval_channel: chan(r.mod_approval_channel_id),
            unverified_channel: chan(r.unverified_channel_id),
            dues_expired_channel: chan(r.dues_expired_channel_id),
            verification_log_channel: chan(r.verification_log_channel_id),
            dues_reminder_channel: chan(r.dues_reminder_channel_id),
            dues_signup_url: r.dues_signup_url,
            reminders_enabled: r.reminders_enabled,
            scan_enabled: r.scan_enabled,
        }
    }
}

#[async_trait]
impl ConfigStore for PgStore {
    type Error = PersistenceError;

    async fn load_config(&self, guild: DiscordGuildId) -> Result<GuildConfig, PersistenceError> {
        let row = sqlx::query_as!(
            GuildConfigRow,
            r#"SELECT moderator_role_id, member_role_id, dues_expired_role_id,
                      unverified_role_id, manual_override_role_id, mod_approval_channel_id,
                      unverified_channel_id, dues_expired_channel_id,
                      verification_log_channel_id, dues_reminder_channel_id,
                      dues_signup_url, reminders_enabled, scan_enabled
               FROM guild_config WHERE guild_id = $1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(GuildConfig::from).unwrap_or_default())
    }

    async fn save_config(
        &self,
        guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), PersistenceError> {
        let role = |r: Option<DiscordRoleId>| r.map(|x| x.0 as i64);
        let chan = |c: Option<DiscordChannelId>| c.map(|x| x.0 as i64);
        sqlx::query!(
            r#"INSERT INTO guild_config
                 (guild_id, moderator_role_id, member_role_id, dues_expired_role_id,
                  unverified_role_id, manual_override_role_id, mod_approval_channel_id,
                  unverified_channel_id, dues_expired_channel_id,
                  verification_log_channel_id, dues_reminder_channel_id,
                  dues_signup_url, reminders_enabled, scan_enabled, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, now())
               ON CONFLICT (guild_id) DO UPDATE SET
                 moderator_role_id           = EXCLUDED.moderator_role_id,
                 member_role_id              = EXCLUDED.member_role_id,
                 dues_expired_role_id        = EXCLUDED.dues_expired_role_id,
                 unverified_role_id          = EXCLUDED.unverified_role_id,
                 manual_override_role_id     = EXCLUDED.manual_override_role_id,
                 mod_approval_channel_id     = EXCLUDED.mod_approval_channel_id,
                 unverified_channel_id       = EXCLUDED.unverified_channel_id,
                 dues_expired_channel_id     = EXCLUDED.dues_expired_channel_id,
                 verification_log_channel_id = EXCLUDED.verification_log_channel_id,
                 dues_reminder_channel_id    = EXCLUDED.dues_reminder_channel_id,
                 dues_signup_url             = EXCLUDED.dues_signup_url,
                 reminders_enabled           = EXCLUDED.reminders_enabled,
                 scan_enabled                = EXCLUDED.scan_enabled,
                 updated_at                  = now()"#,
            guild.0 as i64,
            role(config.moderator_role),
            role(config.member_role),
            role(config.dues_expired_role),
            role(config.unverified_role),
            role(config.manual_override_role),
            chan(config.mod_approval_channel),
            chan(config.unverified_channel),
            chan(config.dues_expired_channel),
            chan(config.verification_log_channel),
            chan(config.dues_reminder_channel),
            config.dues_signup_url,
            config.reminders_enabled,
            config.scan_enabled,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl BulkSessionStore for PgStore {
    type Error = PersistenceError;

    async fn load_session(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<BulkSession>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT scope, status, started_by, created_at, updated_at
               FROM bulk_verify_session WHERE guild_id = $1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let scope = BulkScope::from_token(&r.scope)
            .ok_or_else(|| PersistenceError::BadToken(format!("bulk scope {:?}", r.scope)))?;
        let status = BulkStatus::from_token(&r.status)
            .ok_or_else(|| PersistenceError::BadToken(format!("bulk status {:?}", r.status)))?;
        Ok(Some(BulkSession {
            guild,
            scope,
            status,
            started_by: DiscordUserId(r.started_by as u64),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }))
    }

    async fn start_session(
        &self,
        session: &BulkSession,
        misses: &[BulkQueueEntry],
    ) -> Result<(), PersistenceError> {
        let mut tx = self.pool.begin().await?;
        // Wholesale replace: the CASCADE clears any prior queue with the session row.
        sqlx::query!(
            "DELETE FROM bulk_verify_session WHERE guild_id = $1",
            session.guild.0 as i64
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            r#"INSERT INTO bulk_verify_session (guild_id, scope, status, started_by)
               VALUES ($1, $2, $3, $4)"#,
            session.guild.0 as i64,
            session.scope.as_token(),
            session.status.as_token(),
            session.started_by.0 as i64,
        )
        .execute(&mut *tx)
        .await?;

        if !misses.is_empty() {
            // One UNNEST batch insert, mirroring replace_roster's single round-trip.
            let cap = misses.len();
            let mut ids: Vec<i64> = Vec::with_capacity(cap);
            let mut handles: Vec<Option<String>> = Vec::with_capacity(cap);
            let mut positions: Vec<i32> = Vec::with_capacity(cap);
            let mut states: Vec<String> = Vec::with_capacity(cap);
            let mut kinds: Vec<String> = Vec::with_capacity(cap);
            for m in misses {
                ids.push(m.discord_user_id.0 as i64);
                handles.push(m.handle.as_ref().map(|h| h.0.clone()));
                positions.push(m.position);
                states.push(m.state.as_token().to_owned());
                kinds.push(m.kind.as_token().to_owned());
            }
            sqlx::query!(
                r#"INSERT INTO bulk_verify_miss (guild_id, discord_user_id, handle, position, state, kind)
                   SELECT $1, * FROM UNNEST($2::bigint[], $3::text[], $4::int[], $5::text[], $6::text[])"#,
                session.guild.0 as i64,
                &ids,
                &handles as &[Option<String>],
                &positions,
                &states,
                &kinds,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn next_pending(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<BulkQueueEntry>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT discord_user_id, handle, position, state, kind
               FROM bulk_verify_miss
               WHERE guild_id = $1 AND state = 'pending'
               ORDER BY position ASC LIMIT 1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let state = MissState::from_token(&r.state)
            .ok_or_else(|| PersistenceError::BadToken(format!("miss state {:?}", r.state)))?;
        let kind = BulkQueueKind::from_token(&r.kind)
            .ok_or_else(|| PersistenceError::BadToken(format!("queue kind {:?}", r.kind)))?;
        Ok(Some(BulkQueueEntry {
            discord_user_id: DiscordUserId(r.discord_user_id as u64),
            handle: r.handle.map(DiscordHandle),
            position: r.position,
            state,
            kind,
        }))
    }

    async fn mark_miss(
        &self,
        guild: DiscordGuildId,
        member: DiscordUserId,
        state: MissState,
    ) -> Result<(), PersistenceError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            r#"UPDATE bulk_verify_miss SET state = $3
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            member.0 as i64,
            state.as_token(),
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE bulk_verify_session SET updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn counts(&self, guild: DiscordGuildId) -> Result<MissCounts, PersistenceError> {
        let rows = sqlx::query!(
            r#"SELECT state, COUNT(*) AS "n!" FROM bulk_verify_miss
               WHERE guild_id = $1 GROUP BY state"#,
            guild.0 as i64
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = MissCounts::default();
        for r in rows {
            match MissState::from_token(&r.state) {
                Some(MissState::Pending) => counts.pending = r.n as usize,
                Some(MissState::Verified) => counts.verified = r.n as usize,
                Some(MissState::Skipped) => counts.skipped = r.n as usize,
                None => {
                    return Err(PersistenceError::BadToken(format!(
                        "miss state {:?}",
                        r.state
                    )));
                }
            }
        }
        Ok(counts)
    }

    async fn complete_session(&self, guild: DiscordGuildId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "UPDATE bulk_verify_session SET status = 'complete', updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn abandon_session(&self, guild: DiscordGuildId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "UPDATE bulk_verify_session SET status = 'abandoned', updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Per-guild snapshot history is bounded on every save: keep at most this many of the
/// newest snapshots, and (in the same prune) drop any older than the 6-month TTL spelled
/// in [`save_snapshot`](PgStore::save_snapshot)'s DELETE. The undo stack is for recent
/// mistakes, so a row several applies back - or older than the retention window - is no
/// longer a useful restore target and is reaped rather than kept forever.
const SNAPSHOT_KEEP_MAX: i64 = 5;

#[async_trait]
impl ChannelSnapshotStore for PgStore {
    type Error = PersistenceError;

    /// Append a whole-guild channel-permission snapshot, then prune the guild's history.
    /// Each call inserts a new row (history is never overwritten, so successive saves form
    /// an undo stack) and the same transaction reaps every row past the bound: older than
    /// the 6-month TTL, or beyond the newest [`SNAPSHOT_KEEP_MAX`], whichever removes more.
    /// The `channels` Vec is stored as JSONB so the restore path can deserialize it without
    /// a per-overwrite join.
    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), PersistenceError> {
        let channels = serde_json::to_value(&snapshot.channels)?;
        let guild = snapshot.guild_id.0 as i64;

        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "INSERT INTO channel_perms_snapshot (guild_id, saved_at, format_version, channels) \
             VALUES ($1, $2, $3, $4)",
            guild,
            snapshot.saved_at,
            snapshot.format_version as i32,
            channels,
        )
        .execute(&mut *tx)
        .await?;
        // Keep the undo stack bounded: drop rows older than the 6-month TTL, and any beyond
        // the newest SNAPSHOT_KEEP_MAX for this guild. Same transaction as the insert, so
        // the cap holds atomically and the table can never grow without limit.
        sqlx::query!(
            "DELETE FROM channel_perms_snapshot \
             WHERE guild_id = $1 \
               AND (saved_at < now() - interval '6 months' \
                    OR id NOT IN ( \
                        SELECT id FROM channel_perms_snapshot \
                        WHERE guild_id = $1 \
                        ORDER BY saved_at DESC, id DESC \
                        LIMIT $2 \
                    ))",
            guild,
            SNAPSHOT_KEEP_MAX,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The most recent snapshot for `guild`, ordered by `saved_at DESC` so the
    /// same newest-first rule the in-memory store enforces by insertion-order also
    /// holds in Postgres.
    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT format_version, guild_id, saved_at,
                      channels AS "channels: serde_json::Value"
               FROM channel_perms_snapshot
               WHERE guild_id = $1
               ORDER BY saved_at DESC LIMIT 1"#,
            guild.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        if r.format_version > SNAPSHOT_FORMAT_VERSION as i32 {
            return Err(PersistenceError::SnapshotVersion {
                found: r.format_version,
                known: SNAPSHOT_FORMAT_VERSION as i32,
            });
        }
        let channels = serde_json::from_value(r.channels)?;
        Ok(Some(ChannelSnapshot {
            format_version: r.format_version as u32,
            guild_id: DiscordGuildId(r.guild_id as u64),
            saved_at: r.saved_at,
            channels,
        }))
    }

    /// All snapshots' metadata for `guild`, newest first - for the restore picker. Uses
    /// `jsonb_array_length` so only the length, not the full channel data, crosses the
    /// wire.
    async fn list_snapshots(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Vec<SnapshotMeta>, PersistenceError> {
        let rows = sqlx::query!(
            r#"SELECT saved_at, jsonb_array_length(channels) AS "channel_count!"
               FROM channel_perms_snapshot
               WHERE guild_id = $1
               ORDER BY saved_at DESC"#,
            guild.0 as i64,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| SnapshotMeta {
                saved_at: r.saved_at,
                channel_count: r.channel_count as usize,
            })
            .collect())
    }
}

#[async_trait]
impl GraceStore for PgStore {
    type Error = PersistenceError;

    /// `true` when `id` has a row in `dues_grace_override` whose `grace_until >= today`.
    async fn active_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        today: NaiveDate,
    ) -> Result<bool, PersistenceError> {
        let active = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM dues_grace_override
                   WHERE guild_id = $1 AND discord_user_id = $2
                     AND grace_until >= $3
               ) AS "active!""#,
            guild.0 as i64,
            id.0 as i64,
            today,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(active)
    }

    /// The full grace stamp for `id`, or `None` if there is none.
    async fn grace_override(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<GraceOverride>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT grace_until, granted_by, granted_at, reason
               FROM dues_grace_override
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| GraceOverride {
            until: r.grace_until,
            granted_by: DiscordUserId(r.granted_by as u64),
            granted_at: r.granted_at,
            reason: r.reason,
        }))
    }

    /// Upsert a grace stamp (insert or extend the window).
    async fn set_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        until: NaiveDate,
        granted_by: DiscordUserId,
        reason: Option<String>,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_grace_override
                 (guild_id, discord_user_id, grace_until, granted_by, granted_at, reason)
               VALUES ($1, $2, $3, $4, now(), $5)
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 grace_until = EXCLUDED.grace_until,
                 granted_by  = EXCLUDED.granted_by,
                 granted_at  = EXCLUDED.granted_at,
                 reason      = EXCLUDED.reason"#,
            guild.0 as i64,
            id.0 as i64,
            until,
            granted_by.0 as i64,
            reason,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a member's grace stamp. A member with none is a no-op.
    async fn clear_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM dues_grace_override WHERE guild_id = $1 AND discord_user_id = $2",
            guild.0 as i64,
            id.0 as i64,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl ReminderStore for PgStore {
    type Error = PersistenceError;

    /// The member's per-cycle reminder state, or `None` if they have never been reminded.
    async fn reminder_state(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<ReminderCycleState>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT cycle_xdate, last_sent, snoozed, thread_id
               FROM dues_reminder_state
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let last_sent = r
            .last_sent
            .as_deref()
            .map(|t| {
                Milestone::from_token(t)
                    .ok_or_else(|| PersistenceError::BadToken(format!("milestone {:?}", t)))
            })
            .transpose()?;
        Ok(Some(ReminderCycleState {
            cycle_xdate: r.cycle_xdate,
            last_sent,
            snoozed: r.snoozed,
            thread_id: r.thread_id,
        }))
    }

    /// Record that `milestone` was sent for the cycle ending `cycle_xdate`. Upserts the row
    /// and resets `snoozed` to false when `cycle_xdate` differs from the stored one (the
    /// cycle has turned); otherwise preserves the stored `snoozed` value.
    async fn record_sent(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        milestone: Milestone,
        thread_id: i64,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, snoozed, thread_id, updated_at)
               VALUES ($1, $2, $3, $4, false, $5, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 last_sent   = EXCLUDED.last_sent,
                 cycle_xdate = EXCLUDED.cycle_xdate,
                 thread_id   = EXCLUDED.thread_id,
                 snoozed     = CASE
                                   WHEN dues_reminder_state.cycle_xdate <> EXCLUDED.cycle_xdate
                                   THEN false
                                   ELSE dues_reminder_state.snoozed
                               END,
                 updated_at  = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
            milestone.as_token(),
            thread_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist the lifecycle thread id without recording a send, preserving the existing cycle
    /// fields. Seeds a new row with `cycle_xdate` and no recorded milestone.
    async fn set_thread(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        thread_id: i64,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, snoozed, thread_id, updated_at)
               VALUES ($1, $2, $3, NULL, false, $4, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 thread_id  = EXCLUDED.thread_id,
                 updated_at = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
            thread_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set the snooze for the cycle ending `cycle_xdate`, upserting the row if needed.
    async fn set_snooze(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, snoozed, thread_id, updated_at)
               VALUES ($1, $2, $3, NULL, true, NULL, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 cycle_xdate = EXCLUDED.cycle_xdate,
                 snoozed     = true,
                 updated_at  = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether `id` has a permanent opt-out row in `dues_reminder_optout`.
    async fn is_opted_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<bool, PersistenceError> {
        let opted_out = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM dues_reminder_optout
                   WHERE guild_id = $1 AND discord_user_id = $2
               ) AS "opted_out!""#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(opted_out)
    }

    /// Insert the permanent opt-out row. On conflict (already opted out) updates the
    /// source so a moderator reversal then re-opt-out records the latest actor.
    async fn opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        source: OptOutSource,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_optout
                 (guild_id, discord_user_id, opted_out_at, source)
               VALUES ($1, $2, now(), $3)
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 opted_out_at = EXCLUDED.opted_out_at,
                 source       = EXCLUDED.source"#,
            guild.0 as i64,
            id.0 as i64,
            source.as_token(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove the opt-out row. A member with none is a no-op.
    async fn clear_opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM dues_reminder_optout WHERE guild_id = $1 AND discord_user_id = $2",
            guild.0 as i64,
            id.0 as i64,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// When the reminder sweep last completed for `guild`, or `None` if it never has.
    async fn last_reminder_run(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<DateTime<Utc>>, PersistenceError> {
        let row = sqlx::query_scalar!(
            "SELECT last_run_at FROM dues_reminder_run WHERE guild_id = $1",
            guild.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Upsert the last-run marker for `guild`.
    async fn set_last_reminder_run(
        &self,
        guild: DiscordGuildId,
        at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_run (guild_id, last_run_at)
               VALUES ($1, $2)
               ON CONFLICT (guild_id) DO UPDATE SET last_run_at = EXCLUDED.last_run_at"#,
            guild.0 as i64,
            at,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl ReminderTemplates for PgStore {
    type Error = PersistenceError;

    /// The stored body for `kind`, or `None` to use the built-in default.
    async fn template(
        &self,
        guild: DiscordGuildId,
        kind: ReminderTemplateKind,
    ) -> Result<Option<String>, PersistenceError> {
        let row = sqlx::query_scalar!(
            "SELECT body FROM dues_reminder_template WHERE guild_id = $1 AND kind = $2",
            guild.0 as i64,
            kind.as_token(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Upsert the body for `kind`.
    async fn set_template(
        &self,
        guild: DiscordGuildId,
        kind: ReminderTemplateKind,
        body: String,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_template (guild_id, kind, body)
               VALUES ($1, $2, $3)
               ON CONFLICT (guild_id, kind) DO UPDATE SET body = EXCLUDED.body"#,
            guild.0 as i64,
            kind.as_token(),
            body,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
