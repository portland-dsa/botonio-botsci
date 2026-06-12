//! The object-safe `DiscordClient` trait (with a `mockall` mock under the `mock` feature).

use async_trait::async_trait;

use crate::util::{DiscordGuildId, DiscordUserId, DryRun};

use super::channels::DiscordChannel;
use super::error::DiscordError;
use super::roles::{DiscordMember, ManagedRole, MemberRoles, Role};

/// Async, object-safe interface for the bot's guild operations.
///
/// `mockall` generates a mock under `#[cfg(test)]` so callers can be tested
/// without a live bot token; the production implementation is
/// [`DiscordHttp`](super::DiscordHttp).
///
/// None of these methods are called by the bot's current read-only path, which
/// builds its member index from Solidarity Tech rather than the gateway. They are
/// retained for the role-writing and verification features still to come, so each
/// is flagged below with where it stands.
#[async_trait]
#[cfg_attr(feature = "mock", mockall::automock)]
pub trait DiscordClient: Send + Sync {
    /// Returns the guild's [`DiscordGuildId`], as resolved during construction.
    fn guild_id(&self) -> DiscordGuildId;

    /// Returns the three managed status roles as resolved at construction (id,
    /// name, and override provenance), so a caller can echo its exact write
    /// targets before changing anything.
    ///
    /// Read-only and network-free - the resolution happened once in
    /// [`from_env`](super::DiscordHttp::from_env). Not yet called by the bot.
    fn managed_roles(&self) -> Vec<ManagedRole>;

    /// Returns every guild member, each with its [`current_status`] pre-filled.
    ///
    /// Members are paged from the API in batches; pre-populating
    /// [`current_status`] lets the caller pass it back into [`set_role`] to skip
    /// writes that would be no-ops. Not yet called by the bot.
    ///
    /// [`current_status`]: DiscordMember::current_status
    /// [`set_role`]: DiscordClient::set_role
    async fn list_members(&self) -> Result<Vec<DiscordMember>, DiscordError>;

    /// Sets `target` as the member's status role, removing whichever other
    /// status role they held.
    ///
    /// `current` is the hint from [`list_members`](DiscordClient::list_members);
    /// when it already equals `target` the call is a no-op, so pass `None` only
    /// if you genuinely don't know. The add happens before the remove, so the
    /// member is never momentarily roleless. Honors [`DryRun`]: when dry, the
    /// intended change is logged at `info` and nothing is sent. Not yet called by
    /// the bot.
    async fn set_role(
        &self,
        user: DiscordUserId,
        current: Option<Role>,
        target: Role,
        dry_run: DryRun,
    ) -> Result<(), DiscordError>;

    /// Removes the given managed status roles from the member.
    ///
    /// Strips exactly the roles passed - typically the ones a
    /// [`member_roles`](DiscordClient::member_roles) read reported the member
    /// holds. Each removal is idempotent - Discord returns success when the member
    /// lacks the role - and an empty slice is a no-op. No role is added. Honors
    /// [`DryRun`]: when dry, the intended removals are logged at `info` and nothing
    /// is sent. Not yet called by the bot.
    async fn remove_roles(
        &self,
        user: DiscordUserId,
        roles: &[Role],
        dry_run: DryRun,
    ) -> Result<(), DiscordError>;

    /// Returns the member's roles: every role by name, plus which managed status
    /// roles they currently hold.
    ///
    /// Surfaces a member's roles for display and names the exact managed roles a
    /// [`remove_roles`](DiscordClient::remove_roles) call would strip. Names come
    /// from the guild role list (an id with no matching role is rendered as its
    /// numeric value; the implicit `@everyone` role is omitted). `held` is matched
    /// by role id, so it stays correct under `DISCORD_ROLE_*_ID` name overrides.
    /// This is a read, so it takes no [`DryRun`]. Not yet called by the bot.
    async fn member_roles(&self, user: DiscordUserId) -> Result<MemberRoles, DiscordError>;

    /// Returns every guild channel - categories included, threads excluded - each
    /// projected to a [`DiscordChannel`]. A read, so no [`DryRun`].
    ///
    /// Not yet called by the bot; kept for the planned moderator `/setup` command,
    /// which will let a mod pick channels from a menu instead of pasting raw ids.
    async fn list_channels(&self) -> Result<Vec<DiscordChannel>, DiscordError>;

    /// Returns every role id on the guild, for validating configured role ids
    /// against the roles that actually exist. A read, so no [`DryRun`].
    ///
    /// Not yet called by the bot; kept for the planned moderator `/setup` command's
    /// role picker.
    async fn list_role_ids(&self) -> Result<Vec<u64>, DiscordError>;
}
