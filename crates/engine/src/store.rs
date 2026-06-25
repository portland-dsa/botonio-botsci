//! The reusable member-record store: a flat [`MemberRecord`] and the
//! [`MemberStore`] trait the card resolver reads through.
//!
//! The current implementation ([`InMemoryStore`]) holds the roster in RAM, swept
//! from a Solidarity Tech user list. [`MemberRecord`] is deliberately flat and
//! built from persistence-friendly primitives so a future implementation can back the same
//! [`MemberStore`] trait with a sqlx-mapped Postgres table without changing any caller.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, MembershipStatus, MigsStatus};

use crate::backends::solidarity_tech::{
    DuesStatus, MembershipType, SolidarityTechClient, SolidarityTechMember,
};
use crate::channels::snapshot::{ChannelSnapshot, SnapshotMeta};
use crate::paging::drain_pages;
use crate::seam::NoProgress;
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};

/// A member projected to the flat shape the card and the future cache share.
/// Every field is a persistence-friendly primitive (`String`,
/// `Option<NaiveDate>`, small text-mapped enums) so a future implementation maps it to one
/// Postgres-backed table with no nesting.
///
/// `PartialEq`/`Eq` let two records be compared whole - the basis of the
/// `PgStore`/`InMemoryStore` conformance test, which asserts a record survives the
/// cache's encode/store/decode round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The Solidarity Tech user id - the stable key, and the target a self-heal
    /// writes the Discord identity back to. Every cached record is sourced from a
    /// Solidarity Tech member, so this is always present.
    pub st_user_id: StUserId,
    pub discord_user_id: Option<DiscordUserId>,
    pub discord_handle: Option<DiscordHandle>,
    pub email: Email,
    pub full_name: Option<String>,
    /// Raw "Membership Status"; the [`Role`] is derived (never stored twice).
    pub standing: Option<MigsStatus>,
    pub join_date: Option<NaiveDate>,
    /// Dues-expiry date (`xdate`).
    pub expires: Option<NaiveDate>,
    pub membership_type: Option<MembershipType>,
    pub monthly_dues: Option<DuesStatus>,
    pub yearly_dues: Option<DuesStatus>,
}

impl MemberRecord {
    /// The computed membership status for this record. An absent standing is
    /// [`Malformed`](MembershipStatus::Malformed) - a matched record we cannot decide
    /// a role from - distinct from the live good-standing/lapsed values.
    pub fn membership(&self) -> MembershipStatus {
        self.standing
            .map(MembershipStatus::from)
            .unwrap_or(MembershipStatus::Malformed)
    }
}

impl From<SolidarityTechMember> for MemberRecord {
    fn from(m: SolidarityTechMember) -> Self {
        Self {
            st_user_id: m.id,
            discord_user_id: m.discord_user_id,
            discord_handle: m.discord_handle,
            email: m.email,
            full_name: match (m.first_name, m.last_name) {
                (Some(f), Some(l)) => Some(format!("{f} {l}")),
                (Some(f), None) => Some(f),
                (None, Some(l)) => Some(l),
                (None, None) => None,
            },
            standing: m.membership_standing,
            join_date: m.join_date,
            expires: m.xdate,
            membership_type: m.membership_type,
            monthly_dues: m.monthly_dues,
            yearly_dues: m.yearly_dues,
        }
    }
}

/// Deduplicate projected records the way both stores must agree on: first-wins on the
/// Solidarity Tech id, then first-wins on the Discord id (a later record claiming an id an
/// earlier one already holds is dropped, keeping the id lookups unambiguous). Records with
/// no Discord id are kept - they are exactly who a verify backfill repairs - and are found
/// afterwards by handle. The kept records are returned in input order.
///
/// This is the single definition of the dedup rule. Both the in-memory [`Index`] and the
/// Postgres store run their inputs through it, so the two stores can never silently diverge
/// on which of a pair of colliding records survives.
pub fn dedup_records(records: Vec<MemberRecord>) -> Vec<MemberRecord> {
    let mut seen_st = HashSet::new();
    let mut seen_id = HashSet::new();
    let mut kept = Vec::with_capacity(records.len());
    for rec in records {
        if !seen_st.insert(rec.st_user_id.0.clone()) {
            continue; // the same Solidarity Tech member was already kept
        }
        // A later record claiming an already-taken Discord id is dropped (first-wins).
        if let Some(id) = rec.discord_user_id
            && !seen_id.insert(id.0)
        {
            continue;
        }
        kept.push(rec);
    }
    kept
}

/// An immutable lookup index. Keyed by Discord id for the card's id-only read, and
/// also by handle so a member known to Solidarity Tech by handle but not yet linked
/// to a Discord id is still found - the population verification repairs. The card
/// still reads `by_id` only (see [`crate::card::resolve`]); the handle map exists for
/// the verify path.
#[derive(Default)]
pub struct Index {
    by_id: HashMap<u64, MemberRecord>,
    by_handle: HashMap<String, MemberRecord>,
}

impl Index {
    /// Build from a Solidarity Tech sweep.
    pub fn build(st: Vec<SolidarityTechMember>) -> Self {
        Self::from_records(st.into_iter().map(MemberRecord::from).collect())
    }

    /// Build from already-projected [`MemberRecord`]s (the shape the cache stores).
    ///
    /// Runs the input through [`dedup_records`] - the rule the Postgres store shares - so
    /// a record whose Solidarity Tech id or Discord id was already claimed is dropped from
    /// both maps, keeping the two stores equivalent.
    pub fn from_records(records: Vec<MemberRecord>) -> Self {
        let mut idx = Index::default();
        for rec in dedup_records(records) {
            idx.insert(rec);
        }
        idx
    }

    /// Insert a record the caller has already deduplicated, into whichever maps its
    /// identity supports.
    fn insert(&mut self, rec: MemberRecord) {
        if let Some(handle) = rec.discord_handle.clone() {
            self.by_handle
                .entry(handle.0)
                .or_insert_with(|| rec.clone());
        }
        if let Some(id) = rec.discord_user_id {
            self.by_id.entry(id.0).or_insert(rec);
        }
    }

    #[cfg(test)]
    pub(crate) fn default_for_test() -> Self {
        Index::default()
    }

    /// Look up by Discord user id.
    pub fn by_id(&self, id: DiscordUserId) -> Option<MemberRecord> {
        self.by_id.get(&id.0).cloned()
    }

