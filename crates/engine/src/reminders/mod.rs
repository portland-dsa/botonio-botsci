//! The dues-reminder decision core: which member is due which renewal nudge, decided
//! purely from the cached roster, today's date, and the per-member reminder state.
//! Network-free, mirroring [`crate::scan`] - the bot owns the threads and the sends.

mod plan;
pub use plan::{plan, select_milestone};

use crate::backends::solidarity_tech::MembershipType;
use crate::util::DiscordUserId;

/// One point on a member's lapse timeline, ordered by urgency (the variant order is the
/// `Ord` order): the three pre-lapse nudges and the post-lapse notice. The single ordered
/// marker `Option<Milestone>` in [`ReminderCycleState`](crate::store::ReminderCycleState)
/// records how far a member has been notified this cycle; a milestone is due only when it
/// is strictly more urgent than that marker.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Milestone {
    /// ~30 days before the lapse date.
    Days30,
    /// ~14 days before.
    Days14,
    /// The day before (1 day out).
    Day1,
    /// The dues date has passed; the one notice that ignores snooze and opt-out.
    Expired,
}

impl Milestone {
    /// The three pre-lapse milestones, longest lead first.
    pub const PRE_LAPSE: [Milestone; 3] = [Milestone::Days30, Milestone::Days14, Milestone::Day1];

    /// Days of lead time before the lapse date this milestone marks, or `None` for
    /// [`Expired`](Milestone::Expired), which fires after the date rather than before it.
    pub fn lead_days(self) -> Option<i64> {
        match self {
            Milestone::Days30 => Some(30),
            Milestone::Days14 => Some(14),
            Milestone::Day1 => Some(1),
            Milestone::Expired => None,
        }
    }

    /// The stable token stored in `dues_reminder_state.last_sent`.
    pub fn as_token(self) -> &'static str {
        match self {
            Milestone::Days30 => "days30",
            Milestone::Days14 => "days14",
            Milestone::Day1 => "day1",
            Milestone::Expired => "expired",
        }
    }

    /// Decode a stored token; `None` for an unrecognized value (the caller turns that
    /// into a typed error, never a silent guess), as `MissState::from_token` does.
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "days30" => Some(Milestone::Days30),
            "days14" => Some(Milestone::Days14),
            "day1" => Some(Milestone::Day1),
            "expired" => Some(Milestone::Expired),
            _ => None,
        }
    }
}

/// Which dues type's template a reminder uses; `Expired` is its own single template,
/// not per-type. Built from a member's [`MembershipType`] for a pre-lapse nudge, or
/// fixed to `Expired` for the lapse notice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReminderTemplateKind {
    Monthly,
    Yearly,
    OneTime,
    IncomeBased,
    Expired,
}

impl ReminderTemplateKind {
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
            Self::OneTime => "one_time",
            Self::IncomeBased => "income_based",
            Self::Expired => "expired",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "monthly" => Some(Self::Monthly),
            "yearly" => Some(Self::Yearly),
            "one_time" => Some(Self::OneTime),
            "income_based" => Some(Self::IncomeBased),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// The pre-lapse template kind for a member's dues cadence. (The `Expired` kind is
    /// chosen by the caller for the lapse notice, not derived from a type.)
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
/// the template), and the true days remaining (for the rendered deadline line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueReminder {
    pub id: DiscordUserId,
    pub milestone: Milestone,
    pub membership_type: Option<MembershipType>,
    pub days_until: i64,
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
        assert!(Milestone::Days30 < Milestone::Days14);
        assert!(Milestone::Days14 < Milestone::Day1);
        assert!(Milestone::Day1 < Milestone::Expired);
    }
}
