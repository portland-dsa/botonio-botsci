//! Guild channel value types and their projection from serenity's channel model.
//!
//! Not yet exercised by the bot; kept for the planned moderator `/setup` command,
//! which will let a mod choose channels from a menu rather than paste raw ids.

use serenity::model::channel::{ChannelType, GuildChannel};

use crate::util::DiscordChannelId;

/// A guild channel's type, projected to the kinds this backend distinguishes.
///
/// Threads carry no permission overwrites of their own, so they are filtered out
/// before projection and never become a [`DiscordChannel`]; they have no variant
/// here. Anything serenity reports that is not one of the named kinds (DMs,
/// directories, unknown future types) collapses to [`ChannelKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChannelKind {
    Text,
    Voice,
    Stage,
    Category,
    Forum,
    Announcement,
    Other,
}

/// A guild channel projected to the fields a channel picker needs.
///
/// Categories are included; threads are excluded at the source.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscordChannel {
    pub id: DiscordChannelId,
    pub name: String,
    pub kind: ChannelKind,
    /// The parent category's id, if this channel sits under one.
    pub parent_id: Option<DiscordChannelId>,
    /// The channel's sort position, kept only so review output is stable.
    pub position: u16,
}

/// Maps serenity's [`ChannelType`] to the [`ChannelKind`] this backend tracks.
///
/// Thread types collapse to [`ChannelKind::Other`] but are filtered out before
/// projection (see [`is_thread`]); anything else unrecognized is also `Other`.
fn channel_kind(kind: ChannelType) -> ChannelKind {
    match kind {
        ChannelType::Text => ChannelKind::Text,
        ChannelType::Voice => ChannelKind::Voice,
        ChannelType::Stage => ChannelKind::Stage,
        ChannelType::Category => ChannelKind::Category,
        ChannelType::Forum => ChannelKind::Forum,
        ChannelType::News => ChannelKind::Announcement,
        _ => ChannelKind::Other,
    }
}

/// Whether a serenity [`ChannelType`] is a thread, which carries no permission
/// overwrites of its own and so is excluded from channel listings.
pub(crate) fn is_thread(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::NewsThread | ChannelType::PublicThread | ChannelType::PrivateThread
    )
}

/// Projects a serenity [`GuildChannel`] to a [`DiscordChannel`].
pub(crate) fn project_channel(c: &GuildChannel) -> DiscordChannel {
    DiscordChannel {
        id: DiscordChannelId(c.id.get()),
        name: c.name.clone(),
        kind: channel_kind(c.kind),
        parent_id: c.parent_id.map(|p| DiscordChannelId(p.get())),
        position: c.position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_maps_news_to_announcement_and_unknowns_to_other() {
        assert_eq!(channel_kind(ChannelType::Text), ChannelKind::Text);
        assert_eq!(channel_kind(ChannelType::Voice), ChannelKind::Voice);
        assert_eq!(channel_kind(ChannelType::Stage), ChannelKind::Stage);
        assert_eq!(channel_kind(ChannelType::Category), ChannelKind::Category);
        assert_eq!(channel_kind(ChannelType::Forum), ChannelKind::Forum);
        assert_eq!(channel_kind(ChannelType::News), ChannelKind::Announcement);
        assert_eq!(channel_kind(ChannelType::Private), ChannelKind::Other);
    }

    #[test]
    fn threads_are_detected() {
        for t in [
            ChannelType::PublicThread,
            ChannelType::PrivateThread,
            ChannelType::NewsThread,
        ] {
            assert!(is_thread(t), "{t:?} should be a thread");
        }
        assert!(!is_thread(ChannelType::Text));
        assert!(!is_thread(ChannelType::Category));
    }
}
