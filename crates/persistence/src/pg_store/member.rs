//! The `member_cache` roster: the stored row shape, the cache-local value tokens, and the
//! [`PgStore`] impls of the roster read/write/repair traits.
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

use domain::MigsStatus;
use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::store::{IdentityWrite, MemberRecord, MemberStore, RosterWrite, dedup_records};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

use crate::PersistenceError;

use super::PgStore;

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
