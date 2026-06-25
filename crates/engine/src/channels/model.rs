//! The pure permission-bit math and the [`SetupConfig`] the classifier consumes.
//! Nothing here touches the network. Visibility is the `VIEW_CHANNEL` bit; the
//! two restricted channels additionally move `SEND_MESSAGES`. Every transform
//! copies the channel's overwrites verbatim and flips only the named bit on the
//! named target, so no other permission or role is disturbed.

use std::collections::BTreeSet;

use domain::{DiscordChannelId, DiscordRoleId, DiscordUserId};

use crate::backends::discord::{DiscordChannel, OverwriteTarget, PermOverwrite, Permissions};

/// Everything the classifier needs beyond the channel list itself, built by the
/// bot from `GuildConfig`. All ids are newtypes - no bare primitive crosses in.
#[derive(Debug, Clone)]
pub struct SetupConfig {
    /// The `@everyone` role id (equals the guild id).
    pub everyone: DiscordRoleId,
    pub member_role: DiscordRoleId,
    pub dues_expired_role: DiscordRoleId,
    /// When present, also granted VIEW on the dues-expired channel so a
    /// pre-lapse member (who holds DuesExpiring but is still a Member) can see
    /// their lifecycle thread there.
    pub dues_expiring_role: Option<DiscordRoleId>,
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
/// overwrite decides if it touches `VIEW_CHANNEL` (deny wins over allow), else the base.
pub fn everyone_can_view(ch: &DiscordChannel, everyone: DiscordRoleId, base_view: bool) -> bool {
    let target = OverwriteTarget::Role(everyone);
    for o in &ch.overwrites {
        if o.target == target {
            if o.deny.contains(Permissions::VIEW_CHANNEL) {
                return false;
            }
            if o.allow.contains(Permissions::VIEW_CHANNEL) {
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
        if o.deny.contains(Permissions::VIEW_CHANNEL) {
            allowed = false;
        }
        if o.allow.contains(Permissions::VIEW_CHANNEL) {
            allowed = true;
        }
    }
    let mut role_deny = false;
    let mut role_allow = false;
    for &r in roles {
        if let Some(o) = find(OverwriteTarget::Role(r)) {
            role_deny |= o.deny.contains(Permissions::VIEW_CHANNEL);
            role_allow |= o.allow.contains(Permissions::VIEW_CHANNEL);
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

/// The Member-only array: deny `@everyone` `VIEW_CHANNEL`, allow `Member` `VIEW_CHANNEL`;
/// everything else verbatim. Idempotent.
pub fn lockdown_member(
    current: &[PermOverwrite],
    everyone: DiscordRoleId,
    member: DiscordRoleId,
) -> Vec<PermOverwrite> {
    let out = set_perm(
        current,
        OverwriteTarget::Role(everyone),
        Permissions::VIEW_CHANNEL,
        false,
    );
    set_perm(
        &out,
        OverwriteTarget::Role(member),
        Permissions::VIEW_CHANNEL,
        true,
    )
}

/// The restricted array for an unverified/dues-expired channel: deny `@everyone`
/// {`VIEW_CHANNEL`, `SEND_MESSAGES`}; allow `allow_role` `VIEW_CHANNEL` (read +
/// click buttons, no posting); allow the moderator role {`VIEW_CHANNEL`,
/// `SEND_MESSAGES`}; allow the bot {`VIEW_CHANNEL`, `SEND_MESSAGES`}. Idempotent.
pub fn restrict(
    current: &[PermOverwrite],
    cfg: &SetupConfig,
    allow_role: DiscordRoleId,
) -> Vec<PermOverwrite> {
    let e = OverwriteTarget::Role(cfg.everyone);
    let mut out = set_perm(current, e, Permissions::VIEW_CHANNEL, false);
    out = set_perm(&out, e, Permissions::SEND_MESSAGES, false);
    out = set_perm(
        &out,
        OverwriteTarget::Role(allow_role),
        Permissions::VIEW_CHANNEL,
        true,
    );
    out = set_perm(
        &out,
        OverwriteTarget::Role(cfg.moderator_role),
        Permissions::VIEW_CHANNEL,
        true,
    );
    out = set_perm(
        &out,
        OverwriteTarget::Role(cfg.moderator_role),
        Permissions::SEND_MESSAGES,
        true,
    );
    out = set_perm(
        &out,
        OverwriteTarget::Member(cfg.bot_user),
        Permissions::VIEW_CHANNEL,
        true,
    );
    out = set_perm(
        &out,
        OverwriteTarget::Member(cfg.bot_user),
        Permissions::SEND_MESSAGES,
        true,
    );
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
            dues_expiring_role: None,
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
            Permissions::VIEW_CHANNEL,
        )];
        let result = set_perm(
            &start,
            OverwriteTarget::Role(DiscordRoleId(1)),
            Permissions::VIEW_CHANNEL,
            true,
        );
        let o = find(&result, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(
            o.allow.contains(Permissions::VIEW_CHANNEL),
            "allow must contain VIEW_CHANNEL after grant"
        );
        assert!(
            !o.deny.contains(Permissions::VIEW_CHANNEL),
            "deny must not contain VIEW_CHANNEL after grant"
        );
    }

    #[test]
    fn set_perm_deny_adds_deny_clears_allow() {
        let start = vec![ow(
            OverwriteTarget::Role(DiscordRoleId(1)),
            Permissions::VIEW_CHANNEL,
            Permissions::empty(),
        )];
        let result = set_perm(
            &start,
            OverwriteTarget::Role(DiscordRoleId(1)),
            Permissions::VIEW_CHANNEL,
            false,
        );
        let o = find(&result, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(
            o.deny.contains(Permissions::VIEW_CHANNEL),
            "deny must contain VIEW_CHANNEL after deny"
        );
        assert!(
            !o.allow.contains(Permissions::VIEW_CHANNEL),
            "allow must not contain VIEW_CHANNEL after deny"
        );
    }

    #[test]
    fn set_perm_creates_missing_overwrite_with_only_the_bit() {
        let result = set_perm(
            &[],
            OverwriteTarget::Role(DiscordRoleId(5)),
            Permissions::VIEW_CHANNEL,
            true,
        );
        assert_eq!(result.len(), 1);
        let o = result[0];
        assert_eq!(o.allow, Permissions::VIEW_CHANNEL);
        assert!(o.deny.is_empty());
    }

    #[test]
    fn everyone_view_deny_wins_else_allow_else_base() {
        let everyone = DiscordRoleId(1);

        // deny + allow both set -> deny wins -> false
        let ch = chan(vec![ow(
            OverwriteTarget::Role(everyone),
            Permissions::VIEW_CHANNEL,
            Permissions::VIEW_CHANNEL,
        )]);
        assert!(!everyone_can_view(&ch, everyone, true));

        // allow only -> true regardless of base
        let ch = chan(vec![ow(
            OverwriteTarget::Role(everyone),
            Permissions::VIEW_CHANNEL,
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
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                Permissions::VIEW_CHANNEL,
                Permissions::SEND_MESSAGES,
            ), // @everyone: view+send-deny
            ow(
                OverwriteTarget::Role(DiscordRoleId(40)),
                Permissions::VIEW_CHANNEL,
                Permissions::empty(),
            ), // mod allow
        ];
        let locked = lockdown_member(&current, DiscordRoleId(1), DiscordRoleId(10));
        let e = find(&locked, OverwriteTarget::Role(DiscordRoleId(1)));
        assert!(
            !e.allow.contains(Permissions::VIEW_CHANNEL)
                && e.deny.contains(Permissions::VIEW_CHANNEL)
        );
        assert!(
            e.deny.contains(Permissions::SEND_MESSAGES),
            "unrelated deny bit preserved"
        );
        let m = find(&locked, OverwriteTarget::Role(DiscordRoleId(10)));
        assert!(m.allow.contains(Permissions::VIEW_CHANNEL));
        let md = find(&locked, OverwriteTarget::Role(DiscordRoleId(40)));
        assert!(
            md.allow.contains(Permissions::VIEW_CHANNEL),
            "mod overwrite untouched"
        );
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
            Permissions::VIEW_CHANNEL,
        )];
        let locked = lockdown_member(&current, DiscordRoleId(1), DiscordRoleId(10));
        let m = find(&locked, OverwriteTarget::Role(DiscordRoleId(10)));
        assert!(
            m.allow.contains(Permissions::VIEW_CHANNEL),
            "deny must be cleared and allow set"
        );
        assert!(!m.deny.contains(Permissions::VIEW_CHANNEL));
    }

    #[test]
    fn restrict_denies_everyone_view_and_send_allows_role_view_only() {
        let cfg = cfg();
        let allow_role = cfg.unverified_role;
        let result = restrict(&[], &cfg, allow_role);

        let e = find(&result, OverwriteTarget::Role(cfg.everyone));
        assert!(
            e.deny.contains(Permissions::VIEW_CHANNEL),
            "@everyone must be denied VIEW_CHANNEL"
        );
        assert!(
            e.deny.contains(Permissions::SEND_MESSAGES),
            "@everyone must be denied SEND_MESSAGES"
        );

        let r = find(&result, OverwriteTarget::Role(allow_role));
        assert!(
            r.allow.contains(Permissions::VIEW_CHANNEL),
            "allow_role must have VIEW_CHANNEL allowed"
        );
        assert!(
            !r.allow.contains(Permissions::SEND_MESSAGES),
            "allow_role must NOT have SEND_MESSAGES allowed"
        );

        let m = find(&result, OverwriteTarget::Role(cfg.moderator_role));
        assert!(
            m.allow.contains(Permissions::VIEW_CHANNEL),
            "mod role must have VIEW_CHANNEL"
        );
        assert!(
            m.allow.contains(Permissions::SEND_MESSAGES),
            "mod role must have SEND_MESSAGES"
        );

        let b = find(&result, OverwriteTarget::Member(cfg.bot_user));
        assert!(
            b.allow.contains(Permissions::VIEW_CHANNEL),
            "bot must have VIEW_CHANNEL"
        );
        assert!(
            b.allow.contains(Permissions::SEND_MESSAGES),
            "bot must have SEND_MESSAGES"
        );

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

        // allow_role overwrite has no SEND_MESSAGES allow
        let r = find(&result, OverwriteTarget::Role(allow_role));
        assert!(
            !r.allow.contains(Permissions::SEND_MESSAGES),
            "allow_role must not be able to post"
        );
    }

    #[test]
    fn normalize_drops_empty_and_sorts_by_target() {
        let ows = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(20)),
                Permissions::VIEW_CHANNEL,
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
                Permissions::SEND_MESSAGES,
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
                Permissions::VIEW_CHANNEL,
                Permissions::empty(),
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(2)),
                Permissions::empty(),
                Permissions::SEND_MESSAGES,
            ),
        ];
        let b = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(2)),
                Permissions::empty(),
                Permissions::SEND_MESSAGES,
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                Permissions::VIEW_CHANNEL,
                Permissions::empty(),
            ),
        ];
        assert!(overwrites_equal(&a, &b));

        let c = vec![
            ow(
                OverwriteTarget::Role(DiscordRoleId(1)),
                Permissions::VIEW_CHANNEL,
                Permissions::empty(),
            ),
            ow(
                OverwriteTarget::Role(DiscordRoleId(3)),
                Permissions::empty(),
                Permissions::SEND_MESSAGES,
            ), // different target
        ];
        assert!(!overwrites_equal(&a, &c));
    }
}
