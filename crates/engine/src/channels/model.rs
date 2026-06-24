//! The pure permission-bit math and the [`SetupConfig`] the classifier consumes.
//! Nothing here touches the network. Visibility is one bit, [`VIEW`]; the two
//! restricted channels additionally move [`SEND`]. Every transform copies the
//! channel's overwrites verbatim and flips only the named bit on the named
//! target, so no other permission or role is disturbed.

use std::collections::BTreeSet;

use domain::{DiscordChannelId, DiscordRoleId, DiscordUserId};

use crate::backends::discord::{DiscordChannel, OverwriteTarget, PermOverwrite, Permissions};

/// The single visibility bit the terraform reasons over.
pub const VIEW: Permissions = Permissions::VIEW_CHANNEL;
/// The posting bit, denied to `@everyone` only in the two restricted channels.
pub const SEND: Permissions = Permissions::SEND_MESSAGES;

/// Everything the classifier needs beyond the channel list itself, built by the
/// bot from `GuildConfig`. All ids are newtypes - no bare primitive crosses in.
#[derive(Debug, Clone)]
pub struct SetupConfig {
    /// The `@everyone` role id (equals the guild id).
    pub everyone: DiscordRoleId,
    pub member_role: DiscordRoleId,
    pub dues_expired_role: DiscordRoleId,
    pub unverified_role: DiscordRoleId,
    pub moderator_role: DiscordRoleId,
    /// The bot's own user id, granted VIEW+SEND in the restricted channels so the
    /// `@everyone` SEND deny never stops it posting the self-check button / notice.
    pub bot_user: DiscordUserId,
    pub unverified_channels: BTreeSet<DiscordChannelId>,
    pub dues_expired_channels: BTreeSet<DiscordChannelId>,
    /// Channels the sweep must never touch - the bot seeds the configured mod
    /// channels here as defense in depth.
    pub exclude_channels: BTreeSet<DiscordChannelId>,
}

/// Sets or clears `bit` for `target` within an overwrite set, preserving every
/// other bit and overwrite. Granting adds to `allow` and clears from `deny`;
/// denying does the reverse, so the result is never self-contradictory. A missing
/// overwrite is created carrying only `bit`.
pub fn set_perm(
    ows: &[PermOverwrite],
    target: OverwriteTarget,
    bit: Permissions,
    grant: bool,
) -> Vec<PermOverwrite> {
    let mut out = ows.to_vec();
    if let Some(o) = out.iter_mut().find(|o| o.target == target) {
        if grant {
            o.allow |= bit;
            o.deny &= !bit;
        } else {
            o.deny |= bit;
            o.allow &= !bit;
        }
    } else {
        out.push(PermOverwrite {
            target,
            allow: if grant { bit } else { Permissions::empty() },
            deny: if grant { Permissions::empty() } else { bit },
        });
    }
    out
}

/// Canonical form for comparing overwrite sets: drop empty overwrites, sort by
/// target. The one normalizer shared by [`overwrites_equal`], the synced check,
/// the restore no-op skip, and the drift guard, so they can never disagree.
pub fn normalize(ows: &[PermOverwrite]) -> Vec<PermOverwrite> {
    let mut v: Vec<PermOverwrite> = ows
        .iter()
        .copied()
        .filter(|o| !(o.allow.is_empty() && o.deny.is_empty()))
        .collect();
    v.sort_by_key(|o| o.target);
    v
}

/// Whether two overwrite sets are equal once normalized (order- and
/// empty-overwrite-insensitive).
pub fn overwrites_equal(a: &[PermOverwrite], b: &[PermOverwrite]) -> bool {
    normalize(a) == normalize(b)
}

/// Whether a child is synced to its parent - Discord's own definition: equal
/// overwrite sets.
pub fn overwrites_synced(child: &[PermOverwrite], parent: &[PermOverwrite]) -> bool {
    overwrites_equal(child, parent)
}