    /// Look up by Discord handle. Used only by the verify path; the card resolves by id.
    pub fn by_handle(&self, handle: &DiscordHandle) -> Option<MemberRecord> {
        self.by_handle.get(&handle.0).cloned()
    }

    /// Whether the index holds no members (every input record was a duplicate or
    /// lacked a Discord id and a handle). Used to refuse replacing a populated roster
    /// with an empty sweep.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_handle.is_empty()
    }
}

/// The per-guild runtime configuration set through the bot's `/setup` command:
/// the moderator role, the three managed status roles, the additive Manual Override
/// marker, and the verification channels. Every field is optional - a freshly
/// deployed guild has nothing set until a moderator configures it. Built from id
/// newtypes so a store maps it to a single nullable-column row with no nesting, exactly
/// like [`MemberRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuildConfig {
    pub moderator_role: Option<DiscordRoleId>,
    pub member_role: Option<DiscordRoleId>,
    pub dues_expired_role: Option<DiscordRoleId>,
    pub unverified_role: Option<DiscordRoleId>,
    /// The additive Manual Override marker role, granted alongside `Member` on a hand
    /// approval. Optional and outside the status trichotomy: ordinary verification works
    /// without it, and it is never stripped by the status-role logic.
    pub manual_override_role: Option<DiscordRoleId>,
    pub mod_approval_channel: Option<DiscordChannelId>,
    pub unverified_channel: Option<DiscordChannelId>,
    pub dues_expired_channel: Option<DiscordChannelId>,
    /// The moderator-private channel that logs every successful self-service
    /// verification - the member and the email that matched. Unset by default;
    /// when unset the grant still happens and only the log post is skipped.
    pub verification_log_channel: Option<DiscordChannelId>,
    /// The channel the dues-reminder private threads are parented off. Must be visible to
    /// both `Member` and `Dues Expired` so a member's thread survives their lapse demotion.
    pub dues_reminder_channel: Option<DiscordChannelId>,
    /// The external dues sign-up page the reminder "Renew" button links to.
    pub dues_signup_url: Option<String>,
    /// Whether the dues-reminder sweep runs for this guild. Off by default, like
    /// [`scan_enabled`](Self::scan_enabled); the two toggle independently.
    pub reminders_enabled: bool,
    /// Whether the scheduled membership scan runs for this guild. Off by default - the
    /// scan reconciles roles and can demote, so it is opt-in via /setup.
    pub scan_enabled: bool,
}

/// Reverse lookup from a Discord id to a [`MemberRecord`]. Async and fallible from
/// the start so a later Postgres-backed implementation drops in without a signature
/// change; the in-memory impl's [`Error`](MemberStore::Error) is [`Infallible`].
#[async_trait]
pub trait MemberStore: Send + Sync {
    /// How a read can fail. [`Infallible`] for the in-memory store; a database
    /// error for a Postgres-backed one.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Look up a member by their Discord user snowflake.
    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Self::Error>;

    /// Look up a member by their current Discord handle. The repair fallback when an
    /// id lookup misses; the card never uses it.
    ///
    /// Handles are not unique in the cache - a handle can be recycled between roster
    /// sweeps - so when several records share one, which is returned is unspecified and may
    /// differ between implementations. That is acceptable because the immutable id is
    /// authoritative: verify reads this only after [`by_discord_id`](Self::by_discord_id)
    /// misses, and the conflict guard still refuses to re-link a record already bound to a
    /// different id.
    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Self::Error>;

    /// Every record currently in the roster, in unspecified order. The reminder planner
    /// iterates the whole cached roster once per sweep to build the plan. The
    /// `InMemoryStore` impl clones its index's records; the `PgStore` impl runs
    /// `SELECT * FROM member_cache`.
    async fn all_records(&self) -> Result<Vec<MemberRecord>, Self::Error>;
}

/// Replace the whole cached roster in one shot - the write half of a refresh sweep.
/// Fallible from the start for the same reason as [`MemberStore`]: the in-memory
/// impl's [`Error`](RosterWrite::Error) is [`Infallible`], a Postgres-backed one's
/// is a database error.
#[async_trait]
pub trait RosterWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Atomically replace the stored roster with `records`. An empty roster is a
    /// no-op that preserves the current one: a sweep resolving to zero members is
    /// treated as an upstream glitch, never a real membership of zero.
    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Self::Error>;
}

/// Repair one member's stored Discord identity in place, keyed by their Solidarity
/// Tech id. The write-through half of verification's self-heal: distinct from
/// [`RosterWrite`], which only ever replaces the whole roster. Fallible from the
/// start for the same reason as the other store traits.
#[async_trait]
pub trait IdentityWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Set the Discord user id and handle on the member with `st_user_id`. A member
    /// the store does not hold is a silent no-op (nothing to repair).
    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Self::Error>;

    /// Clear the Discord identity (id and handle) from whichever cached row currently
    /// holds `discord_id`, returning the member to an unlinked state so a later verify
    /// misses by both id and handle. A `discord_id` no row holds is a silent no-op.
    async fn unlink_by_discord_id(&self, discord_id: DiscordUserId) -> Result<(), Self::Error>;
}

/// Read and replace one guild's [`GuildConfig`]. Fallible and async from the start
/// for the same reason as the other store traits: the in-memory impl's
/// [`Error`](ConfigStore::Error) is [`Infallible`], a Postgres-backed one's is a
/// database error. `save_config` replaces the whole row (last-writer-wins); config
/// is admin-only and low-frequency, so no per-field write path is needed.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Load the config for `guild`, returning the default (all-unset) when the guild
    /// has no stored row yet.
    async fn load_config(&self, guild: DiscordGuildId) -> Result<GuildConfig, Self::Error>;

    /// Replace `guild`'s stored config wholesale.
    async fn save_config(
        &self,
        guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Self::Error>;
}

/// A standing manual-override approval: who vouched for a member, and when. Keyed,
/// like the stamp itself, on the immutable Discord id - an overridden member has no
/// Solidarity Tech id. Returned by [`OverrideLog::get_override`] and rendered on the
/// card a moderator or the member sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRecord {
    pub approved_by: DiscordUserId,
    pub approved_at: DateTime<Utc>,
    /// The optional moderator-supplied reason for the hand approval. Shown to moderators
    /// on a lookup; never copied into the audit log.
    pub note: Option<String>,
}

