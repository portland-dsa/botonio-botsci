//! The channel-permission vocabulary the engine reasons over: a structural view
//! of a guild's channels and their permission overwrites, mapped from `serenity`
//! at the edge so the engine never depends on the gateway types.
//!
//! Visibility math elsewhere keys on a single bit, [`Permissions::VIEW_CHANNEL`]
//! (and, for the two restricted channels, [`Permissions::SEND_MESSAGES`]); these
//! types carry the whole overwrite array so a write can preserve every other bit.

use serde::{Deserialize, Serialize};

use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, DiscordUserId};

// Re-export so callers say `backends::discord::Permissions` and never reach into
// serenity for the bitflags directly.
pub use serenity::all::Permissions;

/// Whom a single permission overwrite targets: a role or an individual member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OverwriteTarget {
    Role(DiscordRoleId),
    Member(DiscordUserId),
}

/// One permission overwrite: the allow and deny bit-sets for an [`OverwriteTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermOverwrite {
    pub target: OverwriteTarget,
    pub allow: Permissions,
    pub deny: Permissions,
}

/// The structural kind of a channel. Only [`is_category`](ChannelKind::is_category)
/// drives any decision; the rest is carried for the report. Threads are excluded
/// upstream (their permissions derive from the parent), so there is no thread variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    Category,
    Text,
    Voice,
    Announcement,
    Forum,
    Stage,
    Media,
}

impl ChannelKind {
    /// Whether this is a category - the only distinction the classifier needs.
    pub fn is_category(self) -> bool {
        matches!(self, ChannelKind::Category)
    }
}

/// A guild channel projected to the fields the terraform reasons over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannel {
    pub id: DiscordChannelId,
    pub name: String,
    pub kind: ChannelKind,
    pub parent_id: Option<DiscordChannelId>,
    pub position: u16,
    pub overwrites: Vec<PermOverwrite>,
}

/// One whole-guild channel read: the channel list plus whether the `@everyone`
/// guild role grants `VIEW_CHANNEL` at the base level - needed to classify a
/// channel that carries no `@everyone` overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildChannels {
    pub guild_id: DiscordGuildId,
    pub everyone_base_view: bool,
    pub channels: Vec<DiscordChannel>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_category_is_a_category() {
        assert!(ChannelKind::Category.is_category());
        for k in [
            ChannelKind::Text,
            ChannelKind::Voice,
            ChannelKind::Announcement,
            ChannelKind::Forum,
            ChannelKind::Stage,
            ChannelKind::Media,
        ] {
            assert!(!k.is_category(), "{k:?} must not be a category");
        }
    }

    #[test]
    fn overwrite_round_trips_through_json() {
        let o = PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(7)),
            allow: Permissions::VIEW_CHANNEL,
            deny: Permissions::SEND_MESSAGES,
        };
        let back: PermOverwrite =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(back, o);
    }
}
