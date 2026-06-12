//! Discord member and status-role value types and the pure role-decision logic.

use std::collections::HashMap;

use serenity::model::id::RoleId;

use crate::util::{DiscordHandle, DiscordUserId};
pub use domain::Role;

/// Discord-client-specific extension to the shared [`Role`] vocabulary.
///
/// Kept out of `domain` because the `DISCORD_ROLE_*_ID` override variable names
/// are this backend's configuration, not membership vocabulary.
pub(crate) trait RoleExt {
    /// Environment variable that supplies this role's `RoleId` directly,
    /// bypassing name lookup - useful in CI or a non-English guild.
    fn env_var(self) -> &'static str;
}

impl RoleExt for Role {
    fn env_var(self) -> &'static str {
        match self {
            Role::Member => "DISCORD_ROLE_MEMBER_ID",
            Role::DuesExpired => "DISCORD_ROLE_DUES_EXPIRED_ID",
            Role::Unverified => "DISCORD_ROLE_UNVERIFIED_ID",
        }
    }
}

/// A guild member, projected to what a role decision needs.
///
/// Returned by [`DiscordClient::list_members`](super::DiscordClient::list_members); a slim projection of serenity's
/// `Member` that drops everything not needed for a role decision.
#[derive(Debug, Clone)]
pub struct DiscordMember {
    /// The member's Discord user id.
    pub id: DiscordUserId,
    /// The member's username (handle).
    pub handle: DiscordHandle,
    /// Display name, resolved nick -> global name -> handle, skipping empties.
    pub display_name: String,
    /// The status role the member currently holds, if any. `None` means none of
    /// the three are present (not "unknown"). Populated by
    /// [`list_members`](super::DiscordClient::list_members) so callers can pass it to
    /// [`set_role`](super::DiscordClient::set_role) as a hint and skip no-op writes.
    pub current_status: Option<Role>,
    /// Every role the member holds, by name (numeric id for any role not present
    /// in the guild's role list), with the implicit `@everyone` excluded. Carried
    /// so a caller can show a member's full role set, not just the managed status
    /// role.
    pub role_names: Vec<String>,
    /// Whether this account is a bot. Bots are never members, so a role sweep
    /// skips them and counts them separately.
    pub bot: bool,
}

/// A managed status [`Role`] as resolved against the live guild at startup.
///
/// Carries the Discord role id the [`Role`] maps to, that role's current name on
/// the guild, and whether the id came from a `DISCORD_ROLE_*_ID` environment
/// override rather than a by-name match. Surfaced by
/// [`managed_roles`](super::DiscordClient::managed_roles) so a caller can echo its exact
/// write targets before any change - and so a fat-fingered override id is caught,
/// since its resolved `name` will not be the one expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRole {
    /// Which managed status role this is.
    pub role: Role,
    /// The Discord role id it resolves to.
    pub id: u64,
    /// The role's current name on the guild, or a placeholder if no guild role
    /// carries the resolved id (only possible for a bad override).
    pub name: String,
    /// `true` when the id came from the role's `DISCORD_ROLE_*_ID` override
    /// rather than a by-name match.
    pub from_env_override: bool,
}

/// A member's roles, split into every-role-by-name and the managed status roles
/// they hold.
///
/// `all_names` is every role the member holds, by name (for display); `held`
/// is which of the managed status [`Role`]s they currently hold, matched by
/// role id so it is correct even when the role names are overridden via the
/// `DISCORD_ROLE_*_ID` env vars. A removal strips exactly `held`.
#[derive(Debug, Clone, Default)]
pub struct MemberRoles {
    /// Every role the member holds, by name (or numeric id if unresolved).
    pub all_names: Vec<String>,
    /// The managed status roles the member currently holds.
    pub held: Vec<Role>,
}

/// Decision returned by `diff_status_roles`. Kept separate so the pure logic
/// is unit-testable without spinning up an `Http` client.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StatusDiff {
    NoOp,
    Apply { add: Role, remove: Option<Role> },
}

/// Computes the minimal role change: [`StatusDiff::NoOp`] when `current` already
/// equals `target`, otherwise an [`StatusDiff::Apply`] adding `target` and
/// removing whatever was held.
pub(crate) fn diff_status_roles(current: Option<Role>, target: Role) -> StatusDiff {
    if current == Some(target) {
        StatusDiff::NoOp
    } else {
        StatusDiff::Apply {
            add: target,
            remove: current,
        }
    }
}

/// Resolves a member's display name, preferring server nick, then global name,
/// then handle.
///
/// Empty strings are treated as absent at each step, so the result is never the
/// empty string even when the API returns an empty nick.
pub(crate) fn display_name_from(
    nick: Option<&str>,
    global_name: Option<&str>,
    handle: &str,
) -> String {
    let non_empty = |s: Option<&str>| s.filter(|v| !v.is_empty()).map(str::to_owned);
    non_empty(nick)
        .or_else(|| non_empty(global_name))
        .unwrap_or_else(|| handle.to_owned())
}