/// Who set a dues-reminder opt-out - a member pressing the button, or a moderator by
/// hand. Stored for the audit/analytics trail on `dues_reminder_optout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptOutSource {
    Member,
    Moderator,
}

impl OptOutSource {
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Moderator => "mod",
        }
    }
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "member" => Some(Self::Member),
            "mod" => Some(Self::Moderator),
            _ => None,
        }
    }
}

/// One member's per-cycle reminder bookkeeping. `cycle_xdate` ties `last_sent` and
/// `snoozed` to a specific lapse date: when the member's record shows a different
/// `expires`, the cycle has turned and the planner treats `last_sent`/`snoozed` as
/// reset. `thread_id` is the member's lifecycle thread and survives the reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderCycleState {
    pub cycle_xdate: NaiveDate,
    pub last_sent: Option<crate::reminders::Milestone>,
    pub snoozed: bool,
    pub thread_id: Option<i64>,
}

/// A moderator grace stamp: hold this member at `Member` until `until` (inclusive),
/// ignoring their dues. Active while `until >= today`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraceOverride {
    pub until: NaiveDate,
    pub granted_by: DiscordUserId,
    pub granted_at: DateTime<Utc>,
    pub reason: Option<String>,
}

/// Record and clear the permanent note that a member was hand-approved past Solidarity
/// Tech. Keyed on the immutable Discord id - an overridden member has no Solidarity Tech
/// id to key on. [`stamp_override`](Self::stamp_override) is insert-once: a second stamp
/// for the same subject preserves the first approval. [`delete_override`](Self::delete_override)
/// is the reset path; production withholds the `DELETE` privilege, so the call compiles
/// but cannot succeed there.
#[async_trait]
pub trait OverrideLog: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Stamp that `subject` was hand-approved by `approver`, with an optional `note`
    /// recording why. Insert-once, so a retry after a later failure preserves the
    /// original approval and its note rather than overwriting them.
    async fn stamp_override(
        &self,
        subject: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), Self::Error>;

    /// The override stamp for `subject`, or `None` if they were never hand-approved.
    /// The card path reads this when Solidarity Tech has no record, to tell a
    /// manually-verified member apart from an unknown one.
    async fn get_override(
        &self,
        subject: DiscordUserId,
    ) -> Result<Option<OverrideRecord>, Self::Error>;

    /// Remove `subject`'s override stamp, returning them to an un-stamped state.
    /// Reachable only through the staging-gated reset; production withholds the `DELETE`
    /// grant, so this fails closed there.
    async fn delete_override(&self, subject: DiscordUserId) -> Result<(), Self::Error>;
}

/// The moderator grace override: hold a member at `Member` for a fixed window. Read by
/// the shared role decision (so the scan and verify never demote a graced member) and by
/// the reminder planner (its top gate). Keyed on the immutable Discord id.
#[async_trait]
pub trait GraceStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Whether `id` has a grace active on `today` (its `until >= today`).
    async fn active_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        today: NaiveDate,
    ) -> Result<bool, Self::Error>;

    /// The full grace stamp (for the card banner), or `None` if there is none.
    async fn grace_override(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<GraceOverride>, Self::Error>;

    /// Upsert a grace stamp (set or extend the window).
    async fn set_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        until: NaiveDate,
        granted_by: DiscordUserId,
        reason: Option<String>,
    ) -> Result<(), Self::Error>;

    /// Remove a member's grace stamp (lift it early). A member with none is a no-op.
    async fn clear_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), Self::Error>;
}

/// Per-member dues-reminder bookkeeping: the cycle state, the permanent opt-out, and the
/// sweep's last-run marker. Keyed on the immutable Discord id.
#[async_trait]
pub trait ReminderStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// The member's cycle state, or `None` if they have never been reminded.
    async fn reminder_state(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<ReminderCycleState>, Self::Error>;

    /// Record that `milestone` was sent for the cycle ending `cycle_xdate`, in
    /// `thread_id`. Upserts: sets `last_sent = milestone`, `cycle_xdate`, `thread_id`,
    /// and (when `cycle_xdate` differs from the stored one) resets `snoozed` to false.
    async fn record_sent(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        milestone: crate::reminders::Milestone,
        thread_id: i64,
    ) -> Result<(), Self::Error>;

    /// Persist a member's lifecycle thread id without recording a send. Saves a freshly created
    /// thread before its first message goes out, so a later send/record failure reuses that thread
    /// rather than orphaning it and creating a duplicate next sweep. Upserts: sets `thread_id`,
    /// preserving `last_sent`/`snoozed`/`cycle_xdate`; a new row is seeded with `cycle_xdate` and
    /// no recorded milestone.
    async fn set_thread(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        thread_id: i64,
    ) -> Result<(), Self::Error>;

    /// Set the snooze for the cycle ending `cycle_xdate` (upserting the row if needed).
    async fn set_snooze(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
    ) -> Result<(), Self::Error>;

    /// Whether `id` has a permanent opt-out.
    async fn is_opted_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<bool, Self::Error>;

    /// Set the permanent opt-out (row presence = opted out).
    async fn opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        source: OptOutSource,
    ) -> Result<(), Self::Error>;

    /// Clear the permanent opt-out (the moderator reversal). A member with none is a no-op.
    async fn clear_opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), Self::Error>;

    /// When the reminder sweep last completed for `guild`, or `None` if it never has.
    async fn last_reminder_run(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<DateTime<Utc>>, Self::Error>;

    /// Record that the sweep completed for `guild` at `at`.
    async fn set_last_reminder_run(
        &self,
        guild: DiscordGuildId,
        at: DateTime<Utc>,
    ) -> Result<(), Self::Error>;
}

/// Read a guild's editable per-type reminder template bodies. An absent row is `None`;
/// the bot's render layer supplies the built-in default.
#[async_trait]
pub trait ReminderTemplates: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// The stored body for `kind`, or `None` to use the built-in default.
    async fn template(
        &self,
        guild: DiscordGuildId,
        kind: crate::reminders::ReminderTemplateKind,
    ) -> Result<Option<String>, Self::Error>;

    /// Upsert the body for `kind`.
    async fn set_template(
        &self,
        guild: DiscordGuildId,
        kind: crate::reminders::ReminderTemplateKind,
        body: String,
    ) -> Result<(), Self::Error>;
}