/// Whether `@everyone` can effectively view this channel: its `@everyone`
/// overwrite decides if it touches `VIEW` (deny wins over allow), else the base.
pub fn everyone_can_view(ch: &DiscordChannel, everyone: DiscordRoleId, base_view: bool) -> bool {
    let target = OverwriteTarget::Role(everyone);
    for o in &ch.overwrites {
        if o.target == target {
            if o.deny.contains(VIEW) {
                return false;
            }
            if o.allow.contains(VIEW) {
                return true;
            }
            return base_view;
        }
    }
    base_view
}

/// Whether a member holding exactly `roles` (plus `@everyone`) can view a channel
/// with these overwrites, under Discord's deny-then-allow role precedence. Exact
/// for any channel this tool writes, because every role it grants gets an explicit
/// allow overwrite.
pub fn role_can_view(
    ows: &[PermOverwrite],
    everyone: DiscordRoleId,
    base_view: bool,
    roles: &[DiscordRoleId],
) -> bool {
    let find = |t: OverwriteTarget| ows.iter().find(|o| o.target == t);
    let mut allowed = base_view;
    if let Some(o) = find(OverwriteTarget::Role(everyone)) {
        if o.deny.contains(VIEW) {
            allowed = false;
        }
        if o.allow.contains(VIEW) {
            allowed = true;
        }
    }
    let mut role_deny = false;
    let mut role_allow = false;
    for &r in roles {
        if let Some(o) = find(OverwriteTarget::Role(r)) {
            role_deny |= o.deny.contains(VIEW);
            role_allow |= o.allow.contains(VIEW);
        }
    }
    if role_deny {
        allowed = false;
    }
    if role_allow {
        allowed = true;
    }
    allowed
}

/// The Member-only array: deny `@everyone` VIEW, allow `Member` VIEW; everything
/// else verbatim. Idempotent.
pub fn lockdown_member(
    current: &[PermOverwrite],
    everyone: DiscordRoleId,
    member: DiscordRoleId,
) -> Vec<PermOverwrite> {
    let out = set_perm(current, OverwriteTarget::Role(everyone), VIEW, false);
    set_perm(&out, OverwriteTarget::Role(member), VIEW, true)
}

