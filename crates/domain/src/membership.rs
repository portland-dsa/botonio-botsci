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
///
/// Distinct from any backend's raw status enum; this is the *computed* result
/// that maps onto a [`Role`]. Each good-standing source converts into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MembershipStatus {
    /// In good standing - qualifies for the `Member` role.
    Member,
    /// A known not-in-good-standing status (e.g. lapsed) - gets `Dues Expired`.
    DuesExpired,
    /// No usable status to decide from - left for manual verification.
    #[default]
    Unverified,
}

/// The [`Role`] each verification outcome grants - the durable half of the
/// `source status -> MembershipStatus -> Role` chain, kept with the vocabulary so
/// the role decision never lives in a front-end.
impl From<MembershipStatus> for Role {
    fn from(status: MembershipStatus) -> Self {
        match status {
            MembershipStatus::Member => Role::Member,
            MembershipStatus::DuesExpired => Role::DuesExpired,
            MembershipStatus::Unverified => Role::Unverified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_status_maps_to_matching_role() {
        assert_eq!(Role::from(MembershipStatus::Member), Role::Member);
        assert_eq!(Role::from(MembershipStatus::DuesExpired), Role::DuesExpired);
        assert_eq!(Role::from(MembershipStatus::Unverified), Role::Unverified);
    }

    #[test]
    fn default_is_unverified() {
        assert_eq!(MembershipStatus::default(), MembershipStatus::Unverified);
    }
}