/// Which members `/bulk-verify` sweeps. The DB token spellings are owned here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkScope {
    /// Members not yet sorted into a real membership status - they hold neither `Member`
    /// nor `DuesExpired` (a bare `Unverified`, or no managed role at all, both count). The
    /// onboarding/repair default; a later sweep re-picks-up anyone still here, so a skipped
    /// member is never stranded.
    UnmanagedOnly,
    /// Every member, re-evaluated - the opt-in full resync.
    WholeGuild,
}

impl BulkScope {
    pub fn as_token(self) -> &'static str {
        match self {
            BulkScope::UnmanagedOnly => "unmanaged",
            BulkScope::WholeGuild => "whole_guild",
        }
    }
    /// Decode a stored token; an unrecognized value is `None` (caller treats a
    /// corrupt row as no session, never silently guesses a scope).
    pub fn from_token(t: &str) -> Option<Self> {
        match t {
            "unmanaged" => Some(BulkScope::UnmanagedOnly),
            "whole_guild" => Some(BulkScope::WholeGuild),
            _ => None,
        }
    }
}

/// A bulk session's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkStatus {
    InProgress,
    Complete,
    Abandoned,
}

impl BulkStatus {
    pub fn as_token(self) -> &'static str {
        match self {
            BulkStatus::InProgress => "in_progress",
            BulkStatus::Complete => "complete",
            BulkStatus::Abandoned => "abandoned",
        }
    }
    pub fn from_token(t: &str) -> Option<Self> {
        match t {
            "in_progress" => Some(BulkStatus::InProgress),
            "complete" => Some(BulkStatus::Complete),
            "abandoned" => Some(BulkStatus::Abandoned),
            _ => None,
        }
    }
}

/// Where one queued miss stands in the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissState {
    Pending,
    Verified,
    Skipped,
}

impl MissState {
    pub fn as_token(self) -> &'static str {
        match self {
            MissState::Pending => "pending",
            MissState::Verified => "verified",
            MissState::Skipped => "skipped",
        }
    }
    pub fn from_token(t: &str) -> Option<Self> {
        match t {
            "pending" => Some(MissState::Pending),
            "verified" => Some(MissState::Verified),
            "skipped" => Some(MissState::Skipped),
            _ => None,
        }
    }
}

/// One in-progress (or terminal) per-guild bulk session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkSession {
    pub guild: DiscordGuildId,
    pub scope: BulkScope,
    pub status: BulkStatus,
    pub started_by: DiscordUserId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Why a member sits in the bulk-verify wizard queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkQueueKind {
    /// Solidarity Tech does not know them (the email/override flow).
    Miss,
    /// A record matched but has no usable standing (override-only flow).
    Malformed,
}

impl BulkQueueKind {
    pub fn as_token(self) -> &'static str {
        match self {
            BulkQueueKind::Miss => "miss",
            BulkQueueKind::Malformed => "malformed",
        }
    }

    /// Decode a stored token; `None` for any other value (the caller turns that into a
    /// typed `PersistenceError::BadToken`, exactly like `MissState::from_token`).
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "miss" => Some(BulkQueueKind::Miss),
            "malformed" => Some(BulkQueueKind::Malformed),
            _ => None,
        }
    }
}

/// One member in a session's frozen wizard queue. `handle` is a display snapshot
/// captured at sweep time and is never read back for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkQueueEntry {
    pub discord_user_id: DiscordUserId,
    pub handle: Option<DiscordHandle>,
    pub position: i32,
    pub state: MissState,
    pub kind: BulkQueueKind,
}

/// The pending/verified/skipped tally for the resume prompt and final summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MissCounts {
    pub pending: usize,
    pub verified: usize,
    pub skipped: usize,
}

/// The resumable per-guild bulk-verify session: one in-progress session per guild,
/// any moderator resumes it. Wholesale-replace semantics on start (like
/// [`RosterWrite::replace_roster`]), so the Postgres impl is granted `DELETE`.
/// Fallible/async from the start; the in-memory impl's error is [`Infallible`].
#[async_trait]
pub trait BulkSessionStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// The guild's session, whatever its status, or `None` if it never had one.
    async fn load_session(&self, guild: DiscordGuildId)
    -> Result<Option<BulkSession>, Self::Error>;

    /// Replace the guild's session and its whole miss queue in one shot (deletes any
    /// prior session/queue first). `misses` is stored in `position` order, all
    /// `Pending`. This is both the initial start and "Start over".
    async fn start_session(
        &self,
        session: &BulkSession,
        misses: &[BulkQueueEntry],
    ) -> Result<(), Self::Error>;

    /// The lowest-position still-`Pending` entry for the guild, or `None` when the
    /// queue is exhausted.
    async fn next_pending(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<BulkQueueEntry>, Self::Error>;

    /// Set one queued member's state (keyed on the id), and touch the session's
    /// `updated_at`. A member not in the queue is a silent no-op.
    async fn mark_miss(
        &self,
        guild: DiscordGuildId,
        member: DiscordUserId,
        state: MissState,
    ) -> Result<(), Self::Error>;

    /// The pending/verified/skipped counts for the guild's queue.
    async fn counts(&self, guild: DiscordGuildId) -> Result<MissCounts, Self::Error>;

    /// Mark the session `Complete` (called when no pending miss remains).
    async fn complete_session(&self, guild: DiscordGuildId) -> Result<(), Self::Error>;

    /// Mark the session `Abandoned` - the lazy staleness purge at entry. Leaves the
    /// queue rows; the next `start_session` replaces them.
    async fn abandon_session(&self, guild: DiscordGuildId) -> Result<(), Self::Error>;
}

