//! The chapter's raw "Membership Status" field - whether a member is in good
//! standing - as reported by Solidarity Tech.
//!
//! [`MigsStatus`] ("MIGS" = Member In Good Standing) is the *raw* field value,
//! distinct from the computed [`MembershipStatus`] it folds into. It is a
//! **closed** set of the two values still in use; the two
//! retired values (`Lapsed Member`, `Constitutional Member`) decode to a typed
//! [`MigsStatusError::Retired`], never a variant, so a record still carrying one
//! fails loudly instead of being silently mapped. The source->computed adapter
//! ([`From<MigsStatus>`](MigsStatus) for [`MembershipStatus`]) lives here because
//! both types are local to this crate.

use std::fmt;

use crate::membership::MembershipStatus;

/// The live values of the "Membership Status" field.
///
/// Only [`MemberInGoodStanding`](MigsStatus::MemberInGoodStanding) grants the
/// `Member` role; [`Lapsed`](MigsStatus::Lapsed) folds to `DuesExpired`. The two
/// retired values are deliberately absent - see [`decode`](MigsStatus::decode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigsStatus {
    /// `Member in Good Standing` - the only value that grants auto-approval.
    MemberInGoodStanding,
    /// `Lapsed` - membership has lapsed; does not qualify.
    Lapsed,
}

/// One of the two retired "Membership Status" values, kept typed so a decode
/// failure can say *which* retired value was seen without a bare string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetiredMigsStatus {
    /// `Lapsed Member` - believed unused.
    LapsedMember,
    /// `Constitutional Member` - policy was never defined.
    ConstitutionalMember,
}

impl RetiredMigsStatus {
    /// The exact string the source stores for this retired value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LapsedMember => "Lapsed Member",
            Self::ConstitutionalMember => "Constitutional Member",
        }
    }
}

impl fmt::Display for RetiredMigsStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a raw membership-status value failed to decode to a live [`MigsStatus`].
///
/// The string-bearing variants carry the offending text (no token/PII) so the
/// failure names the bad value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigsStatusError {
    /// The field was absent or empty.
    #[error("membership status field is missing or empty")]
    Missing,
    /// A value that is neither a live nor a retired status.
    #[error("unrecognized membership status: {0:?}")]
    Unknown(String),
    /// A recognized but retired value (`Lapsed Member` / `Constitutional Member`).
    #[error("retired membership status: {0}")]
    Retired(RetiredMigsStatus),
}

impl MigsStatus {
    /// Whether this status counts as good standing - `true` only for
    /// [`MemberInGoodStanding`](Self::MemberInGoodStanding). The single source of
    /// truth for the decisions that hinge on standing (the `Member` role).
    pub fn is_good(self) -> bool {
        matches!(self, Self::MemberInGoodStanding)
    }

    /// The exact string a source stores for this value - the inverse of
    /// [`decode`](Self::decode) for the two live values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemberInGoodStanding => "Member in Good Standing",
            Self::Lapsed => "Lapsed",
        }
    }

    /// Decode a raw membership-status field value.
    ///
    /// `None` or an empty string is [`Missing`](MigsStatusError::Missing); the two
    /// retired values are [`Retired`](MigsStatusError::Retired); anything else is
    /// [`Unknown`](MigsStatusError::Unknown). Only the two live values succeed -
    /// every caller decides for itself whether a given error is fatal.
    pub fn decode(raw: Option<&str>) -> Result<Self, MigsStatusError> {
        match raw.map(str::trim) {
            None | Some("") => Err(MigsStatusError::Missing),
            Some("Member in Good Standing") => Ok(Self::MemberInGoodStanding),
            Some("Lapsed") => Ok(Self::Lapsed),
            Some("Lapsed Member") => Err(MigsStatusError::Retired(RetiredMigsStatus::LapsedMember)),
            Some("Constitutional Member") => Err(MigsStatusError::Retired(
                RetiredMigsStatus::ConstitutionalMember,
            )),
            Some(other) => Err(MigsStatusError::Unknown(other.to_string())),
        }
    }
}

/// The good-standing half of the `source status -> MembershipStatus -> Role` chain.
/// `MemberInGoodStanding -> Member`, `Lapsed -> DuesExpired`. Lives here, with the
/// shared vocabulary, so the role decision never has to be rewritten per source.
impl From<MigsStatus> for MembershipStatus {
    fn from(status: MigsStatus) -> Self {
        match status {
            MigsStatus::MemberInGoodStanding => MembershipStatus::Member,
            MigsStatus::Lapsed => MembershipStatus::DuesExpired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_two_live_values() {
        assert_eq!(
            MigsStatus::decode(Some("Member in Good Standing")),
            Ok(MigsStatus::MemberInGoodStanding)
        );
        assert_eq!(MigsStatus::decode(Some("Lapsed")), Ok(MigsStatus::Lapsed));
    }

    #[test]
    fn retired_values_are_typed_errors() {
        assert_eq!(
            MigsStatus::decode(Some("Lapsed Member")),
            Err(MigsStatusError::Retired(RetiredMigsStatus::LapsedMember))
        );
        assert_eq!(
            MigsStatus::decode(Some("Constitutional Member")),
            Err(MigsStatusError::Retired(
                RetiredMigsStatus::ConstitutionalMember
            ))
        );
    }

    #[test]
    fn missing_and_unknown_are_distinguished() {
        assert_eq!(MigsStatus::decode(None), Err(MigsStatusError::Missing));
        assert_eq!(MigsStatus::decode(Some("")), Err(MigsStatusError::Missing));
        assert_eq!(
            MigsStatus::decode(Some("   ")),
            Err(MigsStatusError::Missing)
        );
        assert_eq!(
            MigsStatus::decode(Some("wat")),
            Err(MigsStatusError::Unknown("wat".to_string()))
        );
    }

    #[test]
    fn as_str_round_trips_live_values() {
        for s in [MigsStatus::MemberInGoodStanding, MigsStatus::Lapsed] {
            assert_eq!(MigsStatus::decode(Some(s.as_str())), Ok(s));
        }
    }

    #[test]
    fn only_good_standing_is_good() {
        assert!(MigsStatus::MemberInGoodStanding.is_good());
        assert!(!MigsStatus::Lapsed.is_good());
    }

    #[test]
    fn maps_to_computed_status() {
        assert_eq!(
            MembershipStatus::from(MigsStatus::MemberInGoodStanding),
            MembershipStatus::Member
        );
        assert_eq!(
            MembershipStatus::from(MigsStatus::Lapsed),
            MembershipStatus::DuesExpired
        );
    }
}
