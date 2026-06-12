//! Private Solidarity Tech request/response wire types and their decode into
//! [`SolidarityTechMember`].

use chrono::NaiveDate;
use domain::{MigsStatus, MigsStatusError};
use serde::{Deserialize, Serialize};

use crate::util::{DiscordUserId, StUserId};

use super::error::SolidarityTechError;
use super::member::{CustomUserProperty, SolidarityTechMember};
use super::status::{DuesStatus, MembershipType};

/// Deserialize-only custom-property bag for reads. Carries every column the read
/// path projects: the three writable identity fields plus the read-only
/// verification columns. Serde ignores keys not listed here.
#[derive(Deserialize, Default)]
struct StReadProps {
    #[serde(
        rename = "discord-handle",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    discord_handle: Option<String>,

    #[serde(
        rename = "discord-user-id",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    discord_user_id: Option<String>,

    #[serde(
        rename = "alternate-email",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    alternate_email: Option<String>,

    #[serde(
        rename = "monthly-dues-status",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    monthly_dues_status: Option<String>,

    #[serde(
        rename = "yearly-dues-status",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    yearly_dues_status: Option<String>,

    #[serde(
        rename = "x-date",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    x_date: Option<String>,

    #[serde(
        rename = "join-date",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    join_date: Option<String>,

    #[serde(
        rename = "membership-type",
        default,
        deserialize_with = "crate::util::nonempty_string"
    )]
    membership_type: Option<String>,

    #[serde(
        rename = "membership-status",
        default,
        deserialize_with = "crate::util::select_label"
    )]
    membership_status: Option<String>,
}

/// Serialize-only custom-property bag for `PUT /users/{id}` write bodies.
/// Carries only the three writable identity fields; the read-only verification
/// columns are physically absent, so accidentally including them is a compile error.
#[derive(Serialize, Default)]
pub(crate) struct StWriteProps {
    #[serde(rename = "discord-handle", skip_serializing_if = "Option::is_none")]
    pub(crate) discord_handle: Option<String>,

    #[serde(rename = "discord-user-id", skip_serializing_if = "Option::is_none")]
    pub(crate) discord_user_id: Option<String>,

    #[serde(rename = "alternate-email", skip_serializing_if = "Option::is_none")]
    pub(crate) alternate_email: Option<String>,
}

/// Serialize-only write body for `PUT /users/{id}`.
#[derive(Serialize)]
pub(crate) struct UserUpdate {
    pub(crate) custom_user_properties: StWriteProps,
}

#[derive(Deserialize)]
pub(crate) struct UsersListResponse {
    #[serde(default)]
    pub(crate) data: Vec<UserResponse>,
    #[serde(default)]
    pub(crate) meta: Option<Meta>,
}

#[derive(Deserialize)]
pub(crate) struct Meta {
    #[serde(default)]
    pub(crate) total_count: u32,
}

#[derive(Deserialize)]
pub(crate) struct UserResponse {
    id: u64,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    phone_number: Option<String>,
    #[serde(default)]
    custom_user_properties: StReadProps,
}

#[derive(Clone, Deserialize)]
pub(crate) struct CustomPropsListResponse {
    #[serde(default)]
    pub(crate) data: Vec<CustomUserProperty>,
}

impl TryFrom<UserResponse> for SolidarityTechMember {
    type Error = SolidarityTechError;

    fn try_from(resp: UserResponse) -> Result<Self, Self::Error> {
        let email = resp
            .email
            .as_deref()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                SolidarityTechError::MalformedMember(format!(
                    "user {} missing required field email",
                    resp.id
                ))
            })?;

        let phone = resp.phone_number.as_deref().and_then(|s| s.parse().ok());

        let cp = resp.custom_user_properties;
        let discord_handle = cp.discord_handle.as_deref().and_then(|s| s.parse().ok());
        let discord_user_id = cp
            .discord_user_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .map(DiscordUserId);
        // A malformed alternate email is tolerated as `None` (like a bad phone),
        // not a hard error.
        let alternate_email = cp.alternate_email.as_deref().and_then(|s| s.parse().ok());

        // Dues decode is failable and *propagates*: an unrecognized value is a
        // hard `UnknownDuesStatus`, not a silently-dropped member.
        let monthly_dues = cp
            .monthly_dues_status
            .as_deref()
            .map(DuesStatus::try_from)
            .transpose()?;
        let yearly_dues = cp
            .yearly_dues_status
            .as_deref()
            .map(DuesStatus::try_from)
            .transpose()?;

        // A non-date `x-date` is tolerated as `None` (like a malformed phone);
        // it isn't a hard error.
        let xdate = cp
            .x_date
            .as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

        let join_date = cp
            .join_date
            .as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

        // Like dues, an unrecognized membership type *propagates* rather than
        // dropping the member.
        let membership_type = cp
            .membership_type
            .as_deref()
            .map(MembershipType::try_from)
            .transpose()?;

        // A missing membership-status is fine (`None`); a present-but-undecodable
        // value (unrecognized or retired) is a hard error, surfaced like the
        // other custom-property decode failures rather than silently dropped.
        let membership_standing = match MigsStatus::decode(cp.membership_status.as_deref()) {
            Ok(s) => Some(s),
            Err(MigsStatusError::Missing) => None,
            Err(e) => return Err(SolidarityTechError::BadMembershipStanding(e)),
        };

        let first_name = resp.first_name.filter(|s| !s.trim().is_empty());
        let last_name = resp.last_name.filter(|s| !s.trim().is_empty());

        Ok(SolidarityTechMember {
            id: StUserId(resp.id.to_string()),
            email,
            first_name,
            last_name,
            phone,
            discord_handle,
            discord_user_id,
            alternate_email,
            monthly_dues,
            yearly_dues,
            xdate,
            join_date,
            membership_type,
            membership_standing,
        })
    }
}

