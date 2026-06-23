//! The computed membership status and its mapping to a [`Role`].
//!
//! [`MembershipStatus`] is the *computed* verification outcome - distinct from
//! any backend's raw status enum. The authoritative source (Solidarity Tech)
//! converts its own status into this with a `From` impl that lives in that
//! backend; the mapping from here to a [`Role`] is the durable half and lives
//! with the rest of the vocabulary, so it never has to be rewritten when the
//! source changes.

use crate::role::Role;

/// The verification outcome for a member - the role decision the bot acts on.
/// A closed set of exactly three computed results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipStatus {
    /// In good standing - qualifies for the `Member` role.
    Member,
    /// A known not-in-good-standing status (e.g. lapsed) - gets `Dues Expired`.
    DuesExpired,
    /// A matched record with no usable standing - no role can be decided from it,
    /// so it is resolved by hand rather than auto-assigned.
    Malformed,
}

/// Why a [`MembershipStatus`] could not be converted to a [`Role`]: it is
/// [`Malformed`](MembershipStatus::Malformed) and has no role to grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("membership status is malformed: no usable standing to assign a role")]
pub struct MalformedMembership;

/// The durable half of the `source status -> MembershipStatus -> Role` chain, kept
/// with the vocabulary so the role decision never lives in a front-end. Fallible:
/// a `Malformed` status has no role, and the type system now forbids inventing one.
impl TryFrom<MembershipStatus> for Role {
    type Error = MalformedMembership;
    fn try_from(status: MembershipStatus) -> Result<Self, Self::Error> {
        match status {
            MembershipStatus::Member => Ok(Role::Member),
            MembershipStatus::DuesExpired => Ok(Role::DuesExpired),
            MembershipStatus::Malformed => Err(MalformedMembership),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_status_converts_to_matching_role() {
        assert_eq!(Role::try_from(MembershipStatus::Member), Ok(Role::Member));
        assert_eq!(
            Role::try_from(MembershipStatus::DuesExpired),
            Ok(Role::DuesExpired)
        );
        assert_eq!(
            Role::try_from(MembershipStatus::Malformed),
            Err(MalformedMembership)
        );
    }
}