/// The in-memory [`MemberStore`]: a snapshot [`Index`] behind a
/// `RwLock<Arc<Index>>`. Reads clone out the `Arc` and never block a concurrent
/// rebuild; the write lock is held only for the pointer swap itself.
pub struct InMemoryStore {
    index: RwLock<Arc<Index>>,
    config: RwLock<GuildConfig>,
    /// Hand-approval stamps: subject Discord id to its [`OverrideRecord`]. The
    /// in-memory analogue of the `manual_override` table, insert-once just like it.
    overrides: RwLock<HashMap<u64, OverrideRecord>>,
    /// The single per-guild bulk session + its queue (in-memory analogue of the
    /// bulk_verify_session/miss tables). `BTreeMap<position, BulkQueueEntry>` keeps the
    /// queue ordered; the option is the at-most-one session.
    bulk: RwLock<Option<(BulkSession, std::collections::BTreeMap<i32, BulkQueueEntry>)>>,
    /// Channel-permission snapshots, in insertion order. Newest is last. The
    /// in-memory analogue of a future snapshots table.
    snapshots: RwLock<Vec<ChannelSnapshot>>,
    /// Moderator grace stamps: Discord id to its [`GraceOverride`].
    grace: RwLock<HashMap<u64, GraceOverride>>,
    /// Per-member reminder cycle state: Discord id to its [`ReminderCycleState`].
    reminder_state: RwLock<HashMap<u64, ReminderCycleState>>,
    /// Permanent opt-outs: Discord id to the [`OptOutSource`] that set it.
    opt_out: RwLock<HashMap<u64, OptOutSource>>,
    /// Per-template-kind bodies: token string to body text.
    templates: RwLock<HashMap<String, String>>,
    /// When the reminder sweep last completed.
    last_reminder_run: RwLock<Option<DateTime<Utc>>>,
}

impl InMemoryStore {
    /// Construct a store from an already-built [`Index`].
    pub fn new(index: Index) -> Self {
        Self {
            index: RwLock::new(Arc::new(index)),
            config: RwLock::new(GuildConfig::default()),
            overrides: RwLock::new(HashMap::new()),
            bulk: RwLock::new(None),
            snapshots: RwLock::new(Vec::new()),
            grace: RwLock::new(HashMap::new()),
            reminder_state: RwLock::new(HashMap::new()),
            opt_out: RwLock::new(HashMap::new()),
            templates: RwLock::new(HashMap::new()),
            last_reminder_run: RwLock::new(None),
        }
    }

    /// Atomically replace the live index. This is the only place the write lock
    /// is taken; in-flight reads hold their own `Arc` clone and are unaffected.
    pub fn swap(&self, index: Index) {
        *self.index.write().expect("index lock poisoned") = Arc::new(index);
    }

    fn snapshot(&self) -> Arc<Index> {
        self.index.read().expect("index lock poisoned").clone()
    }

    /// Rebuild the index from the current snapshot with `mutate` applied to every record,
    /// then swap it in - the copy-on-write the roster refresh uses, shared by the identity
    /// link and unlink. A record mapped by both id and handle is collected from both maps;
    /// the duplicates collapse in [`Index::from_records`], the single dedup point, so no
    /// pre-dedup is needed.
    fn rebuild_records(&self, mutate: impl FnMut(&mut MemberRecord)) {
        let mut records: Vec<MemberRecord> = {
            let snap = self.snapshot();
            snap.by_id
                .values()
                .chain(snap.by_handle.values())
                .cloned()
                .collect()
        };
        records.iter_mut().for_each(mutate);
        self.swap(Index::from_records(records));
    }
}

#[async_trait]
impl MemberStore for InMemoryStore {
    type Error = Infallible;

    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_id(id))
    }

    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_handle(handle))
    }

    async fn all_records(&self) -> Result<Vec<MemberRecord>, Infallible> {
        let snap = self.snapshot();
        // Collect from both maps then dedup: a record with both a Discord id and a handle
        // appears in both maps and collapses to one entry via `Index::from_records`'s rule.
        let records: Vec<MemberRecord> = snap
            .by_id
            .values()
            .chain(snap.by_handle.values())
            .cloned()
            .collect();
        Ok(dedup_records(records))
    }
}

#[async_trait]
impl RosterWrite for InMemoryStore {
    type Error = Infallible;

    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Infallible> {
        let index = Index::from_records(records);
        // Mirror PgStore: never overwrite a populated roster with an empty sweep.
        if index.is_empty() {
            return Ok(());
        }
        self.swap(index);
        Ok(())
    }
}

#[async_trait]
impl IdentityWrite for InMemoryStore {
    type Error = Infallible;

    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Infallible> {
        // Update the one record keyed by `st_user_id` with the discovered identity.
        self.rebuild_records(|rec| {
            if rec.st_user_id == *st_user_id {
                rec.discord_user_id = Some(discord_id);
                rec.discord_handle = Some(handle.clone());
            }
        });
        Ok(())
    }

    /// Clear the Discord identity from the record holding `discord_id`. With both identity
    /// columns cleared the record falls out of both index maps, so a later lookup misses by
    /// id and handle alike.
    async fn unlink_by_discord_id(&self, discord_id: DiscordUserId) -> Result<(), Infallible> {
        self.rebuild_records(|rec| {
            if rec.discord_user_id == Some(discord_id) {
                rec.discord_user_id = None;
                rec.discord_handle = None;
            }
        });
        Ok(())
    }
}

#[async_trait]
impl OverrideLog for InMemoryStore {
    type Error = Infallible;

    async fn stamp_override(
        &self,
        subject: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), Infallible> {
        // Insert-once: the first approver and their note win, so a re-stamp preserves the
        // original.
        self.overrides
            .write()
            .expect("overrides lock poisoned")
            .entry(subject.0)
            .or_insert(OverrideRecord {
                approved_by: approver,
                approved_at: Utc::now(),
                note,
            });
        Ok(())
    }

    async fn get_override(
        &self,
        subject: DiscordUserId,
    ) -> Result<Option<OverrideRecord>, Infallible> {
        Ok(self
            .overrides
            .read()
            .expect("overrides lock poisoned")
            .get(&subject.0)
            .cloned())
    }

    async fn delete_override(&self, subject: DiscordUserId) -> Result<(), Infallible> {
        self.overrides
            .write()
            .expect("overrides lock poisoned")
            .remove(&subject.0);
        Ok(())
    }
}

#[async_trait]
impl ConfigStore for InMemoryStore {
    type Error = Infallible;

    async fn load_config(&self, _guild: DiscordGuildId) -> Result<GuildConfig, Infallible> {
        Ok(self.config.read().expect("config lock poisoned").clone())
    }

    async fn save_config(
        &self,
        _guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Infallible> {
        *self.config.write().expect("config lock poisoned") = config.clone();
        Ok(())
    }
}

#[async_trait]
impl BulkSessionStore for InMemoryStore {
    type Error = Infallible;

