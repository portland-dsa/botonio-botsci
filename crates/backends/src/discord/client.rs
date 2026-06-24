//! The object-safe `DiscordClient` trait (with a hand-written fake under the `fakes` feature).

use async_trait::async_trait;

use crate::MemberPage;
use crate::util::{DiscordChannelId, DiscordGuildId, DiscordUserId};

use super::channels::{GuildChannels, PermOverwrite};
use super::error::DiscordError;
use super::roles::{DiscordRosterMember, ManagedRole, MemberRoles, Role};

/// Async, object-safe interface for the bot's guild role operations.
///
/// A hand-written, state-based fake is available under the `fakes` feature so
/// callers can be tested without a live bot token; the production implementation
/// is [`DiscordHttp`](super::DiscordHttp).
///
/// These are the guild-state writes - plus the reads that inform them - that the
/// bot's role-verification and scheduled-scan work drives.
#[async_trait]
pub trait DiscordClient: Send + Sync {
    /// Returns the guild's [`DiscordGuildId`], as resolved during construction.
    fn guild_id(&self) -> DiscordGuildId;

    /// Returns the three managed status roles as resolved at construction (id,
    /// name, and override provenance), so a caller can echo its exact write
    /// targets before changing anything.
    ///
    /// Read-only and network-free - the resolution happened once before the
    /// client was built (see [`resolve_managed_roles`](super::resolve_managed_roles)).
    fn managed_roles(&self) -> Vec<ManagedRole>;

    /// Sets `target` as the member's status role, removing whichever other
    /// status role they held.
    ///
    /// `current` is the member's existing status, typically taken from a
    /// [`member_roles`](DiscordClient::member_roles) read (its `held` is in
    /// priority order, so the first element is the current status). When it
    /// already equals `target` the call is a no-op, so pass `None` only if you
    /// genuinely don't know. The add happens before the remove, so the member is
    /// never momentarily roleless.
    async fn set_role(
        &self,
        user: DiscordUserId,
        current: Option<Role>,
        target: Role,
    ) -> Result<(), DiscordError>;

    /// Removes the given managed status roles from the member.
    ///
    /// Strips exactly the roles passed - typically the ones a
    /// [`member_roles`](DiscordClient::member_roles) read reported the member
    /// holds. Each removal is idempotent - Discord returns success when the member
    /// lacks the role - and an empty slice is a no-op. No role is added.
    async fn remove_roles(&self, user: DiscordUserId, roles: &[Role]) -> Result<(), DiscordError>;

    /// Returns the member's roles: every role by name, plus which managed status
    /// roles they currently hold.
    ///
    /// Surfaces a member's roles for display and names the exact managed roles a
    /// [`remove_roles`](DiscordClient::remove_roles) call would strip. Names come
    /// from the guild role list (an id with no matching role is rendered as its
    /// numeric value; the implicit `@everyone` role is omitted). `held` is matched
    /// by role id, so it stays correct under `DISCORD_ROLE_*_ID` name overrides,
    /// and is in [`Role::ALL`] priority order.
    async fn member_roles(&self, user: DiscordUserId) -> Result<MemberRoles, DiscordError>;

    /// One page of the guild's members, projected to [`DiscordRosterMember`].
    ///
    /// `cursor` is the opaque "after" snowflake from the previous page's
    /// [`next`](crate::MemberPage::next); `None` starts at the lowest id. Drained by
    /// the engine's `paging::drain_pages`, like Solidarity Tech's `members_page`.
    /// Needs the `GUILD_MEMBERS` privileged intent (enforced on the REST list too).
    async fn members_page(
        &self,
        cursor: Option<&str>,
    ) -> Result<MemberPage<DiscordRosterMember>, DiscordError>;

    /// Add the configured Manual Override marker role to `user`, leaving their status
    /// roles untouched. Errors with [`DiscordError::OverrideRoleUnconfigured`] when no
    /// marker role is configured.
    async fn assign_override_marker(&self, user: DiscordUserId) -> Result<(), DiscordError>;

    /// Remove the Manual Override marker role from `user`. A no-op (still `Ok`) when the
    /// member does not hold it, and likewise when no marker role is configured - there is
    /// then nothing to remove.
    async fn remove_override_marker(&self, user: DiscordUserId) -> Result<(), DiscordError>;

    /// Reads every channel in the guild with its permission overwrites, plus
    /// whether the `@everyone` guild role grants `VIEW_CHANNEL` at the base level.
    ///
    /// REST only - the terraform reads the whole tree once, then plans offline.
    async fn read_channels(&self) -> Result<GuildChannels, DiscordError>;

    /// Replaces a channel's whole permission-overwrite array (Discord's atomic
    /// unit). The terraform never edits a single overwrite in place; it computes
    /// the full desired array and PATCHes it.
    async fn set_channel_overwrites(
        &self,
        id: DiscordChannelId,
        overwrites: &[PermOverwrite],
    ) -> Result<(), DiscordError>;
}