/// Decode one page of users into members. With `lenient`, a member whose custom
/// fields don't decode is skipped with a warning rather than failing the page (a
/// `MalformedMember` from an unparseable email is always skipped). Both whole-roster
/// sweeps - the bot index build and the full-collection `members_page` sweep - pass
/// `lenient` so a single bad record (e.g. a retired membership-status tier) never
/// aborts the run;
/// targeted single-member lookups ([`find_members`](super::SolidarityTechClient::find_members))
/// stay strict and surface the decode error instead.
pub(crate) fn decode_members(
    data: Vec<UserResponse>,
    lenient: bool,
) -> Result<Vec<SolidarityTechMember>, SolidarityTechError> {
    let mut members = Vec::with_capacity(data.len());
    for r in data {
        match SolidarityTechMember::try_from(r) {
            Ok(m) => members.push(m),
            Err(e @ SolidarityTechError::MalformedMember(_)) => {
                tracing::warn!(error = %e, "skipping malformed solidarity tech member");
            }
            Err(e) if lenient => {
                tracing::warn!(error = %e, "skipping undecodable solidarity tech member (lenient index build)");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    // watchlist: drop once a decode scenario asserts a member with no custom
    // properties reads every Discord/dues field as None (covers the none case too).
    #[test]
    fn user_to_member_missing_custom_props_are_none() {
        let u = UserResponse {
            id: 1,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps::default(),
        };
        let m = SolidarityTechMember::try_from(u).unwrap();
        assert!(m.discord_handle.is_none());
        assert!(m.discord_user_id.is_none());
        assert!(m.phone.is_none());
    }

    // watchlist: drop once a strict-lookup scenario reads a null-email (malformed) member.
    #[test]
    fn user_to_member_null_email_returns_err() {
        let u = UserResponse {
            id: 7,
            email: None,
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps::default(),
        };
        assert!(SolidarityTechMember::try_from(u).is_err());
    }

    // watchlist: drop once a decode scenario reads a non-numeric discord-user-id property.
    #[test]
    fn user_to_member_non_numeric_discord_id_is_none() {
        let u = UserResponse {
            id: 9,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps {
                discord_handle: None,
                discord_user_id: Some("not-a-number".to_string()),
                ..StReadProps::default()
            },
        };
        let m = SolidarityTechMember::try_from(u).unwrap();
        assert!(m.discord_user_id.is_none());
    }

    // watchlist: near-duplicate of `user_to_member_missing_custom_props_are_none`;
    // one "member with no custom properties" decode scenario absorbs both.
    #[test]
    fn user_to_member_none_custom_props_are_none() {
        let u = UserResponse {
            id: 9,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps::default(),
        };
        let m = SolidarityTechMember::try_from(u).unwrap();
        assert!(m.discord_handle.is_none());
        assert!(m.discord_user_id.is_none());
    }

    #[test]
    fn user_update_serializes_with_hyphen_keys() {
        let body = UserUpdate {
            custom_user_properties: StWriteProps {
                discord_handle: Some("zoop".to_string()),
                discord_user_id: Some("987654321".to_string()),
                ..StWriteProps::default()
            },
        };
        // The read-only dues fields are `None`, so they stay out of the write
        // body entirely - the merge only ever touches the Discord keys.
        let expected = serde_json::json!({
            "custom_user_properties": {
                "discord-handle": "zoop",
                "discord-user-id": "987654321",
            }
        });
        assert_eq!(serde_json::to_value(&body).unwrap(), expected);
    }

    // watchlist: the alternate-email property is in use today and may need a
    // migration later, so this stays until a read scenario covers it - not a
    // deletion candidate.
    #[test]
    fn user_to_member_reads_alternate_email() {
        let u = UserResponse {
            id: 7,
            email: Some("primary@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps {
                alternate_email: Some("alt@b.com".to_string()),
                ..StReadProps::default()
            },
        };
        let m = SolidarityTechMember::try_from(u).unwrap();
        assert_eq!(
            m.alternate_email.as_ref().map(|e| e.as_str()),
            Some("alt@b.com")
        );
    }

    // watchlist: drop once a decode scenario uses a malformed x-date and asserts
    // the field is empty.
    #[test]
    fn unparseable_xdate_is_none() {
        let u = UserResponse {
            id: 2,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps {
                x_date: Some("not-a-date".to_string()),
                ..StReadProps::default()
            },
        };
        assert!(SolidarityTechMember::try_from(u).unwrap().xdate.is_none());
    }

    // watchlist: drop once a decode scenario reads a member with the
    // membership-status property absent and asserts the standing is empty.
    #[test]
    fn missing_membership_status_is_none() {
        // A member may simply not have the property set - that's `None`, not an
        // error (unlike an unrecognized value).
        let u = UserResponse {
            id: 3,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps::default(),
        };
        assert_eq!(
            SolidarityTechMember::try_from(u)
                .unwrap()
                .membership_standing,
            None
        );
    }

    // watchlist: extend the expiry/type/status decode scenario to also assert the
    // join date, then drop this.
    #[test]
    fn user_to_member_reads_join_date() {
        let u = UserResponse {
            id: 5,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps {
                join_date: Some("2021-03-15".to_string()),
                ..StReadProps::default()
            },
        };
        let m = SolidarityTechMember::try_from(u).unwrap();
        assert_eq!(m.join_date, NaiveDate::from_ymd_opt(2021, 3, 15));
    }

    // watchlist: drop once a decode scenario uses a malformed join date and
    // asserts the field is empty.
    #[test]
    fn unparseable_join_date_is_none() {
        let u = UserResponse {
            id: 6,
            email: Some("a@b.com".to_string()),
            first_name: None,
            last_name: None,
            phone_number: None,
            custom_user_properties: StReadProps {
                join_date: Some("nope".to_string()),
                ..StReadProps::default()
            },
        };
        assert!(
            SolidarityTechMember::try_from(u)
                .unwrap()
                .join_date
                .is_none()
        );
    }

    #[test]
    fn decode_members_lenient_skips_bad_member_strict_errors() {
        fn user(id: u64, status: &str) -> UserResponse {
            UserResponse {
                id,
                email: Some(format!("u{id}@b.com")),
                first_name: None,
                last_name: None,
                phone_number: None,
                custom_user_properties: StReadProps {
                    membership_status: Some(status.to_string()),
                    ..StReadProps::default()
                },
            }
        }
        // "Lapsed Member" is a RETIRED value -> BadMembershipStanding; "Member in Good Standing" is valid.
        // lenient: the bad one is skipped, the good one survives
        let ok = decode_members(
            vec![user(1, "Member in Good Standing"), user(2, "Lapsed Member")],
            true,
        );
        assert_eq!(ok.unwrap().len(), 1);
        // strict: the bad one is a hard error
        let err = decode_members(
            vec![user(1, "Member in Good Standing"), user(2, "Lapsed Member")],
            false,
        );
        assert!(err.is_err());
    }

    #[test]
    fn membership_status_decodes_from_label_value_array() {
        // Confirms the real ST wire shape - a select field comes as
        // `[{"label":"...","value":"<opaque-id>"}]` - is decoded to the correct
        // `MigsStatus` rather than falling through to `None`.
        let json = r#"{
            "id": 1,
            "email": "a@b.com",
            "custom_user_properties": {
                "membership-status": [{ "label": "Member in Good Standing", "value": "AfVqfj0n" }],
                "x-date": "2026-09-20",
                "join-date": "2023-10-15"
            }
        }"#;
        let resp: UserResponse = serde_json::from_str(json).unwrap();
        let m = SolidarityTechMember::try_from(resp).unwrap();
        assert_eq!(
            m.membership_standing,
            Some(domain::MigsStatus::MemberInGoodStanding)
        );
        assert_eq!(m.join_date, chrono::NaiveDate::from_ymd_opt(2023, 10, 15));
    }
}
