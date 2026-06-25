//! The dues-reminder decision core: which member is due which renewal nudge, decided
//! purely from the cached roster, today's date, and the per-member reminder state.
//! Network-free, mirroring [`crate::scan`] - the bot owns the threads and the sends.

mod plan;
pub use plan::{plan, select_milestone};

use chrono::TimeDelta;

use crate::backends::solidarity_tech::MembershipType;
use crate::util::DiscordUserId;

/// The single pre-lapse window, in days. Entering it triggers both the renewal reminder
/// and the Dues Expiring role; past it (negative days) is a lapse.
pub const PRE_LAPSE_WINDOW_DAYS: i64 = 14;

/// Where a member sits relative to their dues lapse - the typed replacement for a bare
/// day count. Built from `xdate - today`. Distinct from `domain::MembershipStatus`
/// (Solidarity Tech standing): this is the dues timing axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExpiryStatus {
    /// More than the window away - dues current, no notice due.
    Current,
    /// Inside the window (`0 ..= PRE_LAPSE_WINDOW_DAYS` days left, expiry day included);
    /// carries the time left for the sweep's log line.
    Expiring { time_left: TimeDelta },
    /// Past the lapse date.
    Lapsed,
}

impl From<TimeDelta> for ExpiryStatus {
    /// Classify on whole days, so sub-day precision never makes "expires today" ambiguous.
    fn from(time_left: TimeDelta) -> Self {
        match time_left.num_days() {
            d if d > PRE_LAPSE_WINDOW_DAYS => Self::Current,
            d if d >= 0 => Self::Expiring { time_left },
            _ => Self::Lapsed,
        }
    }
}

/// One point on a member's lapse timeline, ordered by urgency (the variant order is the
/// `Ord` order): the pre-lapse renewal notice, then the post-lapse notice. The single
/// ordered marker `Option<Milestone>` in [`ReminderCycleState`](crate::store::ReminderCycleState)
/// records how far a member has been notified this cycle; a milestone is due only when it
/// is strictly more urgent than that marker.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Milestone {
    /// The single pre-lapse renewal notice, sent inside the 14-day window.
    Renewal,
    /// The dues date has passed; sent once on lapse regardless of opt-out and auto-renew.
    Lapse,
}

impl Milestone {
    /// The stable token stored in `dues_reminder_state.last_sent`.
    pub fn as_token(self) -> &'static str {
        match self {
            Milestone::Renewal => "renewal",
            Milestone::Lapse => "lapse",
        }
    }

    /// Decode a stored token; `None` for an unrecognized value (the caller turns that
    /// into a typed error, never a silent guess).
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "renewal" => Some(Milestone::Renewal),
            "lapse" => Some(Milestone::Lapse),
            _ => None,
        }
    }
}

/// Which template a message uses: the dues-cadence renewal templates and the non-dues
/// message kinds. Built from a member's [`MembershipType`] for a renewal notice, or
/// chosen directly for banner, unverified, and lapse notices.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageKind {
    Monthly,
    Yearly,
    OneTime,
    IncomeBased,
    Unverified,
    DuesBanner,
}

impl MessageKind {
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
            Self::OneTime => "one_time",
            Self::IncomeBased => "income_based",
            Self::Unverified => "unverified",
            Self::DuesBanner => "dues_banner",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "monthly" => Some(Self::Monthly),
            "yearly" => Some(Self::Yearly),
            "one_time" => Some(Self::OneTime),
            "income_based" => Some(Self::IncomeBased),
            "unverified" => Some(Self::Unverified),
            "dues_banner" => Some(Self::DuesBanner),
            _ => None,
        }
    }

    /// The renewal-notice template kind for a member's dues cadence. (The `Lapse` notice
    /// is chosen by the caller directly, not derived from a type.)
    pub fn from_membership_type(t: MembershipType) -> Self {
        match t {
            MembershipType::Monthly => Self::Monthly,
            MembershipType::Yearly => Self::Yearly,
            MembershipType::OneTime => Self::OneTime,
            MembershipType::IncomeBased => Self::IncomeBased,
        }
    }
}

/// One member's due reminder for this sweep: who, which milestone, the dues type (for
/// the template), and the lapse date (for the rendered deadline line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueReminder {
    pub id: DiscordUserId,
    pub milestone: Milestone,
    pub membership_type: Option<MembershipType>,
    pub xdate: chrono::NaiveDate,
}

/// The members due a reminder this sweep, in roster order. The bot applies each, paced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReminderPlan {
    pub due: Vec<DueReminder>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urgency_order_is_ascending() {
        assert!(Milestone::Renewal < Milestone::Lapse);
    }
}
