//! The object-safe `SolidarityTechClient` trait and its `StClearFlags` input.

use async_trait::async_trait;

use crate::MemberPage;
use crate::util::{DiscordHandle, DiscordUserId, DryRun, Email, Phone};

use super::error::SolidarityTechError;
use super::member::{CustomUserProperty, SolidarityTechMember};

/// Which Discord identity properties [`clear_discord_identity`] blanks.
///
/// The two flags are independent; with both `false` the call is a no-op. A named
/// pair rather than two positional `bool`s so a call site reads which property it
/// clears.
///
/// [`clear_discord_identity`]: SolidarityTechClient::clear_discord_identity
#[derive(Debug, Clone, Copy)]
pub struct StClearFlags {
    /// Blank the `discord-handle` custom property.
    pub handle: bool,
    /// Blank the `discord-user-id` custom property.
    pub user_id: bool,
}

/// Reads Solidarity Tech members and writes Discord identity back to them.
///
/// The trait is object-safe and `async` (through the `async_trait` crate), and
/// gets a `mockall` mock under `#[cfg(test)]` so callers can be unit-tested
/// without a live API. The production implementation is
/// [`SolidarityTechHttp`](super::SolidarityTechHttp).
///
/// The bot's current read-only path calls only
/// [`members_in_list_page`](Self::members_in_list_page), to build its membership
/// index. The lookup, write, and property-discovery methods are retained for the
/// verification and write features still to come; each is flagged below.
#[async_trait]
#[cfg_attr(feature = "mock", mockall::automock)]
pub trait SolidarityTechClient: Send + Sync {
    /// Look up members by email. Returns the full match list (usually 0 or 1);
    /// thin wrapper over [`find_members`](Self::find_members).
    async fn find_by_email(
        &self,
        email: &Email,
    ) -> Result<Vec<SolidarityTechMember>, SolidarityTechError> {
        self.find_members(Some(email), None).await
    }

    /// Look up members by phone number. Returns the full match list; thin
    /// wrapper over [`find_members`](Self::find_members).
    async fn find_by_phone(
        &self,
        phone: &Phone,
    ) -> Result<Vec<SolidarityTechMember>, SolidarityTechError> {
        self.find_members(None, Some(phone)).await
    }

    /// Looks up members, ANDing whatever is provided to raise specificity.
    ///
    /// At least one of `email`/`phone` must be `Some`, otherwise this returns
    /// [`NoQueryCriteria`] before sending anything. Only the first page is
    /// fetched (see `ST_PAGE_SIZE`); a unique email or phone yields 0-1 rows in
    /// practice, but if the API reports more matches than were returned the call
    /// warns rather than silently truncating. The match list is returned as-is -
    /// de-duplication is the caller's responsibility. Not yet called by the bot.
    ///
    /// [`NoQueryCriteria`]: SolidarityTechError::NoQueryCriteria
    async fn find_members(
        &self,
        email: Option<&Email>,
        phone: Option<&Phone>,
    ) -> Result<Vec<SolidarityTechMember>, SolidarityTechError>;

    /// Fetches one page of the `/users` collection (by `_offset`/`_limit`),
    /// returning the members that project cleanly. A record that can't be
    /// projected at all (no parseable email) is warn-logged and skipped, but a
    /// bad custom-property value still fails the read, matching
    /// [`find_members`](Self::find_members). `cursor` is `None` for the first
    /// page; otherwise it is the prior page's [`next`](MemberPage::next).
    ///
    /// Solidarity Tech reports `meta.total_count`, so [`total`](MemberPage::total)
    /// is populated - a front end can size a determinate bar from it. Not yet
    /// called by the bot, which sweeps a pre-filtered list via
    /// [`members_in_list_page`](Self::members_in_list_page) instead.
    async fn members_page(
        &self,
        cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError>;

    /// Fetches one page of the members in the given Solidarity Tech user list
    /// (`GET /users?user_list_ids={list_id}`), same paging contract as
    /// [`members_page`](Self::members_page). Used to build the bot's member index
    /// from a list pre-filtered to Discord-linked members, instead of sweeping the
    /// whole collection.
    async fn members_in_list_page(
        &self,
        list_id: &str,
        cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError>;

    /// Sets only the `discord-handle` custom property on a member via
    /// `PUT /users/{id}`, overwriting any existing value.
    ///
    /// A handle-only counterpart to
    /// [`set_discord_identity`](Self::set_discord_identity), for a caller that has
    /// a handle but not always a Discord user id. Because the property is org-level
    /// the PUT is a safe merge - `discord-user-id` and every other property are
    /// untouched. Honors [`DryRun`]: when dry, logs at `info` and returns `Ok(())`
    /// without touching the network. Surfaces [`SolidarityTechError`] on a non-2xx
    /// response. Not yet called by the bot.
    async fn set_discord_handle(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError>;

    /// Sets only the `alternate-email` custom property on a member via
    /// `PUT /users/{id}`, overwriting any existing value.
    ///
    /// The alternate-email counterpart to
    /// [`set_discord_handle`](Self::set_discord_handle). Because the property is
    /// org-level the PUT is a safe merge - every other property is untouched.
    /// Honors [`DryRun`]: when dry, logs at `info` and returns `Ok(())` without
    /// touching the network. Surfaces [`SolidarityTechError`] on a non-2xx
    /// response. Not yet called by the bot.
    async fn set_alternate_email(
        &self,
        member_id: &str,
        alternate_email: &Email,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError>;

    /// Sets the Discord handle and user-id custom properties on a member via
    /// `PUT /users/{id}`.
    ///
    /// Because these are org-level properties the PUT is a merge, so the
    /// member's other properties are left untouched. Honors [`DryRun`]: when dry,
    /// the call is logged at `info` and returns `Ok(())` without touching the
    /// network. Surfaces [`SolidarityTechError`] on a non-2xx response. Not yet
    /// called by the bot.
    async fn set_discord_identity(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
        id: DiscordUserId,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError>;

    /// Blanks the Discord handle and/or user-id custom properties on a member via
    /// `PUT /users/{id}`.
    ///
    /// The inverse of [`set_discord_identity`](Self::set_discord_identity): each
    /// property [`flags`](StClearFlags) requests is sent as an empty string.
    /// Because these are org-level properties the PUT is a merge, so unlisted
    /// properties - including the one the caller did not ask to clear - are left
    /// untouched. With both flags `false` this is a no-op. Honors [`DryRun`]:
    /// when dry, logs at `info` and returns `Ok(())` without touching the
    /// network. Surfaces [`SolidarityTechError`] on a non-2xx response. Not yet
    /// called by the bot.
    async fn clear_discord_identity(
        &self,
        member_id: &str,
        flags: StClearFlags,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError>;

    /// Lists the org's custom user property definitions, so the internal `key`s
    /// behind "Discord Handle" and "Discord User ID" can be discovered.
    ///
    /// How the property keys in `StReadProps`/`StWriteProps` get verified against
    /// the live org. Not yet called by the bot; a property-key diagnostic for
    /// confirming a key before trusting a write.
    async fn list_custom_user_properties(
        &self,
    ) -> Result<Vec<CustomUserProperty>, SolidarityTechError>;
}
