//! Discord member and status-role value types and the pure role-decision logic.

use std::collections::HashMap;

use serenity::model::id::RoleId;

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

/// Maps a member's role ids to display names, falling back to the numeric id for
/// any id absent from `names` (a role deleted since the guild list was fetched).
///
/// Used by [`member_roles`](super::DiscordClient::member_roles) to render a
/// member's roles. Discord never lists the implicit `@everyone` role in a
/// member's `roles`, so it is naturally excluded.
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
}
