//! The chapter's raw "Membership Status" field - whether a member is in good
//! standing - as reported by Solidarity Tech.
//!
//! [`MigsStatus`] ("MIGS" = Member In Good Standing) is the *raw* field value,
//! distinct from the computed [`MembershipStatus`] it folds into. It is a
//! **closed** set, grouped by the standing each value implies: every live value
//! sits inside [`GoodStanding`], [`Lapsed`], or [`NotAMember`], so the *group*
//! decides the role while the inner value keeps the exact word the source
//! published - which is what a log line, a cached row, or a moderator-facing post
//! reports. The two retired values (`Lapsed Member`, `Constitutional Member`)
//! decode to a typed [`MigsStatusError::Retired`], never a variant, so a record
//! still carrying one fails loudly instead of being silently mapped. The
//! source->computed adapter ([`From<MigsStatus>`](MigsStatus) for
//! [`MembershipStatus`]) lives here because both types are local to this crate.

use std::fmt;

use crate::membership::MembershipStatus;

/// Define one membership-status group: an enum of the exact labels the source
/// publishes, plus the label table ([`as_str`]) and the enumeration (`ALL`) that
/// [`MigsStatus::decode`] scans. Both are generated from the single variant list,
/// so a value can never be spelled one way going out and another coming back, and
/// a variant added here is decodable the moment it is written.
macro_rules! status_group {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            /// Every value in this group, in the order the source lists them.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact string the source stores for this value.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

status_group! {
    /// The values that earn the `Member` role.
    ///
    /// Several are provisional readings pending a definition from the national
    /// organization: the chapter errs toward granting access rather than demoting a
    /// member on a status nobody has explained yet.
    GoodStanding {
        /// The ordinary dues-current member.
        InGoodStanding => "Member in Good Standing",
        /// Two national records for one person were combined. Read as good standing
        /// because the chapter has no handling for a merged record yet; the surviving
        /// record's own standing is the thing to consult once we do.
        Merged => "Merged",
        /// Believed to be someone who gives without holding membership. Read as good
        /// standing pending a definition.
        Subscriber => "Subscriber",
        /// Believed to be someone who gives without holding membership. Read as good
        /// standing pending a definition.
        Contributor => "Contributor",
        /// Believed to be a membership whose first payment has not cleared.
        Pending => "Pending",
        /// Believed to be national staff, on a record keyed to a national domain
        /// address; their own membership is held separately under a personal one.
        Organization => "Organization",
    }
}

status_group! {
    /// The values that earn the `Dues Expired` role: a membership that has run out
    /// and that the person can restore by paying or signing a waiver.
    Lapsed {
        /// Dues lapsed - the ordinary expiry.
        Lapsed => "Lapsed",
        /// The member ended their own membership, and can rejoin by paying.
        Quit => "Quit",
    }
}

status_group! {
    /// The values that earn the `Unverified` role: the record says this person holds
    /// no membership, and no step is available to them that would restore one. That
    /// missing remedy - not the wording - is what separates this group from
    /// [`Lapsed`], where `Dues Expired` promises a cure that actually exists.
    NotAMember {
        /// On file, but never a member.
        NotADsaMember => "Not a DSA Member",
        /// The member has died. Here rather than with the lapsed values because
        /// nothing about the record is recoverable; a dedicated memorial role is a
        /// possible future kindness.
        Deceased => "Deceased",
        /// Membership was revoked by the organization.
        Expelled => "Expelled",
        /// Believed to be a national staff post given up - the counterpart to
        /// `Organization`, not to `Quit`. Because the field holds a single value, a
        /// resigned staffer still paying dues would read as good standing instead.
        Resigned => "Resigned",
    }
}

status_group! {
    /// One of the two retired "Membership Status" values, kept typed so a decode
    /// failure can say *which* retired value was seen without a bare string.
    RetiredMigsStatus {
        /// `Lapsed Member` - believed unused.
        LapsedMember => "Lapsed Member",
        /// `Constitutional Member` - policy was never defined.
        ConstitutionalMember => "Constitutional Member",
    }
}

/// The live values of the "Membership Status" field, grouped by the standing they imply.
///
/// The outer variant decides the role - [`MemberInGoodStanding`](Self::MemberInGoodStanding)
/// earns `Member`, [`Lapsed`](Self::Lapsed) earns `Dues Expired`,
/// [`NotAMember`](Self::NotAMember) earns `Unverified` - and the inner value carries the
/// exact word the source used, so a decision can be reported with the reason behind it. The
/// two retired values are deliberately absent - see [`decode`](MigsStatus::decode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigsStatus {
    /// In good standing: dues are current, or the value is one we read permissively.
    MemberInGoodStanding(GoodStanding),
    /// The membership has run out, and the person can restore it.
    Lapsed(Lapsed),
    /// The record says this person holds no membership, with no cure available.
    NotAMember(NotAMember),
}

impl MigsStatus {
    /// Whether this status counts as good standing - `true` for every value in
    /// [`GoodStanding`]. A convenience predicate over the group, not the role
    /// decision itself: that runs through `From<MigsStatus> for MembershipStatus`
    /// and `TryFrom<MembershipStatus> for Role`.
    pub fn is_good(self) -> bool {
        matches!(self, Self::MemberInGoodStanding(_))
    }

