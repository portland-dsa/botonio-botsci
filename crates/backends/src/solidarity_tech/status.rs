//! Solidarity Tech's dues-status and membership-type enums and their wire decode.

use super::error::SolidarityTechError;

/// One dues-cadence column's value ("Monthly Dues Status" / "Yearly Dues
/// Status"), as the verification layer cares about it.
///
/// A **closed** set: an unrecognized wire string is an error
/// ([`SolidarityTechError::UnknownDuesStatus`]), never a catch-all variant, so a
/// never-seen-before dues option fails loudly instead of reading as a default.
/// Solidarity Tech reports a finer-grained raw value (eight of them); the private
/// `DuesStatusRaw` proxy decodes each exactly, then collapses into this type.
/// The sub-distinctions within `Overdue` and `Cancelled` are intentionally
/// dropped at the public level - the authoritative role path is
/// `MigsStatus -> MembershipStatus -> Role`; dues columns are reserved for the
/// bot's renewal reminders, which only need "active vs. not".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuesStatus {
    /// `active` - a currently-recurring subscription. The only current value.
    Active,
    /// `never` - this member has never paid dues of this cadence.
    Never,
    /// `overdue` or `past_due` - the member has fallen behind on payment.
    Overdue,
    /// Any `cancelled_by_*` wire value - the subscription was cancelled.
    Cancelled,
}

impl DuesStatus {
    /// Whether this cadence counts as currently active. Only `Active` is current.
    pub fn current(&self) -> bool {
        matches!(self, DuesStatus::Active)
    }
}

/// Literal one-to-one decode of a raw Solidarity Tech dues-status string: one
/// variant per wire value, no catch-all.
///
/// Kept private - it exists only to make the [`DuesStatus`] mapping exhaustive
/// and to localize the exact wire spellings in one place (note that
/// `canceled_by_failure` uses a single `l`, unlike the other `cancelled_*`).
enum DuesStatusRaw {
    Active,
    Never,
    Overdue,
    PastDue,
    CancelledByProcessor,
    CancelledByUser,
    CancelledByAdmin,
    CanceledByFailure,
}

impl DuesStatusRaw {
    /// Match a raw wire string to its proxy variant, or `None` if unrecognized.
    fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Self::Active,
            "never" => Self::Never,
            "overdue" => Self::Overdue,
            "past_due" => Self::PastDue,
            "cancelled_by_processor" => Self::CancelledByProcessor,
            "cancelled_by_user" => Self::CancelledByUser,
            "cancelled_by_admin" => Self::CancelledByAdmin,
            // Single-l "canceled" is the actual wire value, not a typo.
            "canceled_by_failure" => Self::CanceledByFailure,
            _ => return None,
        })
    }
}

impl From<DuesStatusRaw> for DuesStatus {
    fn from(raw: DuesStatusRaw) -> Self {
        match raw {
            DuesStatusRaw::Active => DuesStatus::Active,
            DuesStatusRaw::Never => DuesStatus::Never,
            DuesStatusRaw::Overdue | DuesStatusRaw::PastDue => DuesStatus::Overdue,
            DuesStatusRaw::CancelledByProcessor
            | DuesStatusRaw::CancelledByUser
            | DuesStatusRaw::CancelledByAdmin
            | DuesStatusRaw::CanceledByFailure => DuesStatus::Cancelled,
        }
    }
}

impl TryFrom<&str> for DuesStatus {
    type Error = SolidarityTechError;

    /// Decode a raw dues-status string, mapping an unrecognized value to a hard
    /// [`SolidarityTechError::UnknownDuesStatus`] that retains the offending text.
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        DuesStatusRaw::from_wire(s)
            .map(DuesStatus::from)
            .ok_or_else(|| SolidarityTechError::UnknownDuesStatus(s.to_string()))
    }
}

/// The membership cadence from the `membership-type` custom property - the
/// field the bot customizes its dues reminders per.
///
/// A **closed** set of the four cadences the chapter offers; an unrecognized
/// value is a [`SolidarityTechError::UnknownMembershipType`], never a catch-all
/// variant, so a never-seen cadence fails loudly instead of reading as a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipType {
    /// `monthly`.
    Monthly,
    /// `yearly`.
    Yearly,
    /// `one-time`.
    OneTime,
    /// `income-based`.
    IncomeBased,
}

impl TryFrom<&str> for MembershipType {
    type Error = SolidarityTechError;

    /// Decode a raw `membership-type` value, mapping an unrecognized value to a
    /// [`SolidarityTechError::UnknownMembershipType`] that retains the text.
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Ok(match s {
            "monthly" => Self::Monthly,
            "yearly" => Self::Yearly,
            "one-time" => Self::OneTime,
            "income-based" => Self::IncomeBased,
            _ => return Err(SolidarityTechError::UnknownMembershipType(s.to_string())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dues_status_decodes_every_wire_value() {
        // All 8 raw wire values still map explicitly (no catch-all arm in DuesStatusRaw).
        let cases = [
            ("active", DuesStatus::Active),
            ("never", DuesStatus::Never),
            ("overdue", DuesStatus::Overdue),
            ("past_due", DuesStatus::Overdue),
            ("cancelled_by_processor", DuesStatus::Cancelled),
            ("cancelled_by_user", DuesStatus::Cancelled),
            ("cancelled_by_admin", DuesStatus::Cancelled),
            // Single-l "canceled" is the real wire spelling for this one.
            ("canceled_by_failure", DuesStatus::Cancelled),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                DuesStatus::try_from(raw).unwrap(),
                expected,
                "decoding {raw:?}"
            );
        }
    }

    #[test]
    fn only_active_is_current() {
        assert!(DuesStatus::Active.current());
        for s in [
            DuesStatus::Never,
            DuesStatus::Overdue,
            DuesStatus::Cancelled,
        ] {
            assert!(!s.current(), "{s:?} should not be current");
        }
    }

    #[test]
    fn membership_type_decodes_every_value() {
        use MembershipType::*;
        for (raw, expected) in [
            ("monthly", Monthly),
            ("yearly", Yearly),
            ("one-time", OneTime),
            ("income-based", IncomeBased),
        ] {
            assert_eq!(
                MembershipType::try_from(raw).unwrap(),
                expected,
                "decoding {raw:?}"
            );
        }
    }
}
