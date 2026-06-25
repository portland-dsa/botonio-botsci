//! Dues-reminder bookkeeping: the per-member cycle state, the permanent opt-out, and the
//! editable per-kind message bodies.
//!
//! [`ReminderStore`] tracks where each member is in their renewal cycle and whether they
//! opted out; [`MessageTemplates`] holds the moderator-edited bodies (the render layer
//! supplies a built-in default when a row is absent). Both [`InMemoryStore`] impls reach the
//! store's private fields from the hub.

use std::convert::Infallible;

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::DiscordGuildId;

use crate::reminders::{MessageKind, Milestone};
use crate::util::DiscordUserId;

use super::InMemoryStore;

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
/// `expiring_marked` to a specific lapse date: when the member's record shows a different
/// `expires`, the cycle has turned and the planner treats `last_sent`/`expiring_marked` as
/// reset. `thread_id` is the member's lifecycle thread and survives the reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderCycleState {
    pub cycle_xdate: NaiveDate,
    pub last_sent: Option<Milestone>,
    /// Whether the Dues Expiring marker role is currently held for this cycle. The sweep
    /// flips this on grant and removal so the marker is written to Discord once on entry
    /// and once on exit, not every pass.
    pub expiring_marked: bool,
    pub thread_id: Option<i64>,
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
    /// `thread_id`. Upserts: sets `last_sent = milestone`, `cycle_xdate`, `thread_id`;
    /// when `cycle_xdate` differs from the stored one, resets `expiring_marked` to false
    /// and clears `last_sent`. `expiring_marked` is otherwise untouched.
    async fn record_sent(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        milestone: Milestone,
        thread_id: i64,
    ) -> Result<(), Self::Error>;

    /// Persist a member's lifecycle thread id without recording a send. Saves a freshly created
    /// thread before its first message goes out, so a later send/record failure reuses that thread
    /// rather than orphaning it and creating a duplicate next sweep. Upserts: sets `thread_id`,
    /// preserving `last_sent`/`expiring_marked`/`cycle_xdate`; a new row is seeded with
    /// `cycle_xdate` and no recorded milestone.
    async fn set_thread(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        thread_id: i64,
    ) -> Result<(), Self::Error>;

    /// Set the Dues Expiring marker flag for the member's current cycle (upserting the
    /// row, seeded with `cycle_xdate`, if needed). The sweep flips this on grant/removal
    /// so a marker is written to Discord once on entry and once on exit, not every pass.
    async fn set_expiring_marked(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        marked: bool,
    ) -> Result<(), Self::Error>;

    /// Every member whose cycle state currently has `expiring_marked = true`. Drives the
    /// one-time cleanup when reminders are disabled.
    async fn marked_members(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Vec<DiscordUserId>, Self::Error>;

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
}

/// Read a guild's editable per-type message template bodies. An absent row is `None`;
/// the bot's render layer supplies the built-in default.
#[async_trait]
pub trait MessageTemplates: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// The stored body for `kind`, or `None` to use the built-in default.
    async fn template(
        &self,
        guild: DiscordGuildId,
        kind: MessageKind,
    ) -> Result<Option<String>, Self::Error>;

    /// Upsert the body for `kind`.
    async fn set_template(
        &self,
        guild: DiscordGuildId,
        kind: MessageKind,
        body: String,
    ) -> Result<(), Self::Error>;
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
        milestone: Milestone,
        thread_id: i64,
    ) -> Result<(), Infallible> {
        let mut guard = self
            .reminder_state
            .write()
            .expect("reminder_state lock poisoned");
        let entry = guard.entry(id.0).or_insert(ReminderCycleState {
            cycle_xdate,
            last_sent: None,
            expiring_marked: false,
            thread_id: None,
        });
        // New cycle: reset the marker and last_sent so the member enters fresh.
        if entry.cycle_xdate != cycle_xdate {
            entry.expiring_marked = false;
            entry.last_sent = None;
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
            expiring_marked: false,
            thread_id: None,
        });
        // Preserve the cycle, last_sent, and expiring_marked; only stamp the thread id.
        entry.thread_id = Some(thread_id);
        Ok(())
    }

    async fn set_expiring_marked(
        &self,
        _guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        marked: bool,
    ) -> Result<(), Infallible> {
        let mut guard = self
            .reminder_state
            .write()
            .expect("reminder_state lock poisoned");
        let entry = guard.entry(id.0).or_insert(ReminderCycleState {
            cycle_xdate,
            last_sent: None,
            expiring_marked: false,
            thread_id: None,
        });
        entry.cycle_xdate = cycle_xdate;
        entry.expiring_marked = marked;
        Ok(())
    }

    async fn marked_members(
        &self,
        _guild: DiscordGuildId,
    ) -> Result<Vec<DiscordUserId>, Infallible> {
        let guard = self
            .reminder_state
            .read()
            .expect("reminder_state lock poisoned");
        let ids = guard
            .iter()
            .filter(|(_, s)| s.expiring_marked)
            .map(|(id, _)| DiscordUserId(*id))
            .collect();
        Ok(ids)
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
}

#[async_trait]
impl MessageTemplates for InMemoryStore {
    type Error = Infallible;

    async fn template(
        &self,
        _guild: DiscordGuildId,
        kind: MessageKind,
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
        kind: MessageKind,
        body: String,
    ) -> Result<(), Infallible> {
        self.templates
            .write()
            .expect("templates lock poisoned")
            .insert(kind.as_token().to_string(), body);
        Ok(())
    }
}
