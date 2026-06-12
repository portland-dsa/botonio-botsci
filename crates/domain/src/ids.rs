//! Newtype vocabulary for every external identifier the crate handles.
//!
//! Each backend speaks in these types rather than raw strings, which turns a
//! whole class of mix-ups - passing a [`StUserId`] where an [`Email`] is
//! expected, or forwarding a string from one API into another that wants a
//! different shape - into compile errors. Every type round-trips through
//! [`FromStr`] and [`Display`] and serializes transparently, so the wire form is
//! just the inner value with no wrapping object.
//!
//! The three snowflake types ([`DiscordUserId`], [`DiscordGuildId`],
//! [`DiscordChannelId`]) wrap `u64` and reject non-numeric input. The four string
//! types ([`DiscordHandle`], [`StUserId`], [`Email`], [`Phone`]) guarantee only
//! non-emptiness - see each type for the deliberately narrow validation it does,
//! and does not, perform.
//!
//! [`FromStr`]: std::str::FromStr
//! [`Display`]: std::fmt::Display

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Error returned by the [`FromStr`] implementation of every identifier newtype.
///
/// [`FromStr`]: std::str::FromStr
#[derive(Debug, thiserror::Error)]
pub enum IdParseError {
    /// The input was not a valid `u64`, so it cannot be a Discord snowflake.
    #[error("discord snowflake must be a u64: {0}")]
    DiscordSnowflake(#[from] std::num::ParseIntError),
    /// The input was empty or contained only whitespace.
    ///
    /// This is the only validation the string newtypes perform; any non-blank
    /// input is accepted as-is.
    #[error("value cannot be empty")]
    Empty,
}

/// Defines a `u64`-backed snowflake identifier newtype.
///
/// The numeric mirror of `string_newtype!`: every such type wraps a `u64`,
/// rejects non-numeric input on [`FromStr`] (surfacing
/// [`IdParseError::DiscordSnowflake`]), and serializes transparently. Leading
/// attributes are forwarded onto the generated struct - that is how each newtype
/// carries its own doc comment, and how a type that needs more than the common
/// derives (e.g. an `Ord`) adds them with an extra `#[derive(...)]`.
///
/// [`FromStr`]: std::str::FromStr
macro_rules! numeric_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok($name(s.parse()?))
            }
        }
    };
}

numeric_newtype! {
    /// A Discord user snowflake, guaranteed to hold a valid `u64`.
    ///
    /// Wrapping the raw `u64` keeps a user id distinct from a [`DiscordGuildId`] so
    /// the two can never be transposed at a call site. The inner field is public so
    /// a backend that already holds a known-good value decoded from an API response
    /// can construct one directly without routing through [`FromStr`].
    ///
    /// ```
    /// use domain::DiscordUserId;
    ///
    /// let id: DiscordUserId = "123456789012345678".parse().unwrap();
    /// assert_eq!(id.to_string(), "123456789012345678");
    /// assert!("not-a-snowflake".parse::<DiscordUserId>().is_err());
    /// ```
    ///
    /// [`FromStr`]: std::str::FromStr
    DiscordUserId
}

numeric_newtype! {
    /// A Discord guild (server) snowflake, guaranteed to hold a valid `u64`.
    ///
    /// A separate type from [`DiscordUserId`] for the very reason the two exist at
    /// all: a guild id and a user id are both `u64` underneath, and keeping them
    /// distinct stops one being passed where the other is required.
    ///
    /// ```
    /// use domain::DiscordGuildId;
    ///
    /// let g: DiscordGuildId = "987654321098765432".parse().unwrap();
    /// assert_eq!(g.to_string(), "987654321098765432");
    /// assert!("nope".parse::<DiscordGuildId>().is_err());
    /// ```
    DiscordGuildId
}

numeric_newtype! {
    /// A Discord channel snowflake, guaranteed to hold a valid `u64`.
    ///
    /// Distinct from [`DiscordUserId`] and [`DiscordGuildId`] for the same reason
    /// they are distinct from each other: a channel id, a user id, and a guild id
    /// are all `u64` underneath, and separate types stop one being passed where
    /// another is required. The inner field is public so a backend decoding a
    /// channel from an API response can construct one directly.
    ///
    /// ```
    /// use domain::DiscordChannelId;
    ///
    /// let c: DiscordChannelId = "112233445566778899".parse().unwrap();
    /// assert_eq!(c.to_string(), "112233445566778899");
    /// assert!("not-a-channel".parse::<DiscordChannelId>().is_err());
    /// ```
    // `Ord` (and its `PartialOrd` prerequisite) beyond the common derives: channel
    // ids are sorted for stable review output.
    #[derive(PartialOrd, Ord)]
    DiscordChannelId
}