/// The restricted array for an unverified/dues-expired channel: deny `@everyone`
/// {VIEW, SEND}; allow `allow_role` VIEW (read + click buttons, no posting); allow
/// the moderator role {VIEW, SEND}; allow the bot {VIEW, SEND}. Idempotent.
pub fn restrict(
    current: &[PermOverwrite],
    cfg: &SetupConfig,
    allow_role: DiscordRoleId,
) -> Vec<PermOverwrite> {
    let e = OverwriteTarget::Role(cfg.everyone);
    let mut out = set_perm(current, e, VIEW, false);
    out = set_perm(&out, e, SEND, false);
    out = set_perm(&out, OverwriteTarget::Role(allow_role), VIEW, true);
    out = set_perm(&out, OverwriteTarget::Role(cfg.moderator_role), VIEW, true);
    out = set_perm(&out, OverwriteTarget::Role(cfg.moderator_role), SEND, true);
    out = set_perm(&out, OverwriteTarget::Member(cfg.bot_user), VIEW, true);
    out = set_perm(&out, OverwriteTarget::Member(cfg.bot_user), SEND, true);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DiscordChannelId, DiscordRoleId, DiscordUserId};

    /// Build a `PermOverwrite` with the given target, allow bits, and deny bits.
    fn ow(target: OverwriteTarget, allow: Permissions, deny: Permissions) -> PermOverwrite {
        PermOverwrite {
            target,
            allow,
            deny,
        }
    }

    /// Build a minimal `DiscordChannel` with the given overwrites for testing.
    fn chan(overwrites: Vec<PermOverwrite>) -> DiscordChannel {
        use crate::backends::discord::ChannelKind;
        DiscordChannel {
            id: DiscordChannelId(999),
            name: "test".to_string(),
            kind: ChannelKind::Text,
            parent_id: None,
            position: 0,
            overwrites,
        }
    }

    /// Find the overwrite for a given target, panicking if absent.
    fn find(ows: &[PermOverwrite], target: OverwriteTarget) -> PermOverwrite {
        *ows.iter()
            .find(|o| o.target == target)
            .unwrap_or_else(|| panic!("no overwrite for {target:?}"))
    }

    /// A minimal `SetupConfig` for restrict tests.
    fn cfg() -> SetupConfig {
        SetupConfig {
            everyone: DiscordRoleId(1),
            member_role: DiscordRoleId(10),
            dues_expired_role: DiscordRoleId(11),
            unverified_role: DiscordRoleId(12),
            moderator_role: DiscordRoleId(40),
            bot_user: DiscordUserId(99),
            unverified_channels: BTreeSet::new(),
            dues_expired_channels: BTreeSet::new(),
            exclude_channels: BTreeSet::new(),
        }
    }

    #[test]
    fn set_perm_grant_adds_allow_clears_deny() {
        let start = vec![ow(
            OverwriteTarget::Role(DiscordRoleId(1)),
            Permissions::empty(),
            VIEW,
        )];
        let result = set_perm(&start, OverwriteTarget::Role(DiscordRoleId(1)), VIEW, true);
        let o = find(&result, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(
            o.allow.contains(VIEW),
            "allow must contain VIEW after grant"
        );
        assert!(
            !o.deny.contains(VIEW),
            "deny must not contain VIEW after grant"
        );
    }

    #[test]
    fn set_perm_deny_adds_deny_clears_allow() {
        let start = vec![ow(
            OverwriteTarget::Role(DiscordRoleId(1)),
            VIEW,
            Permissions::empty(),
        )];
        let result = set_perm(&start, OverwriteTarget::Role(DiscordRoleId(1)), VIEW, false);
        let o = find(&result, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(o.deny.contains(VIEW), "deny must contain VIEW after deny");
        assert!(
            !o.allow.contains(VIEW),
            "allow must not contain VIEW after deny"
        );
    }

    #[test]
    fn set_perm_creates_missing_overwrite_with_only_the_bit() {
        let result = set_perm(&[], OverwriteTarget::Role(DiscordRoleId(5)), VIEW, true);
        assert_eq!(result.len(), 1);
        let o = result[0];
        assert_eq!(o.allow, VIEW);
        assert!(o.deny.is_empty());
    }

    #[test]
    fn everyone_view_deny_wins_else_allow_else_base() {
        let everyone = DiscordRoleId(1);

        // deny + allow both set -> deny wins -> false
        let ch = chan(vec![ow(OverwriteTarget::Role(everyone), VIEW, VIEW)]);
        assert!(!everyone_can_view(&ch, everyone, true));

        // allow only -> true regardless of base
        let ch = chan(vec![ow(
            OverwriteTarget::Role(everyone),
            VIEW,
            Permissions::empty(),
        )]);
        assert!(everyone_can_view(&ch, everyone, false));

        // neutral overwrite (neither VIEW allow nor VIEW deny) -> falls through to base
        let ch = chan(vec![ow(
            OverwriteTarget::Role(everyone),
            Permissions::empty(),
            Permissions::empty(),
        )]);
        assert!(everyone_can_view(&ch, everyone, true));
        assert!(!everyone_can_view(&ch, everyone, false));

        // no overwrite at all -> base
        let ch = chan(vec![]);
        assert!(everyone_can_view(&ch, everyone, true));
        assert!(!everyone_can_view(&ch, everyone, false));
    }

    #[test]
    fn lockdown_denies_everyone_allows_member_preserves_others_and_is_idempotent() {
        let current = vec![
            ow(OverwriteTarget::Role(DiscordRoleId(1)), VIEW, SEND), // @everyone: view+send-deny
            ow(
                OverwriteTarget::Role(DiscordRoleId(40)),
                VIEW,
                Permissions::empty(),
            ), // mod allow
        ];
        let locked = lockdown_member(&current, DiscordRoleId(1), DiscordRoleId(10));
        let e = find(&locked, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(!e.allow.contains(VIEW) && e.deny.contains(VIEW));
        assert!(e.deny.contains(SEND), "unrelated deny bit preserved");
        let m = find(&locked, OverwriteTarget::Role(DiscordRoleId(10)));
        assert!(m.allow.contains(VIEW));
        let md = find(&locked, OverwriteTarget::Role(DiscordRoleId(40)));
        assert!(md.allow.contains(VIEW), "mod overwrite untouched");
        assert!(overwrites_equal(
            &locked,
            &lockdown_member(&locked, DiscordRoleId(1), DiscordRoleId(10))
        ));
    }

    #[test]
    fn lockdown_clears_a_preexisting_member_deny() {
        let current = vec![ow(
            OverwriteTarget::Role(DiscordRoleId(10)),
            Permissions::empty(),
            VIEW,
        )];
        let locked = lockdown_member(&current, DiscordRoleId(1), DiscordRoleId(10));
        let m = find(&locked, OverwriteTarget::Role(DiscordRoleId(10)));
        assert!(m.allow.contains(VIEW), "deny must be cleared and allow set");
        assert!(!m.deny.contains(VIEW));
    }

    #[test]
    fn restrict_denies_everyone_view_and_send_allows_role_view_only() {
        let cfg = cfg();
        let allow_role = cfg.unverified_role;
        let result = restrict(&[], &cfg, allow_role);

        let e = find(&result, OverwriteTarget::Role(cfg.everyone));
        assert!(e.deny.contains(VIEW), "@everyone must be denied VIEW");
        assert!(e.deny.contains(SEND), "@everyone must be denied SEND");

        let r = find(&result, OverwriteTarget::Role(allow_role));
        assert!(r.allow.contains(VIEW), "allow_role must have VIEW allowed");
        assert!(
            !r.allow.contains(SEND),
            "allow_role must NOT have SEND allowed"
        );

        let m = find(&result, OverwriteTarget::Role(cfg.moderator_role));
        assert!(m.allow.contains(VIEW), "mod role must have VIEW");
        assert!(m.allow.contains(SEND), "mod role must have SEND");

        let b = find(&result, OverwriteTarget::Member(cfg.bot_user));
        assert!(b.allow.contains(VIEW), "bot must have VIEW");
        assert!(b.allow.contains(SEND), "bot must have SEND");

        // idempotent
        assert!(overwrites_equal(
            &result,
            &restrict(&result, &cfg, allow_role)
        ));
    }

    #[test]
    fn restrict_member_cannot_post_but_role_can_view() {
        let cfg = cfg();
        let allow_role = cfg.unverified_role;
        let result = restrict(&[], &cfg, allow_role);

        // allow_role can view (via explicit allow overwrite)
        assert!(role_can_view(&result, cfg.everyone, false, &[allow_role]));

        // allow_role overwrite has no SEND allow
        let r = find(&result, OverwriteTarget::Role(allow_role));
        assert!(
            !r.allow.contains(SEND),
            "allow_role must not be able to post"
        );
    }

    #[test]
    fn normalize_drops_empty_and_sorts_by_target() {
        let ows = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(20)),
                VIEW,
                Permissions::empty(),
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(5)),
                Permissions::empty(),
                Permissions::empty(),
            ), // empty - must be dropped
            ow(
                OverwriteTarget::Role(DiscordRoleId(10)),
                Permissions::empty(),
                SEND,
            ),
        ];
        let norm = normalize(&ows);
        assert_eq!(norm.len(), 2, "empty overwrite must be dropped");
        assert!(norm[0].target < norm[1].target, "must be sorted by target");
        assert_eq!(norm[0].target, OverwriteTarget::Role(DiscordRoleId(10)));
        assert_eq!(norm[1].target, OverwriteTarget::Role(DiscordRoleId(20)));
    }

    #[test]
    fn overwrites_equal_is_order_independent() {
        let a = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                VIEW,
                Permissions::empty(),
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(2)),
                Permissions::empty(),
                SEND,
            ),
        ];
        let b = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(2)),
                Permissions::empty(),
                SEND,
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                VIEW,
                Permissions::empty(),
            ),
        ];
        assert!(overwrites_equal(&a, &b));

        let c = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                VIEW,
                Permissions::empty(),
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(3)),
                Permissions::empty(),
                SEND,
            ), // different target
        ];
        assert!(!overwrites_equal(&a, &c));
    }
}
