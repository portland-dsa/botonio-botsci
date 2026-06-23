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
    /// In good standing but with no linked Discord id - found only by email. The
    /// target of the manual verify-by-email path: the auto match misses (nothing on
    /// record by id or handle), the email lookup finds this record, and the verify
    /// backfills the moderator-supplied account's id and assigns `Member`. Its email
    /// is the address from its map key.
    EmailVerify,
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
            "email_verify" => Self::EmailVerify,
            _ => return None,
        })
    }

    /// This persona's served `/users` user object, dated against `today`.
    ///
    /// `discord_user_id`, when `Some`, is stamped into the `discord-user-id` custom
    /// property the index keys on; an email-keyed persona passes `None` and so is found
    /// only by email. `email_override` replaces the persona's default email (an
    /// email-keyed entry stamps the address from its map key). `st_id` is the record's
    /// synthetic Solidarity Tech user id.
    pub fn user_json(
        self,
        st_id: u64,
        discord_user_id: Option<u64>,
        email_override: Option<&str>,
        today: NaiveDate,
    ) -> Value {
        let ymd = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
        // "Membership Status" is a select field: `[{label, value}]`; the decode
        // reads the label, so the value is an opaque placeholder.
        let standing = |label: &str| json!([{ "label": label, "value": "mock" }]);

        // The Malformed persona has no email at all, so it takes no email override and
        // is built directly; its `discord-user-id` is still stamped when present.
        if let Self::Malformed = self {
            let mut props = json!({});
            if let Some(did) = discord_user_id {
                props["discord-user-id"] = json!(did.to_string());
            }
            return user_json(st_id, None, props);
        }

        // Each remaining persona yields its default email and its custom properties
        // *without* the Discord id, which is stamped (or not) below.
        let (default_email, mut props): (&str, Value) = match self {
            Self::GoodStanding | Self::EmailVerify => (
                match self {
                    Self::EmailVerify => "email-verify@persona.test",
                    _ => "good-standing@persona.test",
                },
                json!({
                    "membership-status": standing("Member in Good Standing"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "active",
                    "x-date": ymd(today + Duration::days(300)),
                    "join-date": "2021-03-15",
                }),
            ),
            Self::Amber => (
                "amber@persona.test",
                json!({
                    "membership-status": standing("Member in Good Standing"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "active",
                    "x-date": ymd(today + Duration::days(12)),
                    "join-date": "2021-03-15",
                }),
            ),
            Self::Lapsed => (
                "lapsed@persona.test",
                json!({
                    "membership-status": standing("Lapsed"),
                    "membership-type": "yearly",
                    "monthly-dues-status": "overdue",
                    "x-date": ymd(today - Duration::days(60)),
                    "join-date": "2020-01-10",
                }),
            ),
            Self::RetiredTier => (
                "retired@persona.test",
                json!({ "membership-status": standing("Lapsed Member") }),
            ),
            Self::Malformed => unreachable!("Malformed is handled above"),
        };
        if let Some(did) = discord_user_id {
            props["discord-user-id"] = json!(did.to_string());
        }
        user_json(st_id, Some(email_override.unwrap_or(default_email)), props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backends::solidarity_tech::fixtures::decode_user;
    use domain::{MembershipStatus, MigsStatus, Role};

    /// Replicate the standing-to-role chain locally so the guard test does not
    /// depend on the engine crate, keeping the mock-st crate independent.
    fn role_of(standing: Option<MigsStatus>) -> Role {
        let status = standing
            .map(MembershipStatus::from)
            .unwrap_or(MembershipStatus::Malformed);
        Role::try_from(status).unwrap_or(Role::Unverified)
    }

    #[test]
    fn parses_known_personas_only() {
        assert_eq!(Persona::parse("amber"), Some(Persona::Amber));
        assert_eq!(Persona::parse("  lapsed "), Some(Persona::Lapsed));
        assert_eq!(Persona::parse("email_verify"), Some(Persona::EmailVerify));
        assert_eq!(Persona::parse("nope"), None);
    }

    #[test]
    fn personas_decode_to_intended_states() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let m = decode_user(&Persona::GoodStanding.user_json(1, Some(111), None, today)).unwrap();
        assert_eq!(
            m.membership_standing,
            Some(MigsStatus::MemberInGoodStanding)
        );
        assert_eq!(m.discord_user_id.map(|d| d.0), Some(111));
        assert_eq!(role_of(m.membership_standing), Role::Member);

        let m = decode_user(&Persona::Amber.user_json(2, Some(222), None, today)).unwrap();
        assert_eq!(
            m.membership_standing,
            Some(MigsStatus::MemberInGoodStanding)
        );
        let x = m.xdate.expect("amber has an x-date");
        assert!(
            x > today && (x - today).num_days() <= 30,
            "amber x-date in reminder window"
        );

        let m = decode_user(&Persona::Lapsed.user_json(3, Some(333), None, today)).unwrap();
        assert_eq!(m.membership_standing, Some(MigsStatus::Lapsed));
        assert_eq!(role_of(m.membership_standing), Role::DuesExpired);

        // Found only by email: good standing, no linked Discord id, email from its key.
        let m = decode_user(&Persona::EmailVerify.user_json(
            6,
            None,
            Some("by-email@persona.test"),
            today,
        ))
        .unwrap();
        assert_eq!(
            m.membership_standing,
            Some(MigsStatus::MemberInGoodStanding)
        );
        assert_eq!(role_of(m.membership_standing), Role::Member);
        assert_eq!(m.discord_user_id, None);
        assert_eq!(m.email.as_str(), "by-email@persona.test");

        // Skipped by the lenient sweep: their decode errors.
        assert!(decode_user(&Persona::RetiredTier.user_json(4, Some(444), None, today)).is_err());
        assert!(decode_user(&Persona::Malformed.user_json(5, Some(555), None, today)).is_err());
    }
}