/// Defines a `String`-backed identifier newtype.
///
/// Every such type rejects empty/all-whitespace input on [`FromStr`] but
/// otherwise stores the value verbatim - see the per-type docs for the exact
/// (narrow) guarantees. Leading attributes, including doc comments, are
/// forwarded onto the generated struct, which is how each newtype carries its
/// own documentation.
///
/// [`FromStr`]: std::str::FromStr
macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                if s.trim().is_empty() {
                    Err(IdParseError::Empty)
                } else {
                    Ok($name(s.to_owned()))
                }
            }
        }

        impl $name {
            /// Returns the wrapped string as a slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_newtype! {
    /// A Discord username, guaranteed non-empty.
    ///
    /// Discord usernames no longer carry discriminators, so this simply holds
    /// whatever the guild API returns. Emptiness is the only thing rejected:
    /// there is no format check, and [`FromStr`] keeps the value untrimmed, so a
    /// handle submitted with surrounding whitespace is preserved verbatim.
    ///
    /// ```
    /// use domain::DiscordHandle;
    ///
    /// let h: DiscordHandle = "zoop".parse().unwrap();
    /// assert_eq!(h.as_str(), "zoop");
    /// assert!("".parse::<DiscordHandle>().is_err());
    /// assert!("   ".parse::<DiscordHandle>().is_err());
    /// ```
    ///
    /// [`FromStr`]: std::str::FromStr
    DiscordHandle
}

string_newtype! {
    /// A Solidarity Tech numeric user id, guaranteed non-empty.
    ///
    /// Used verbatim in `PUT /users/{id}` paths. The API returns it as a `u64`
    /// but we store it as a string so it threads through the URL without an extra
    /// conversion at every call site.
    ///
    /// ```
    /// use domain::StUserId;
    ///
    /// let id: StUserId = "4242".parse().unwrap();
    /// assert_eq!(id.as_str(), "4242");
    /// assert!("".parse::<StUserId>().is_err());
    /// ```
    StUserId
}

string_newtype! {
    /// An email address, guaranteed non-empty.
    ///
    /// Despite the name this performs **no** RFC validation - it only rejects
    /// blank input. Callers that need real format checking must do it before
    /// constructing the value; the backends treat it as opaque and hand it to
    /// API filters verbatim.
    ///
    /// ```
    /// use domain::Email;
    ///
    /// let e: Email = "member@example.com".parse().unwrap();
    /// assert_eq!(e.as_str(), "member@example.com");
    /// assert!("".parse::<Email>().is_err());
    /// ```
    Email
}

string_newtype! {
    /// A phone number, guaranteed non-empty.
    ///
    /// Like [`Email`], this is a thin wrapper with no normalization: no E.164
    /// enforcement, no digit stripping. Solidarity Tech stores values such as
    /// `+15035551234` verbatim.
    ///
    /// ```
    /// use domain::Phone;
    ///
    /// let p: Phone = "+15035551234".parse().unwrap();
    /// assert_eq!(p.as_str(), "+15035551234");
    /// assert!("".parse::<Phone>().is_err());
    /// ```
    Phone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_user_id_round_trips() {
        let id: DiscordUserId = "123456789012345678".parse().unwrap();
        assert_eq!(id.to_string(), "123456789012345678");
    }

    #[test]
    fn discord_user_id_rejects_non_numeric() {
        assert!("abc".parse::<DiscordUserId>().is_err());
    }

    #[test]
    fn discord_channel_id_round_trips_and_rejects_non_numeric() {
        let c: DiscordChannelId = "112233445566778899".parse().unwrap();
        assert_eq!(c.to_string(), "112233445566778899");
        assert!("nope".parse::<DiscordChannelId>().is_err());
    }

    #[test]
    fn string_newtypes_reject_empty() {
        assert!("".parse::<Email>().is_err());
        assert!("   ".parse::<DiscordHandle>().is_err());
    }

    #[test]
    fn string_newtypes_accept_non_empty() {
        let h: DiscordHandle = "zoop".parse().unwrap();
        assert_eq!(h.as_str(), "zoop");
    }
}