    async fn load_session(
        &self,
        _guild: DiscordGuildId,
    ) -> Result<Option<BulkSession>, Infallible> {
        Ok(self
            .bulk
            .read()
            .expect("bulk lock poisoned")
            .as_ref()
            .map(|(s, _)| s.clone()))
    }

    async fn start_session(
        &self,
        session: &BulkSession,
        misses: &[BulkQueueEntry],
    ) -> Result<(), Infallible> {
        let queue = misses.iter().map(|m| (m.position, m.clone())).collect();
        *self.bulk.write().expect("bulk lock poisoned") = Some((session.clone(), queue));
        Ok(())
    }

    async fn next_pending(
        &self,
        _guild: DiscordGuildId,
    ) -> Result<Option<BulkQueueEntry>, Infallible> {
        Ok(self
            .bulk
            .read()
            .expect("bulk lock poisoned")
            .as_ref()
            .and_then(|(_, q)| q.values().find(|m| m.state == MissState::Pending).cloned()))
    }

    async fn mark_miss(
        &self,
        _guild: DiscordGuildId,
        member: DiscordUserId,
        state: MissState,
    ) -> Result<(), Infallible> {
        let mut guard = self.bulk.write().expect("bulk lock poisoned");
        if let Some((session, q)) = guard.as_mut()
            && let Some(miss) = q.values_mut().find(|m| m.discord_user_id == member)
        {
            miss.state = state;
            session.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn counts(&self, _guild: DiscordGuildId) -> Result<MissCounts, Infallible> {
        let guard = self.bulk.read().expect("bulk lock poisoned");
        let mut counts = MissCounts::default();
        if let Some((_, q)) = guard.as_ref() {
            for m in q.values() {
                match m.state {
                    MissState::Pending => counts.pending += 1,
                    MissState::Verified => counts.verified += 1,
                    MissState::Skipped => counts.skipped += 1,
                }
            }
        }
        Ok(counts)
    }

    async fn complete_session(&self, _guild: DiscordGuildId) -> Result<(), Infallible> {
        if let Some((s, _)) = self.bulk.write().expect("bulk lock poisoned").as_mut() {
            s.status = BulkStatus::Complete;
            s.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn abandon_session(&self, _guild: DiscordGuildId) -> Result<(), Infallible> {
        if let Some((s, _)) = self.bulk.write().expect("bulk lock poisoned").as_mut() {
            s.status = BulkStatus::Abandoned;
            s.updated_at = Utc::now();
        }
        Ok(())
    }
}

/// Persist and recall whole-guild channel-permission snapshots - the save/restore
/// behind the terraform's disaster recovery. Fallible from the start, like the
/// other store traits; the in-memory impl's [`Error`](ChannelSnapshotStore::Error)
/// is [`Infallible`].
#[async_trait]
pub trait ChannelSnapshotStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Append a snapshot (history is kept; never overwrite an earlier one).
    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), Self::Error>;

    /// The most recent snapshot for `guild`, or `None` if none was ever saved.
    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, Self::Error>;

    /// All snapshots' metadata for `guild`, newest first - for the restore picker.
    async fn list_snapshots(&self, guild: DiscordGuildId)
    -> Result<Vec<SnapshotMeta>, Self::Error>;
}

#[async_trait]
impl ChannelSnapshotStore for InMemoryStore {
    type Error = Infallible;

    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), Infallible> {
        self.snapshots
            .write()
            .expect("snapshots lock poisoned")
            .push(snapshot.clone());
        Ok(())
    }

    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, Infallible> {
        Ok(self
            .snapshots
            .read()
            .expect("snapshots lock poisoned")
            .iter()
            .rfind(|s| s.guild_id == guild)
            .cloned())
    }

    async fn list_snapshots(&self, guild: DiscordGuildId) -> Result<Vec<SnapshotMeta>, Infallible> {
        let guard = self.snapshots.read().expect("snapshots lock poisoned");
        let mut metas: Vec<SnapshotMeta> = guard
            .iter()
            .filter(|s| s.guild_id == guild)
            .map(|s| SnapshotMeta {
                saved_at: s.saved_at,
                channel_count: s.channels.len(),
            })
            .collect();
        // Newest first for the restore picker.
        metas.reverse();
        Ok(metas)
    }
}

#[async_trait]
impl GraceStore for InMemoryStore {
    type Error = Infallible;

    async fn active_grace(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        today: NaiveDate,
    ) -> Result<bool, Infallible> {
        Ok(self
            .grace
            .read()
            .expect("grace lock poisoned")
            .get(&id.0)
            .map(|g| g.until >= today)
            .unwrap_or(false))
    }

    async fn grace_override(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<GraceOverride>, Infallible> {
        Ok(self
            .grace
            .read()
            .expect("grace lock poisoned")
            .get(&id.0)
            .cloned())
    }

    async fn set_grace(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        until: NaiveDate,
        granted_by: DiscordUserId,
        reason: Option<String>,
    ) -> Result<(), Infallible> {
        self.grace.write().expect("grace lock poisoned").insert(
            id.0,
            GraceOverride {
                until,
                granted_by,
                granted_at: Utc::now(),
                reason,
            },
        );
        Ok(())
    }

    async fn clear_grace(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), Infallible> {
        self.grace
            .write()
            .expect("grace lock poisoned")
            .remove(&id.0);
        Ok(())
    }
}

#[async_trait]
impl ReminderStore for InMemoryStore {
    type Error = Infallible;