    /// The exact string a source stores for this value - the inverse of
    /// [`decode`](Self::decode) for every live value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemberInGoodStanding(v) => v.as_str(),
            Self::Lapsed(v) => v.as_str(),
            Self::NotAMember(v) => v.as_str(),
        }
    }

    /// Every live value, group by group. The enumeration [`decode`](Self::decode)
    /// searches, and the list a round-trip test walks.
    pub fn all() -> impl Iterator<Item = Self> {
        let good = GoodStanding::ALL
            .iter()
            .copied()
            .map(Self::MemberInGoodStanding);
        let lapsed = Lapsed::ALL.iter().copied().map(Self::Lapsed);
        let not_a_member = NotAMember::ALL.iter().copied().map(Self::NotAMember);
        good.chain(lapsed).chain(not_a_member)
    }

    /// Decode a raw membership-status field value.
    ///
    /// `None` or an empty string is [`Missing`](MigsStatusError::Missing); the two
    /// retired values are [`Retired`](MigsStatusError::Retired); anything else is
    /// [`Unknown`](MigsStatusError::Unknown). Only the live values succeed - every
    /// caller decides for itself whether a given error is fatal.
    pub fn decode(raw: Option<&str>) -> Result<Self, MigsStatusError> {
        let Some(label) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(MigsStatusError::Missing);
        };
        if let Some(status) = Self::all().find(|s| s.as_str() == label) {
            return Ok(status);
        }
        if let Some(retired) = RetiredMigsStatus::ALL
            .iter()
            .copied()
            .find(|r| r.as_str() == label)
        {
            return Err(MigsStatusError::Retired(retired));
        }
        Err(MigsStatusError::Unknown(label.to_string()))
    }
}

impl fmt::Display for MigsStatus {
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

/// The good-standing half of the `source status -> MembershipStatus -> Role` chain.
/// The *group* decides: any [`GoodStanding`] value is `Member`, any [`Lapsed`] value is
/// `DuesExpired`, any [`NotAMember`] value is `NotAMember`. Lives here, with the shared
/// vocabulary, so the role decision never has to be rewritten per source.
impl From<MigsStatus> for MembershipStatus {
    fn from(status: MigsStatus) -> Self {
        match status {
            MigsStatus::MemberInGoodStanding(_) => MembershipStatus::Member,
            MigsStatus::Lapsed(_) => MembershipStatus::DuesExpired,
            MigsStatus::NotAMember(_) => MembershipStatus::NotAMember,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_live_value_decodes_to_its_group() {
        // The table that matters most: each raw value the source publishes, and the
        // standing group it must land in. A regrouping here is a policy change, so it
        // has to be a deliberate edit to this list.
        let cases = [
            (
                "Member in Good Standing",
                MigsStatus::MemberInGoodStanding(GoodStanding::InGoodStanding),
            ),
            (
                "Merged",
                MigsStatus::MemberInGoodStanding(GoodStanding::Merged),
            ),
            (
                "Subscriber",
                MigsStatus::MemberInGoodStanding(GoodStanding::Subscriber),
            ),
            (
                "Contributor",
                MigsStatus::MemberInGoodStanding(GoodStanding::Contributor),
            ),
            (
                "Pending",
                MigsStatus::MemberInGoodStanding(GoodStanding::Pending),
            ),
            (
                "Organization",
                MigsStatus::MemberInGoodStanding(GoodStanding::Organization),
            ),
            ("Lapsed", MigsStatus::Lapsed(Lapsed::Lapsed)),
            ("Quit", MigsStatus::Lapsed(Lapsed::Quit)),
            (
                "Not a DSA Member",
                MigsStatus::NotAMember(NotAMember::NotADsaMember),
            ),
            ("Deceased", MigsStatus::NotAMember(NotAMember::Deceased)),
            ("Expelled", MigsStatus::NotAMember(NotAMember::Expelled)),
            ("Resigned", MigsStatus::NotAMember(NotAMember::Resigned)),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                MigsStatus::decode(Some(raw)),
                Ok(expected),
                "decoding {raw:?}"
            );
        }
        // Nothing in the enum is missing from the table above.
        assert_eq!(MigsStatus::all().count(), cases.len());
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
    fn as_str_round_trips_every_live_value() {
        // The cache stores `as_str` and reads it back with `decode`, so a value that
        // does not round-trip would silently blank a stored standing.
        for s in MigsStatus::all() {
            assert_eq!(MigsStatus::decode(Some(s.as_str())), Ok(s));
        }
    }

    #[test]
    fn only_the_good_standing_group_is_good() {
        for s in MigsStatus::all() {
            assert_eq!(
                s.is_good(),
                matches!(s, MigsStatus::MemberInGoodStanding(_)),
                "{s} classified wrong"
            );
        }
    }

    #[test]
    fn each_group_maps_to_one_computed_status() {
        for s in MigsStatus::all() {
            let expected = match s {
                MigsStatus::MemberInGoodStanding(_) => MembershipStatus::Member,
                MigsStatus::Lapsed(_) => MembershipStatus::DuesExpired,
                MigsStatus::NotAMember(_) => MembershipStatus::NotAMember,
            };
            assert_eq!(MembershipStatus::from(s), expected, "{s} mapped wrong");
        }
    }
}
