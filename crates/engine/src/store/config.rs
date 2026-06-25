//! Guild-level configuration and channel-permission snapshots: the two guild-scoped,
//! non-member stores.
//!
//! [`GuildConfig`] is the `/setup`-managed settings row (every field optional until a
//! moderator configures it); [`MessageRef`] is a posted-message pointer it carries.
//! [`ChannelSnapshotStore`] is the save/restore behind the channel-terraform's recovery.
//! Both [`InMemoryStore`] impls reach the store's private fields from the hub.

use std::convert::Infallible;

use async_trait::async_trait;

use domain::{DiscordChannelId, DiscordGuildId, DiscordMessageId, DiscordRoleId};

use crate::channels::snapshot::{ChannelSnapshot, SnapshotMeta};

use super::InMemoryStore;

/// A reference to one standing message the bot posted and can later edit in place:
/// the channel it lives in and its id. Held for each message `/setup` publishes (the
/// verification prompt, the dues-expiring banner) so a re-publish edits the existing
/// message rather than posting a duplicate. The two halves are persisted and read back
/// together, so the reference is present or absent as a unit - there is no
/// "message id but no channel" state to represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageRef {
    pub channel: DiscordChannelId,
    pub message: DiscordMessageId,
}

/// The per-guild runtime configuration set through the bot's `/setup` command:
/// the moderator role, the three managed status roles, the additive Manual Override
/// marker, and the verification channels. Every field is optional - a freshly
/// deployed guild has nothing set until a moderator configures it. Built from id
/// newtypes so a store maps it to a single nullable-column row with no nesting, exactly
/// like [`MemberRecord`](super::MemberRecord).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuildConfig {
    pub moderator_role: Option<DiscordRoleId>,
    pub member_role: Option<DiscordRoleId>,
    pub dues_expired_role: Option<DiscordRoleId>,
    pub unverified_role: Option<DiscordRoleId>,
    /// The additive Manual Override marker role, granted alongside `Member` on a hand
    /// approval. Optional and outside the status trichotomy: ordinary verification works
    /// without it, and it is never stripped by the status-role logic.
    pub manual_override_role: Option<DiscordRoleId>,
    /// The additive Dues Expiring marker, granted while a member is inside the pre-lapse
    /// window; outside the status trichotomy and never stripped by status logic.
    pub dues_expiring_role: Option<DiscordRoleId>,
    pub mod_approval_channel: Option<DiscordChannelId>,
    pub unverified_channel: Option<DiscordChannelId>,
    pub dues_expired_channel: Option<DiscordChannelId>,
    /// The moderator-private channel that logs every successful self-service
    /// verification - the member and the email that matched. Unset by default;
    /// when unset the grant still happens and only the log post is skipped.
    pub verification_log_channel: Option<DiscordChannelId>,
    /// The external dues sign-up page the reminder "Renew" button links to.
    pub dues_signup_url: Option<String>,
    /// Where the standing verification prompt was last posted, if at all. Set when a
    /// moderator publishes the prompt through `/setup`; a later publish edits this
    /// message in place rather than posting a duplicate. Bot bookkeeping, not a setting
    /// the moderator picks - see [`MessageRef`].
    pub unverified_prompt: Option<MessageRef>,
    /// Where the standing dues-expiring banner was last posted, if at all. The dues
    /// counterpart to [`unverified_prompt`](Self::unverified_prompt).
    pub dues_banner: Option<MessageRef>,
    /// Whether the dues-reminder sweep runs for this guild. Off by default, like
    /// [`scan_enabled`](Self::scan_enabled); the two toggle independently.
    pub reminders_enabled: bool,
    /// Whether the scheduled membership scan runs for this guild. Off by default - the
    /// scan reconciles roles and can demote, so it is opt-in via /setup.
    pub scan_enabled: bool,
}

/// Read and replace one guild's [`GuildConfig`]. Fallible and async from the start
/// for the same reason as the other store traits: the in-memory impl's
/// [`Error`](ConfigStore::Error) is [`Infallible`], a Postgres-backed one's is a
/// database error. `save_config` replaces the whole row (last-writer-wins); config
/// is admin-only and low-frequency, so no per-field write path is needed.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Load the config for `guild`, returning the default (all-unset) when the guild
    /// has no stored row yet.
    async fn load_config(&self, guild: DiscordGuildId) -> Result<GuildConfig, Self::Error>;

    /// Replace `guild`'s stored config wholesale.
    async fn save_config(
        &self,
        guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Self::Error>;
}

/// Persist and recall whole-guild channel-permission snapshots - the save/restore
/// behind the terraform's disaster recovery. Fallible from the start, like the
/// other store traits; the in-memory impl's [`Error`](ChannelSnapshotStore::Error)
/// is [`Infallible`].
#[async_trait]
pub trait ChannelSnapshotStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Append a snapshot (history is kept; never overwrite an earlier one).
    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), Self::Error>;

    /// The most recent snapshot for `guild`, or `None` if none was ever saved.
    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, Self::Error>;

    /// All snapshots' metadata for `guild`, newest first - for the restore picker.
    async fn list_snapshots(&self, guild: DiscordGuildId)
    -> Result<Vec<SnapshotMeta>, Self::Error>;
}

#[async_trait]
impl ConfigStore for InMemoryStore {
    type Error = Infallible;

    async fn load_config(&self, _guild: DiscordGuildId) -> Result<GuildConfig, Infallible> {
        Ok(self.config.read().expect("config lock poisoned").clone())
    }

    async fn save_config(
        &self,
        _guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Infallible> {
        *self.config.write().expect("config lock poisoned") = config.clone();
        Ok(())
    }
}

#[async_trait]
impl ChannelSnapshotStore for InMemoryStore {
    type Error = Infallible;

    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), Infallible> {
        self.snapshots
            .write()
            .expect("snapshots lock poisoned")
            .push(snapshot.clone());
        Ok(())
    }

    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, Infallible> {
        Ok(self
            .snapshots
            .read()
            .expect("snapshots lock poisoned")
            .iter()
            .rfind(|s| s.guild_id == guild)
            .cloned())
    }

    async fn list_snapshots(&self, guild: DiscordGuildId) -> Result<Vec<SnapshotMeta>, Infallible> {
        let guard = self.snapshots.read().expect("snapshots lock poisoned");
        let mut metas: Vec<SnapshotMeta> = guard
            .iter()
            .filter(|s| s.guild_id == guild)
            .map(|s| SnapshotMeta {
                saved_at: s.saved_at,
                channel_count: s.channels.len(),
            })
            .collect();
        // Newest first for the restore picker.
        metas.reverse();
        Ok(metas)
    }
}