    async fn reminder_state(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<ReminderCycleState>, Infallible> {
        Ok(self
            .reminder_state
            .read()
            .expect("reminder_state lock poisoned")
            .get(&id.0)
            .cloned())
    }

    async fn record_sent(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        milestone: crate::reminders::Milestone,
        thread_id: i64,
    ) -> Result<(), Infallible> {
        let mut guard = self
            .reminder_state
            .write()
            .expect("reminder_state lock poisoned");
        let entry = guard.entry(id.0).or_insert(ReminderCycleState {
            cycle_xdate,
            last_sent: None,
            snoozed: false,
            thread_id: None,
        });
        // New cycle: reset snooze; same cycle: preserve it.
        if entry.cycle_xdate != cycle_xdate {
            entry.snoozed = false;
        }
        entry.cycle_xdate = cycle_xdate;
        entry.last_sent = Some(milestone);
        entry.thread_id = Some(thread_id);
        Ok(())
    }

    async fn set_thread(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        thread_id: i64,
    ) -> Result<(), Infallible> {
        let mut guard = self
            .reminder_state
            .write()
            .expect("reminder_state lock poisoned");
        let entry = guard.entry(id.0).or_insert(ReminderCycleState {
            cycle_xdate,
            last_sent: None,
            snoozed: false,
            thread_id: None,
        });
        // Preserve the cycle, last_sent, and snooze; only stamp the thread id.
        entry.thread_id = Some(thread_id);
        Ok(())
    }

    async fn set_snooze(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
    ) -> Result<(), Infallible> {
        let mut guard = self
            .reminder_state
            .write()
            .expect("reminder_state lock poisoned");
        let entry = guard.entry(id.0).or_insert(ReminderCycleState {
            cycle_xdate,
            last_sent: None,
            snoozed: false,
            thread_id: None,
        });
        entry.cycle_xdate = cycle_xdate;
        entry.snoozed = true;
        Ok(())
    }

    async fn is_opted_out(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<bool, Infallible> {
        Ok(self
            .opt_out
            .read()
            .expect("opt_out lock poisoned")
            .contains_key(&id.0))
    }

    async fn opt_out(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        source: OptOutSource,
    ) -> Result<(), Infallible> {
        self.opt_out
            .write()
            .expect("opt_out lock poisoned")
            .insert(id.0, source);
        Ok(())
    }

    async fn clear_opt_out(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), Infallible> {
        self.opt_out
            .write()
            .expect("opt_out lock poisoned")
            .remove(&id.0);
        Ok(())
    }

    async fn last_reminder_run(
        &self,
        _guild: DiscordGuildId,
    ) -> Result<Option<DateTime<Utc>>, Infallible> {
        Ok(*self
            .last_reminder_run
            .read()
            .expect("last_reminder_run lock poisoned"))
    }

    async fn set_last_reminder_run(
        &self,
        _guild: DiscordGuildId,
        at: DateTime<Utc>,
    ) -> Result<(), Infallible> {
        *self
            .last_reminder_run
            .write()
            .expect("last_reminder_run lock poisoned") = Some(at);
        Ok(())
    }
}

#[async_trait]
impl ReminderTemplates for InMemoryStore {
    type Error = Infallible;

    async fn template(
        &self,
        _guild: DiscordGuildId,
        kind: crate::reminders::ReminderTemplateKind,
    ) -> Result<Option<String>, Infallible> {
        Ok(self
            .templates
            .read()
            .expect("templates lock poisoned")
            .get(kind.as_token())
            .cloned())
    }

    async fn set_template(
        &self,
        _guild: DiscordGuildId,
        kind: crate::reminders::ReminderTemplateKind,
        body: String,
    ) -> Result<(), Infallible> {
        self.templates
            .write()
            .expect("templates lock poisoned")
            .insert(kind.as_token().to_string(), body);
        Ok(())
    }
}

/// Sweep the Solidarity Tech user list (pre-filtered to Discord-linked members)
/// into the flat [`MemberRecord`]s the cache stores. The store-agnostic half of the
/// refresh: the caller hands the result to [`RosterWrite::replace_roster`].
pub async fn sweep_roster(
    st: &impl SolidarityTechClient,
    list_id: &str,
) -> crate::Result<Vec<MemberRecord>> {
    let st_members = drain_pages(
        &NoProgress,
        "solidarity tech discord list",
        |cursor| async move { st.members_in_list_page(list_id, cursor.as_deref()).await },
    )
    .await?;
    tracing::info!(
        members = st_members.len(),
        list_id,
        "fetched discord-list members from solidarity tech"
    );
    Ok(st_members.into_iter().map(MemberRecord::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::solidarity_tech::SolidarityTechMember;
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use chrono::NaiveDate;
    use domain::{MembershipStatus, MigsStatus, Role};

    use crate::backends::solidarity_tech::FakeSolidarityTech;

    #[tokio::test]
    async fn sweep_roster_fetches_the_discord_list() {
        let st_client = FakeSolidarityTech::new().with_members(vec![st("zoop", 42, "zoop")]);
        let records = sweep_roster(&st_client, "1234").await.unwrap();
        assert!(
            records
                .iter()
                .any(|r| r.discord_user_id == Some(DiscordUserId(42)))
        );
    }

    #[test]
    fn st_member_maps_into_record() {
        let st = SolidarityTechMember {
            id: StUserId("1".into()),
            email: Email("a@b.com".into()),
            first_name: Some("zoop".into()),
            discord_handle: Some(DiscordHandle("zoop".into())),
            discord_user_id: Some(DiscordUserId(42)),
            membership_standing: Some(MigsStatus::MemberInGoodStanding),
            xdate: NaiveDate::from_ymd_opt(2026, 12, 31),
            join_date: NaiveDate::from_ymd_opt(2021, 3, 15),
            ..Default::default()
        };
        let r = MemberRecord::from(st);
        assert_eq!(r.discord_user_id, Some(DiscordUserId(42)));
        assert_eq!(r.email.as_str(), "a@b.com");
        assert_eq!(r.full_name.as_deref(), Some("zoop"));
        assert_eq!(r.standing, Some(MigsStatus::MemberInGoodStanding));
        assert_eq!(Role::try_from(r.membership()), Ok(Role::Member));
        assert_eq!(r.join_date, NaiveDate::from_ymd_opt(2021, 3, 15));
    }

    #[test]
    fn full_name_combines_first_and_last() {
        let st = SolidarityTechMember {
            id: StUserId("9".into()),
            email: Email("z@b.com".into()),
            first_name: Some("zoop".into()),
            last_name: Some("goop".into()),
            ..Default::default()
        };
        assert_eq!(
            MemberRecord::from(st).full_name.as_deref(),
            Some("zoop goop")
        );
    }

    fn base_st() -> SolidarityTechMember {
        SolidarityTechMember {
            id: StUserId("base".into()),
            email: Email("base@test.com".into()),
            ..Default::default()
        }
    }

    #[test]
    fn membership_is_malformed_when_standing_absent() {
        let st = SolidarityTechMember {
            membership_standing: None,
            ..base_st()
        };
        assert_eq!(
            MemberRecord::from(st).membership(),
            MembershipStatus::Malformed
        );
    }

    fn st(handle: &str, id: u64, name: &str) -> SolidarityTechMember {
        SolidarityTechMember {
            id: StUserId(id.to_string()),
            email: Email(format!("{name}@st.test")),
            first_name: Some(name.into()),
            discord_handle: Some(DiscordHandle(handle.into())),
            discord_user_id: Some(DiscordUserId(id)),
            membership_standing: Some(MigsStatus::MemberInGoodStanding),
            ..Default::default()
        }
    }

    #[test]
    fn index_looks_up_by_id() {
        let idx = Index::build(vec![st("zoop", 42, "zoop")]);
        assert_eq!(
            idx.by_id(DiscordUserId(42)).unwrap().email.as_str(),
            "zoop@st.test"
        );
        assert!(idx.by_id(DiscordUserId(99)).is_none());
    }

    #[tokio::test]
    async fn in_memory_store_reads_and_swaps() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some()
        );
        // Swap in an index that no longer contains 42.
        store.swap(Index::build(vec![st("rose", 99, "rose")]));
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .by_discord_id(DiscordUserId(99))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn empty_roster_does_not_wipe_a_populated_store() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        // An empty sweep must be a no-op, not a wipe.
        store.replace_roster(vec![]).await.unwrap();
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some(),
            "empty replace_roster must preserve the existing roster"
        );
    }

    #[tokio::test]
    async fn roster_of_only_unlinked_records_does_not_wipe() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        // Records with neither a Discord id nor a handle are unstorable, leaving an empty
        // index - which must be treated the same as an empty sweep, not as a wipe.
        let unlinked = MemberRecord {
            st_user_id: StUserId("ghost-1".into()),
            discord_user_id: None,
            discord_handle: None,
            email: Email("ghost@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        };
        store.replace_roster(vec![unlinked]).await.unwrap();
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some(),
            "a roster with no linkable members must preserve the existing roster"
        );
    }

    #[tokio::test]
    async fn config_round_trips_through_in_memory_store() {
        use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId};
        let store = InMemoryStore::new(Index::default_for_test());
        let guild = DiscordGuildId(7);
        // Default is all-unset.
        assert_eq!(
            store.load_config(guild).await.unwrap(),
            GuildConfig::default()
        );
        let cfg = GuildConfig {
            moderator_role: Some(DiscordRoleId(10)),
            member_role: Some(DiscordRoleId(11)),
            mod_approval_channel: Some(DiscordChannelId(20)),
            ..Default::default()
        };
        store.save_config(guild, &cfg).await.unwrap();
        assert_eq!(store.load_config(guild).await.unwrap(), cfg);
    }

