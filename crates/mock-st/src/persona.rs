//! The fabricated-member templates and the env value that names one.
//!
//! A [`Persona`] is everything about a served member *except* which real Discord
//! account wears it - the id is stamped in at roster-build time. Each variant is
//! a coherent membership state; the [`Persona::user_json`] output decodes through
//! the real backend exactly as a live `/users` record would (pinned by the guard
//! test below).

use backends::solidarity_tech::fixtures::user_json;
use chrono::{Duration, NaiveDate};
use serde_json::{Value, json};

/// A named fabricated-member template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persona {
    /// In good standing, dues far from expiry - the green card.
    GoodStanding,
    /// In good standing, dues inside the 30-day reminder window - the amber card.
    Amber,
    /// Lapsed standing - the red card / `Dues Expired` role.
    Lapsed,
    /// A retired membership-status tier: a hard decode error, so the lenient list
    /// sweep skips the member (absent from the index -> "not a member").
    RetiredTier,
    /// No email: the strict decode rejects it as malformed, so the sweep skips it.
    Malformed,
}

impl Persona {
    /// The [`Persona`] named by an id-to-persona map entry, or `None` for an
    /// unknown name.
    ///
    /// ```
    /// use mock_st::Persona;
    /// assert_eq!(Persona::parse("amber"), Some(Persona::Amber));
    /// assert!(Persona::parse("nope").is_none());
    /// ```
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim() {
            "good_standing" => Self::GoodStanding,
            "amber" => Self::Amber,
            "lapsed" => Self::Lapsed,
            "retired_tier" => Self::RetiredTier,
            "malformed" => Self::Malformed,
            _ => return None,
        })
    }

    /// This persona's served `/users` user object, dated against `today`.
    ///
    /// `discord_user_id` is stamped into the `discord-user-id` custom property the
    /// index keys on; `st_id` is the record's synthetic Solidarity Tech user id.
    pub fn user_json(self, st_id: u64, discord_user_id: u64, today: NaiveDate) -> Value {
        let ymd = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
        // "Membership Status" is a select field: `[{label, value}]`; the decode
        // reads the label, so the value is an opaque placeholder.
        let standing = |label: &str| json!([{ "label": label, "value": "mock" }]);
        let did = discord_user_id.to_string();
        match self {
            Self::GoodStanding => user_json(
                st_id,
                Some("good-standing@persona.test"),
                json!({
                    "membership-status": standing("Member in Good Standing"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "active",
                    "x-date": ymd(today + Duration::days(300)),
                    "join-date": "2021-03-15",
                    "discord-user-id": did,
                }),
            ),
            Self::Amber => user_json(
                st_id,
                Some("amber@persona.test"),
                json!({
                    "membership-status": standing("Member in Good Standing"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "active",
                    "x-date": ymd(today + Duration::days(12)),
                    "join-date": "2021-03-15",
                    "discord-user-id": did,
                }),
            ),
            Self::Lapsed => user_json(
                st_id,
                Some("lapsed@persona.test"),
                json!({
                    "membership-status": standing("Lapsed"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "overdue",
                    "x-date": ymd(today - Duration::days(60)),
                    "join-date": "2020-01-10",
                    "discord-user-id": did,
                }),
            ),
            Self::RetiredTier => user_json(
                st_id,
                Some("retired@persona.test"),
                json!({
                    "membership-status": standing("Lapsed Member"),
                    "discord-user-id": did,
                }),
            ),
            Self::Malformed => user_json(st_id, None, json!({ "discord-user-id": did })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backends::solidarity_tech::fixtures::decode_user;
    use domain::{MembershipStatus, MigsStatus, Role};

    /// Replicates `MemberRecord::role()`'s `standing -> MembershipStatus -> Role`
    /// chain without depending on the engine.
    fn role_of(standing: Option<MigsStatus>) -> Role {
        Role::from(standing.map(MembershipStatus::from).unwrap_or_default())
    }

    #[test]
    fn parses_known_personas_only() {
        assert_eq!(Persona::parse("amber"), Some(Persona::Amber));
        assert_eq!(Persona::parse("  lapsed "), Some(Persona::Lapsed));
        assert_eq!(Persona::parse("nope"), None);
    }

    #[test]
    fn personas_decode_to_intended_states() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let m = decode_user(&Persona::GoodStanding.user_json(1, 111, today)).unwrap();
        assert_eq!(
            m.membership_standing,
            Some(MigsStatus::MemberInGoodStanding)
        );
        assert_eq!(m.discord_user_id.map(|d| d.0), Some(111));
        assert_eq!(role_of(m.membership_standing), Role::Member);

        let m = decode_user(&Persona::Amber.user_json(2, 222, today)).unwrap();
        assert_eq!(
            m.membership_standing,
            Some(MigsStatus::MemberInGoodStanding)
        );
        let x = m.xdate.expect("amber has an x-date");
        assert!(
            x > today && (x - today).num_days() <= 30,
            "amber x-date in reminder window"
        );

        let m = decode_user(&Persona::Lapsed.user_json(3, 333, today)).unwrap();
        assert_eq!(m.membership_standing, Some(MigsStatus::Lapsed));
        assert_eq!(role_of(m.membership_standing), Role::DuesExpired);

        // Skipped by the lenient sweep: their decode errors.
        assert!(decode_user(&Persona::RetiredTier.user_json(4, 444, today)).is_err());
        assert!(decode_user(&Persona::Malformed.user_json(5, 555, today)).is_err());
    }
}
