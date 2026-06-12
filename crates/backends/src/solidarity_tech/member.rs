//! The `SolidarityTechMember` record and its custom-property value type.

use chrono::NaiveDate;
use domain::MigsStatus;
use serde::Deserialize;

use crate::util::{DiscordHandle, DiscordUserId, Email, Phone, StUserId};

use super::status::{DuesStatus, MembershipType};

/// A Solidarity Tech member, projected to the fields this backend keeps.
///
/// Returned by [`SolidarityTechClient::find_members`](super::SolidarityTechClient::find_members)
/// and its wrappers. The top-level fields come straight from the user record; the
/// two Discord fields are read out of the member's `custom_user_properties` and
/// are `None` whenever the property is unset or stored as an empty string.
#[derive(Debug, Clone)]
pub struct SolidarityTechMember {
    /// Solidarity Tech user id, used verbatim as `{id}` in `PUT /users/{id}`.
    pub id: StUserId,
    /// Primary email, parsed from the user's `email` field. Required; records
    /// with an absent or unparseable email are skipped during list operations.
    pub email: Email,
    /// First name from the user's `first_name` field; `None` when absent or
    /// blank. Used to corroborate a phone-only match (a phone hit with a
    /// mismatched first name is treated as no match).
    pub first_name: Option<String>,
    /// The member's family name, from the ST `last_name` field; `None` if unset.
    pub last_name: Option<String>,
    /// Phone number, parsed from `phone_number`; `None` if absent or unparseable
    /// as a [`Phone`].
    pub phone: Option<Phone>,
    /// Discord handle read from the `discord-handle` custom property; `None`
    /// when unset or empty.
    pub discord_handle: Option<DiscordHandle>,
    /// Discord user id read from the `discord-user-id` custom property; `None`
    /// when unset, empty, or non-numeric.
    pub discord_user_id: Option<DiscordUserId>,
    /// Secondary email from the `alternate-email` custom property; `None` when
    /// unset, empty, or unparseable as an [`Email`].
    pub alternate_email: Option<Email>,
    /// "Monthly Dues Status" custom property, decoded. `None` when the property
    /// is unset or empty; an *unrecognized* value is **not** `None` - it fails
    /// the read with [`SolidarityTechError::UnknownDuesStatus`](super::SolidarityTechError::UnknownDuesStatus).
    pub monthly_dues: Option<DuesStatus>,
    /// "Yearly Dues Status" custom property, decoded. Same `None`/error rules as
    /// [`monthly_dues`](Self::monthly_dues).
    pub yearly_dues: Option<DuesStatus>,
    /// Dues-expiry date from the `x-date` custom property; `None` if the property
    /// is unset or not a `YYYY-MM-DD` date.
    pub xdate: Option<NaiveDate>,
    /// Membership join/start date from the `join-date` custom property; `None` if
    /// the property is unset or not a `YYYY-MM-DD` date. The card's "Join Date".
    pub join_date: Option<NaiveDate>,
    /// Dues cadence from the `membership-type` custom property; `None` when
    /// unset. An *unrecognized* value is **not** `None` - it fails the read with
    /// [`SolidarityTechError::UnknownMembershipType`](super::SolidarityTechError::UnknownMembershipType).
    pub membership_type: Option<MembershipType>,
    /// "Membership Status" from the `membership-status` custom property, decoded
    /// to the shared [`MigsStatus`]. `None` when the property is unset; a retired
    /// or otherwise unrecognized value fails the read with
    /// [`SolidarityTechError::BadMembershipStanding`](super::SolidarityTechError::BadMembershipStanding).
    pub membership_standing: Option<MigsStatus>,
}

/// A baseline member with an empty id/email and every optional field `None`, so
/// a fixture can set only the fields it cares about and spread the rest with
/// `..Default::default()`.
///
/// Gated to test and `mock` builds on purpose. A `Default` is normally an
/// unconditional impl, but this one is a foot-gun in production: the empty
/// `StUserId`/`Email` it builds bypass the non-empty invariant those newtypes
/// enforce on [`FromStr`](std::str::FromStr), and a member with an empty `id`
/// would `PUT /users/` (trailing slash). It exists solely for fixture ergonomics
/// (the test and `mock` builds where every fixture sets `id`/`email` anyway), so
/// keeping it out of production code is correct, not arbitrary.
#[cfg(any(test, feature = "mock"))]
impl Default for SolidarityTechMember {
    fn default() -> Self {
        Self {
            id: StUserId(String::new()),
            email: Email(String::new()),
            first_name: None,
            last_name: None,
            phone: None,
            discord_handle: None,
            discord_user_id: None,
            alternate_email: None,
            monthly_dues: None,
            yearly_dues: None,
            xdate: None,
            join_date: None,
            membership_type: None,
            membership_standing: None,
        }
    }
}

/// An org-level custom user property definition from `GET /custom_user_properties`.
/// `key` is the internal name that appears in a user's `custom_user_properties`
/// map; `name` is the human-facing label. Used to discover the real property keys.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomUserProperty {
    pub id: u64,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub field_type: Option<String>,
}
