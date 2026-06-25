//! Moderator-driven stores: the manual-override stamp, the grace override, and the
//! resumable bulk-verify session with its frozen queue.
//!
//! All three are keyed on the immutable Discord id and are written by hand by a moderator
//! (override, grace) or driven through a moderator wizard (bulk). The [`InMemoryStore`] impls
//! of [`OverrideLog`]/[`GraceStore`]/[`BulkSessionStore`] reach the store's private fields
//! from the hub.

use std::convert::Infallible;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use domain::DiscordGuildId;

use crate::util::{DiscordHandle, DiscordUserId};

use super::InMemoryStore;

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

/// A moderator grace stamp: hold this member at `Member` until `until` (inclusive),
/// ignoring their dues. Active while `until >= today`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraceOverride {
    pub until: NaiveDate,
    pub granted_by: DiscordUserId,
    pub granted_at: DateTime<Utc>,
    pub reason: Option<String>,
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
/// [`RosterWrite::replace_roster`](super::RosterWrite::replace_roster)), so the Postgres
/// impl is granted `DELETE`. Fallible/async from the start; the in-memory impl's error is
/// [`Infallible`].
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
