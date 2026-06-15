//! The object-safe `DiscordClient` trait (with a `mockall` mock under the `mock` feature).

use async_trait::async_trait;

use crate::util::{DiscordGuildId, DiscordUserId, DryRun};

use super::error::DiscordError;
use super::roles::{ManagedRole, MemberRoles, Role};

/// Async, object-safe interface for the bot's guild role operations.
///
/// `mockall` generates a mock under the `mock` feature so callers can be tested
/// without a live bot token; the production implementation is
/// [`DiscordHttp`](super::DiscordHttp).
///
/// These are the guild-state writes - plus the reads that inform them - that the
/// bot's role-verification and scheduled-scan work will drive. None are exercised
/// by the current read-only card path, which builds its member index from
/// Solidarity Tech rather than the gateway, so each is reserved for slice 2.
#[async_trait]
#[cfg_attr(feature = "mock", mockall::automock)]
pub trait DiscordClient: Send + Sync {
    /// Returns the guild's [`DiscordGuildId`], as resolved during construction.
    fn guild_id(&self) -> DiscordGuildId;

    /// Returns the three managed status roles as resolved at construction (id,
    /// name, and override provenance), so a caller can echo its exact write
    /// targets before changing anything.
    ///
    /// Read-only and network-free - the resolution happened once before the
    /// client was built (see [`resolve_managed_roles`](super::resolve_managed_roles)).
    /// Reserved for slice 2.
    fn managed_roles(&self) -> Vec<ManagedRole>;

    /// Sets `target` as the member's status role, removing whichever other
    /// status role they held.
    ///
    /// `current` is the member's existing status, typically taken from a
    /// [`member_roles`](DiscordClient::member_roles) read (its `held` is in
    /// priority order, so the first element is the current status). When it
    /// already equals `target` the call is a no-op, so pass `None` only if you
    /// genuinely don't know. The add happens before the remove, so the member is
    /// never momentarily roleless. Honors [`DryRun`]: when dry, the intended
    /// change is logged at `info` and nothing is sent. Reserved for slice 2.
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
    /// is sent. Reserved for slice 2.
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
    /// by role id, so it stays correct under `DISCORD_ROLE_*_ID` name overrides,
    /// and is in [`Role::ALL`] priority order. This is a read, so it takes no
    /// [`DryRun`]. Reserved for slice 2.
    async fn member_roles(&self, user: DiscordUserId) -> Result<MemberRoles, DiscordError>;
}
