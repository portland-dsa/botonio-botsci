//! The bot-side bridge from the live [`GuildConfig`] to the role-write client's
//! `Role -> RoleId` map. Pure so it is unit-tested without a gateway.

use std::collections::HashMap;

use domain::Role;
use engine::store::GuildConfig;
use serenity::model::id::RoleId;

/// The managed `Role -> RoleId` map for the role-write client, or `None` if any of
/// the three managed roles is unset. A write path that gets `None` reports that the
/// roles are not configured rather than acting on a partial map.
pub fn managed_role_map(cfg: &GuildConfig) -> Option<HashMap<Role, RoleId>> {
    Some(HashMap::from([
        (Role::Member, RoleId::new(cfg.member_role?.0)),
        (Role::DuesExpired, RoleId::new(cfg.dues_expired_role?.0)),
        (Role::Unverified, RoleId::new(cfg.unverified_role?.0)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::DiscordRoleId;

    #[test]
    fn none_until_all_three_managed_roles_set() {
        let mut cfg = GuildConfig::default();
        assert!(managed_role_map(&cfg).is_none());
        cfg.member_role = Some(DiscordRoleId(1));
        cfg.dues_expired_role = Some(DiscordRoleId(2));
        assert!(managed_role_map(&cfg).is_none(), "still missing unverified");
        cfg.unverified_role = Some(DiscordRoleId(3));
        let map = managed_role_map(&cfg).expect("all three set");
        assert_eq!(map[&Role::Member], RoleId::new(1));
        assert_eq!(map[&Role::DuesExpired], RoleId::new(2));
        assert_eq!(map[&Role::Unverified], RoleId::new(3));
    }
}