/// Determines which status role, if any, a member currently holds.
///
/// A member should hold at most one, but data drift can leave several; in that
/// case this warns and returns the highest-priority one per [`Role::ALL`]'s
/// order rather than failing.
pub(crate) fn pick_current_status(
    member_roles: &[RoleId],
    role_ids: &HashMap<Role, RoleId>,
) -> Option<Role> {
    let held: Vec<Role> = Role::ALL
        .into_iter()
        .filter(|r| role_ids.get(r).is_some_and(|id| member_roles.contains(id)))
        .collect();
    match held.len() {
        0 => None,
        1 => Some(held[0]),
        _ => {
            tracing::warn!(?held, "member holds multiple status roles; picking highest");
            Some(held[0])
        }
    }
}

/// Maps a member's role ids to display names, falling back to the numeric id for
/// any id absent from `names` (a role deleted since the guild list was fetched).
///
/// Shared by [`list_members`](super::DiscordClient::list_members) and
/// [`member_roles`](super::DiscordClient::member_roles) so both render a member's roles
/// the same way. Discord never lists the implicit `@everyone` role in a member's
/// `roles`, so it is naturally excluded.
pub(crate) fn role_names_for(
    member_roles: &[RoleId],
    names: &HashMap<RoleId, String>,
) -> Vec<String> {
    member_roles
        .iter()
        .map(|rid| names.get(rid).cloned().unwrap_or_else(|| rid.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u64) -> RoleId {
        RoleId::new(n)
    }

    fn role_id_map() -> HashMap<Role, RoleId> {
        HashMap::from([
            (Role::Member, rid(1)),
            (Role::DuesExpired, rid(2)),
            (Role::Unverified, rid(3)),
        ])
    }

    #[test]
    fn role_name_and_env_var_mapping() {
        assert_eq!(Role::Member.as_str(), "Member");
        assert_eq!(Role::DuesExpired.as_str(), "Dues Expired");
        assert_eq!(Role::Unverified.as_str(), "Unverified");

        assert_eq!(Role::Member.env_var(), "DISCORD_ROLE_MEMBER_ID");
        assert_eq!(Role::DuesExpired.env_var(), "DISCORD_ROLE_DUES_EXPIRED_ID");
        assert_eq!(Role::Unverified.env_var(), "DISCORD_ROLE_UNVERIFIED_ID");
    }

    #[test]
    fn diff_noop_when_already_target() {
        assert_eq!(
            diff_status_roles(Some(Role::Member), Role::Member),
            StatusDiff::NoOp
        );
    }

    #[test]
    fn diff_adds_only_when_no_current() {
        assert_eq!(
            diff_status_roles(None, Role::Member),
            StatusDiff::Apply {
                add: Role::Member,
                remove: None,
            }
        );
    }

    #[test]
    fn diff_swaps_when_different() {
        assert_eq!(
            diff_status_roles(Some(Role::DuesExpired), Role::Member),
            StatusDiff::Apply {
                add: Role::Member,
                remove: Some(Role::DuesExpired),
            }
        );
    }

    #[test]
    fn pick_status_none_when_no_status_role() {
        let map = role_id_map();
        // member holds only an unrelated role
        assert_eq!(pick_current_status(&[rid(99)], &map), None);
        // member holds no roles at all
        assert_eq!(pick_current_status(&[], &map), None);
    }

    #[test]
    fn pick_status_returns_held_role() {
        let map = role_id_map();
        assert_eq!(
            pick_current_status(&[rid(2), rid(99)], &map),
            Some(Role::DuesExpired)
        );
    }

    #[test]
    fn pick_status_picks_highest_when_multiple() {
        let map = role_id_map();
        // Member > DuesExpired > Unverified per Role::ALL
        assert_eq!(
            pick_current_status(&[rid(3), rid(2), rid(1)], &map),
            Some(Role::Member)
        );
        assert_eq!(
            pick_current_status(&[rid(3), rid(2)], &map),
            Some(Role::DuesExpired)
        );
    }

    #[test]
    fn role_names_for_maps_known_and_falls_back_to_id() {
        let names = HashMap::from([
            (rid(1), "Member".to_string()),
            (rid(2), "Volunteer".to_string()),
        ]);
        // rid(9) has no entry, so it renders as its numeric id.
        let got = role_names_for(&[rid(1), rid(9), rid(2)], &names);
        assert_eq!(
            got,
            vec![
                "Member".to_string(),
                "9".to_string(),
                "Volunteer".to_string()
            ]
        );
    }

    #[test]
    fn role_names_for_empty_is_empty() {
        let names: HashMap<RoleId, String> = HashMap::new();
        assert!(role_names_for(&[], &names).is_empty());
    }

    #[test]
    fn display_name_prefers_nick_then_global_then_handle() {
        assert_eq!(
            display_name_from(Some("nick"), Some("global"), "handle"),
            "nick"
        );
        assert_eq!(display_name_from(None, Some("global"), "handle"), "global");
        assert_eq!(display_name_from(None, None, "handle"), "handle");
        // Defensive: empty-string nick treated as absent so we don't ever
        // display "" as a name.
        assert_eq!(
            display_name_from(Some(""), Some("global"), "handle"),
            "global"
        );
        assert_eq!(display_name_from(Some(""), Some(""), "handle"), "handle");
    }
}
