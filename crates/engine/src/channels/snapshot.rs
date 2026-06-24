//! The channel-permission snapshot: every channel's overwrites at a moment, so a
//! `setup` (or any mistake) can be rolled back. Persisted by a
//! [`ChannelSnapshotStore`](crate::store::ChannelSnapshotStore); the value itself
//! is pure and serde-round-trippable.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use domain::{DiscordChannelId, DiscordGuildId};

use crate::backends::discord::{ChannelKind, DiscordChannel, PermOverwrite};

/// Bumped whenever the snapshot layout changes incompatibly.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// A full channel-permission snapshot of one guild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    pub format_version: u32,
    pub guild_id: DiscordGuildId,
    pub saved_at: DateTime<Utc>,
    pub channels: Vec<SavedChannel>,
}

/// One channel's saved overwrites. Position is intentionally dropped - restore
/// keys on id and only rewrites overwrites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChannel {
    pub id: DiscordChannelId,
    pub name: String,
    pub kind: ChannelKind,
    pub parent_id: Option<DiscordChannelId>,
    pub overwrites: Vec<PermOverwrite>,
}

impl ChannelSnapshot {
    /// Build a snapshot from a live channel list.
    pub fn from_channels(
        guild_id: DiscordGuildId,
        channels: &[DiscordChannel],
        saved_at: DateTime<Utc>,
    ) -> Self {
        ChannelSnapshot {
            format_version: SNAPSHOT_FORMAT_VERSION,
            guild_id,
            saved_at,
            channels: channels
                .iter()
                .map(|c| SavedChannel {
                    id: c.id,
                    name: c.name.clone(),
                    kind: c.kind,
                    parent_id: c.parent_id,
                    overwrites: c.overwrites.clone(),
                })
                .collect(),
        }
    }
}

/// Lightweight snapshot listing for the restore picker: when, and how many channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub saved_at: DateTime<Utc>,
    pub channel_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::discord::Permissions;
    use crate::backends::discord::{DiscordChannel, OverwriteTarget, PermOverwrite};
    use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId};

    fn make_channel(id: u64, name: &str) -> DiscordChannel {
        DiscordChannel {
            id: DiscordChannelId(id),
            name: name.to_owned(),
            kind: ChannelKind::Text,
            parent_id: None,
            position: 0,
            overwrites: vec![PermOverwrite {
                target: OverwriteTarget::Role(DiscordRoleId(100)),
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::empty(),
            }],
        }
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        use chrono::TimeZone;
        let guild = DiscordGuildId(42);
        let saved_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        let channels = vec![make_channel(1, "general"), make_channel(2, "members-only")];
        let snap = ChannelSnapshot::from_channels(guild, &channels, saved_at);

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: ChannelSnapshot = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back, snap);
        assert_eq!(back.channels.len(), 2);
        assert_eq!(back.format_version, SNAPSHOT_FORMAT_VERSION);
        assert_eq!(back.guild_id, guild);
    }

    #[test]
    fn from_channels_drops_position_keeps_overwrites() {
        use chrono::TimeZone;
        let guild = DiscordGuildId(7);
        let saved_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut ch = make_channel(5, "test-chan");
        ch.position = 99; // position must be dropped
        ch.parent_id = Some(DiscordChannelId(3));

        let snap = ChannelSnapshot::from_channels(guild, &[ch], saved_at);
        assert_eq!(snap.channels.len(), 1);
        let saved = &snap.channels[0];
        assert_eq!(saved.id, DiscordChannelId(5));
        assert_eq!(saved.name, "test-chan");
        assert_eq!(saved.parent_id, Some(DiscordChannelId(3)));
        assert_eq!(saved.overwrites.len(), 1);
    }
}