    #[tokio::test]
    async fn get_override_round_trips_stamp() {
        let store = InMemoryStore::new(Index::default());
        assert!(
            store
                .get_override(DiscordUserId(7))
                .await
                .unwrap()
                .is_none()
        );
        store
            .stamp_override(DiscordUserId(7), DiscordUserId(99), None)
            .await
            .unwrap();
        let got = store.get_override(DiscordUserId(7)).await.unwrap().unwrap();
        assert_eq!(got.approved_by, DiscordUserId(99));
    }

    #[tokio::test]
    async fn stamp_override_records_and_preserves_the_note() {
        let store = InMemoryStore::new(Index::default());
        store
            .stamp_override(
                DiscordUserId(7),
                DiscordUserId(99),
                Some("vouched in person".into()),
            )
            .await
            .unwrap();
        // Insert-once preserves the first note even if a later stamp carries another.
        store
            .stamp_override(
                DiscordUserId(7),
                DiscordUserId(1),
                Some("a later note".into()),
            )
            .await
            .unwrap();
        let got = store.get_override(DiscordUserId(7)).await.unwrap().unwrap();
        assert_eq!(got.approved_by, DiscordUserId(99));
        assert_eq!(got.note.as_deref(), Some("vouched in person"));
    }

    #[test]
    fn bulk_enum_tokens_round_trip() {
        for s in [BulkScope::UnmanagedOnly, BulkScope::WholeGuild] {
            assert_eq!(BulkScope::from_token(s.as_token()), Some(s));
        }
        for s in [
            BulkStatus::InProgress,
            BulkStatus::Complete,
            BulkStatus::Abandoned,
        ] {
            assert_eq!(BulkStatus::from_token(s.as_token()), Some(s));
        }
        for s in [MissState::Pending, MissState::Verified, MissState::Skipped] {
            assert_eq!(MissState::from_token(s.as_token()), Some(s));
        }
        assert_eq!(BulkScope::from_token("nonsense"), None);
        assert_eq!(BulkStatus::from_token("nonsense"), None);
        assert_eq!(MissState::from_token("nonsense"), None);
    }

    // Helpers for snapshot tests.
    fn empty_snapshot(guild: u64, saved_at: chrono::DateTime<Utc>) -> ChannelSnapshot {
        ChannelSnapshot {
            format_version: crate::channels::snapshot::SNAPSHOT_FORMAT_VERSION,
            guild_id: domain::DiscordGuildId(guild),
            saved_at,
            channels: vec![],
        }
    }

    #[tokio::test]
    async fn snapshot_save_latest_and_list() {
        use chrono::TimeZone;
        let store = InMemoryStore::new(Index::default_for_test());
        let guild = domain::DiscordGuildId(100);
        let other = domain::DiscordGuildId(999);

        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        // Nothing saved yet.
        assert!(store.latest_snapshot(guild).await.unwrap().is_none());
        assert!(store.list_snapshots(guild).await.unwrap().is_empty());

        let s1 = empty_snapshot(guild.0, t1);
        let s2 = empty_snapshot(guild.0, t2);
        let s_other = empty_snapshot(other.0, t1);

        store.save_snapshot(&s1).await.unwrap();
        store.save_snapshot(&s_other).await.unwrap(); // different guild - must not affect guild
        store.save_snapshot(&s2).await.unwrap();

        // latest returns the most recently saved for this guild.
        assert_eq!(
            store.latest_snapshot(guild).await.unwrap(),
            Some(s2.clone())
        );

        // list returns newest first.
        let metas = store.list_snapshots(guild).await.unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].saved_at, t2);
        assert_eq!(metas[1].saved_at, t1);

        // other guild has its own snapshot, not leaking into guild.
        assert_eq!(store.latest_snapshot(other).await.unwrap(), Some(s_other));
        assert_eq!(store.list_snapshots(other).await.unwrap().len(), 1);
    }
}
