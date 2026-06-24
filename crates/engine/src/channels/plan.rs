//! The classifier: turn the live channel list plus a [`SetupConfig`] into a frozen
//! [`ChannelPlan`]. Two passes - lock every `@everyone`-visible category to
//! `Member`, then classify every channel under the strict partition
//! `unverified > dues-expired > exclude > sweep`. A synced child follows its
//! locked parent; a desynced-but-public orphan changes directly; a private channel
//! is left `Unchanged`. Pure: a function of its inputs, fully unit-testable.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use domain::{DiscordChannelId, DiscordGuildId};

use crate::backends::discord::{ChannelKind, DiscordChannel, PermOverwrite};

use super::model::{
    SetupConfig, everyone_can_view, lockdown_member, overwrites_equal, overwrites_synced, restrict,
    role_can_view,
};

/// What the plan will do to one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelAction {
    MemberOnly,
    UnverifiedOnly,
    ExpiredOnly,
    SyncedToParent,
    Excluded,
    Unchanged,
}

impl ChannelAction {
    fn is_changing(self) -> bool {
        matches!(
            self,
            ChannelAction::MemberOnly
                | ChannelAction::UnverifiedOnly
                | ChannelAction::ExpiredOnly
                | ChannelAction::SyncedToParent
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            ChannelAction::MemberOnly => "member-only",
            ChannelAction::UnverifiedOnly => "unverified-only",
            ChannelAction::ExpiredOnly => "dues-expired-only",
            ChannelAction::SyncedToParent => "synced-to-parent",
            ChannelAction::Excluded => "excluded",
            ChannelAction::Unchanged => "unchanged",
        }
    }
}

/// One channel's frozen decision.
#[derive(Debug, Clone)]
pub struct PlannedChannel {
    pub id: DiscordChannelId,
    pub name: String,
    pub kind: ChannelKind,
    pub parent_id: Option<DiscordChannelId>,
    pub position: u16,
    pub action: ChannelAction,
    pub everyone_view_before: bool,
    pub current_overwrites: Vec<PermOverwrite>,
    pub final_overwrites: Vec<PermOverwrite>,
    pub allow_roles: Vec<domain::DiscordRoleId>,
    pub writes: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanCounts {
    pub total: usize,
    pub member_only: usize,
    pub unverified_only: usize,
    pub expired_only: usize,
    pub synced_children: usize,
    pub excluded: usize,
    pub unchanged: usize,
    pub no_op: usize,
    pub writes: usize,
}

#[derive(Debug, Clone)]
pub struct ChannelPlan {
    pub guild_id: DiscordGuildId,
    pub channels: Vec<PlannedChannel>,
    pub counts: PlanCounts,
    pub resolved_at: DateTime<Utc>,
}

impl ChannelPlan {
    pub fn writes(&self) -> impl Iterator<Item = &PlannedChannel> {
        self.channels.iter().filter(|c| c.writes)
    }
}

/// Resolve the channel list into a frozen plan. See the module docs for the two
/// passes and the partition.
pub fn resolve_plan(
    channels: &[DiscordChannel],
    cfg: &SetupConfig,
    everyone_base_view: bool,
    resolved_at: DateTime<Utc>,
) -> ChannelPlan {
    let classifier = Classifier::new(channels, cfg, everyone_base_view);

    let mut counts = PlanCounts::default();
    let mut planned = Vec::with_capacity(channels.len());
    for c in channels {
        let everyone_view_before = everyone_can_view(c, cfg.everyone, everyone_base_view);
        let Classified {
            action,
            desired,
            allow_roles,
        } = classifier.classify(c);
        let current = c.overwrites.clone();
        let final_overwrites = desired.unwrap_or_else(|| current.clone());
        let no_op = action.is_changing() && overwrites_equal(&final_overwrites, &current);
        let writes = action.is_changing() && !no_op;

        counts.total += 1;
        match action {
            ChannelAction::MemberOnly => counts.member_only += 1,
            ChannelAction::UnverifiedOnly => counts.unverified_only += 1,
            ChannelAction::ExpiredOnly => counts.expired_only += 1,
            ChannelAction::SyncedToParent => counts.synced_children += 1,
            ChannelAction::Excluded => counts.excluded += 1,
            ChannelAction::Unchanged => counts.unchanged += 1,
        }
        if no_op {
            counts.no_op += 1;
        }
        if writes {
            counts.writes += 1;
        }

        planned.push(PlannedChannel {
            id: c.id,
            name: c.name.clone(),
            kind: c.kind,
            parent_id: c.parent_id,
            position: c.position,
            action,
            everyone_view_before,
            current_overwrites: current,
            final_overwrites,
            allow_roles,
            writes,
        });
    }

    ChannelPlan {
        guild_id: cfg_guild_id(cfg),
        channels: planned,
        counts,
        resolved_at,
    }
}

fn cfg_guild_id(cfg: &SetupConfig) -> DiscordGuildId {
    DiscordGuildId(cfg.everyone.0) // @everyone role id == guild id
}

/// One channel's classification: the action, the desired overwrite array (`None`
/// when nothing changes), and the roles it grants view (for the report). A named
/// bag, not a tuple, so call sites read by field.
struct Classified {
    action: ChannelAction,
    desired: Option<Vec<PermOverwrite>>,
    allow_roles: Vec<domain::DiscordRoleId>,
}

fn is_explicit(id: DiscordChannelId, cfg: &SetupConfig) -> bool {
    cfg.unverified_channels.contains(&id)
        || cfg.dues_expired_channels.contains(&id)
        || cfg.exclude_channels.contains(&id)
}

/// The shared context one resolve pass classifies against: the config, the id
/// index, the base-view flag, and the categories pass 1 decided to lock. Built
/// once per [`resolve_plan`] so [`classify`](Classifier::classify) reads as a
/// single method over its inputs rather than a free function threading five
/// arguments - the same handle-over-its-data shape as [`verify::Member`].
///
/// [`verify::Member`]: crate::verify::Member
struct Classifier<'a> {
    cfg: &'a SetupConfig,
    base_view: bool,
    by_id: HashMap<DiscordChannelId, &'a DiscordChannel>,
    category_locked: HashMap<DiscordChannelId, Vec<PermOverwrite>>,
}

impl<'a> Classifier<'a> {
    /// Build the context and run pass 1: lock every `@everyone`-visible,
    /// non-explicit category to `Member` and record its new array.
    fn new(channels: &'a [DiscordChannel], cfg: &'a SetupConfig, base_view: bool) -> Self {
        let by_id: HashMap<_, _> = channels.iter().map(|c| (c.id, c)).collect();
        let mut category_locked = HashMap::new();
        for c in channels
            .iter()
            .filter(|c| c.kind.is_category() && !is_explicit(c.id, cfg))
        {
            if everyone_can_view(c, cfg.everyone, base_view) {
                category_locked.insert(
                    c.id,
                    lockdown_member(&c.overwrites, cfg.everyone, cfg.member_role),
                );
            }
        }
        Self {
            cfg,
            base_view,
            by_id,
            category_locked,
        }
    }

    /// Classify one channel under the strict partition
    /// `unverified > dues-expired > exclude > sweep`. See the module docs.
    fn classify(&self, c: &DiscordChannel) -> Classified {
        let cfg = self.cfg;
        let member = |ch: &DiscordChannel| Classified {
            action: ChannelAction::MemberOnly,
            desired: Some(lockdown_member(
                &ch.overwrites,
                cfg.everyone,
                cfg.member_role,
            )),
            allow_roles: vec![cfg.member_role],
        };
        let unchanged = || Classified {
            action: ChannelAction::Unchanged,
            desired: None,
            allow_roles: vec![],
        };

        // Explicit buckets win, in priority order. NOT gated on @everyone
        // visibility - granting their role view is a real access change (the no-op
        // check still drops a write that resolves to the current state).
        if cfg.unverified_channels.contains(&c.id) {
            return Classified {
                action: ChannelAction::UnverifiedOnly,
                desired: Some(restrict(&c.overwrites, cfg, cfg.unverified_role)),
                allow_roles: vec![cfg.unverified_role],
            };
        }
        if cfg.dues_expired_channels.contains(&c.id) {
            return Classified {
                action: ChannelAction::ExpiredOnly,
                desired: Some(restrict(&c.overwrites, cfg, cfg.dues_expired_role)),
                allow_roles: vec![cfg.dues_expired_role],
            };
        }
        if cfg.exclude_channels.contains(&c.id) {
            return Classified {
                action: ChannelAction::Excluded,
                desired: None,
                allow_roles: vec![],
            };
        }

        if c.kind.is_category() {
            return match self.category_locked.get(&c.id) {
                Some(locked) => Classified {
                    action: ChannelAction::MemberOnly,
                    desired: Some(locked.clone()),
                    allow_roles: vec![cfg.member_role],
                },
                None => unchanged(),
            };
        }

        match c.parent_id {
            Some(parent) => {
                let synced = self
                    .by_id
                    .get(&parent)
                    .is_some_and(|p| overwrites_synced(&c.overwrites, &p.overwrites));
                if synced {
                    match self.category_locked.get(&parent) {
                        Some(locked) => Classified {
                            action: ChannelAction::SyncedToParent,
                            desired: Some(locked.clone()),
                            allow_roles: vec![cfg.member_role],
                        },
                        None => unchanged(),
                    }
                } else if everyone_can_view(c, cfg.everyone, self.base_view) {
                    member(c)
                } else {
                    unchanged()
                }
            }
            None if everyone_can_view(c, cfg.everyone, self.base_view) => member(c),
            None => unchanged(),
        }
    }
}

/// Any nominated unverified channel the plan would leave NOT viewable by the
/// `Unverified` role - the verification breach. A non-empty result must abort the
/// run before any write.
pub fn verification_breaches(plan: &ChannelPlan, cfg: &SetupConfig) -> Vec<DiscordChannelId> {
    plan.channels
        .iter()
        .filter(|p| cfg.unverified_channels.contains(&p.id))
        .filter(|p| {
            !role_can_view(
                &p.final_overwrites,
                cfg.everyone,
                true,
                &[cfg.unverified_role],
            )
        })
        .map(|p| p.id)
        .collect()
}

/// A read-only report of children whose overwrites differ from their category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesyncReport {
    /// `(child id, child name, parent category id)` for each out-of-sync child.
    pub out_of_sync: Vec<(DiscordChannelId, String, DiscordChannelId)>,
}

/// Compute the desync report - the `/channels check` subcommand's whole job.
pub fn desync_report(channels: &[DiscordChannel]) -> DesyncReport {
    let by_id: HashMap<DiscordChannelId, &DiscordChannel> =
        channels.iter().map(|c| (c.id, c)).collect();
    let mut out_of_sync = Vec::new();
    for c in channels {
        if let Some(parent) = c.parent_id
            && let Some(p) = by_id.get(&parent)
            && !overwrites_synced(&c.overwrites, &p.overwrites)
        {
            out_of_sync.push((c.id, c.name.clone(), parent));
        }
    }
    DesyncReport { out_of_sync }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use domain::{DiscordChannelId, DiscordRoleId, DiscordUserId};

    use crate::backends::discord::{
        ChannelKind, DiscordChannel, OverwriteTarget, PermOverwrite, Permissions,
    };

    use super::super::model::{VIEW, lockdown_member, restrict};
    use super::*;

    /// Build a `PermOverwrite` with the given target, allow bits, and deny bits.
    fn ow(target: OverwriteTarget, allow: Permissions, deny: Permissions) -> PermOverwrite {
        PermOverwrite {
            target,
            allow,
            deny,
        }
    }

    /// Build a `DiscordChannel` for testing.
    fn chan(
        id: u64,
        kind: ChannelKind,
        parent: Option<u64>,
        overwrites: Vec<PermOverwrite>,
    ) -> DiscordChannel {
        DiscordChannel {
            id: DiscordChannelId(id),
            name: format!("ch-{id}"),
            kind,
            parent_id: parent.map(DiscordChannelId),
            position: 0,
            overwrites,
        }
    }

    /// A minimal `SetupConfig` for plan tests.
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

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// Helper: look up the planned channel for the given id.
    fn find_planned(plan: &ChannelPlan, id: u64) -> &PlannedChannel {
        plan.channels
            .iter()
            .find(|p| p.id == DiscordChannelId(id))
            .unwrap_or_else(|| panic!("no planned channel for id {id}"))
    }

    // --- private channel is left unchanged ---

    #[test]
    fn private_channel_is_left_unchanged() {
        let cfg = cfg();
        // @everyone VIEW denied -> private
        let private = chan(
            100,
            ChannelKind::Text,
            None,
            vec![ow(
                OverwriteTarget::Role(cfg.everyone),
                Permissions::empty(),
                VIEW,
            )],
        );
        let plan = resolve_plan(&[private], &cfg, true, now());
        let p = find_planned(&plan, 100);
        assert_eq!(
            p.action,
            ChannelAction::Unchanged,
            "private channel must be Unchanged"
        );
        assert!(!p.writes, "private channel must not write");
    }

    // --- synced child follows locked category and stays synced ---

    #[test]
    fn synced_child_follows_locked_category_and_stays_synced() {
        let cfg = cfg();
        // Public category (no overwrites, base_view=true)
        let cat = chan(200, ChannelKind::Category, None, vec![]);
        // Child with same overwrites as category (synced)
        let child = chan(201, ChannelKind::Text, Some(200), vec![]);
        let plan = resolve_plan(&[cat, child], &cfg, true, now());

        let cat_p = find_planned(&plan, 200);
        let child_p = find_planned(&plan, 201);

        assert_eq!(
            cat_p.action,
            ChannelAction::MemberOnly,
            "category must be MemberOnly"
        );
        assert_eq!(
            child_p.action,
            ChannelAction::SyncedToParent,
            "synced child must be SyncedToParent"
        );
        // The final overwrites of category and child must be equal (child follows parent)
        assert!(
            overwrites_equal(&cat_p.final_overwrites, &child_p.final_overwrites),
            "synced child final overwrites must equal category final overwrites"
        );
    }

    // --- desynced public orphan changes directly; desynced+private is unchanged ---

    #[test]
    fn desynced_public_orphan_changes_directly() {
        let cfg = cfg();
        // Public category
        let cat = chan(300, ChannelKind::Category, None, vec![]);
        // Desynced public child (has an extra overwrite the parent doesn't have)
        let desynced_public = chan(
            301,
            ChannelKind::Text,
            Some(300),
            vec![ow(
                OverwriteTarget::Role(DiscordRoleId(99)),
                VIEW,
                Permissions::empty(),
            )],
        );
        // Desynced private child (@everyone VIEW denied)
        let desynced_private = chan(
            302,
            ChannelKind::Text,
            Some(300),
            vec![ow(
                OverwriteTarget::Role(cfg.everyone),
                Permissions::empty(),
                VIEW,
            )],
        );
        let plan = resolve_plan(&[cat, desynced_public, desynced_private], &cfg, true, now());

        let pub_p = find_planned(&plan, 301);
        let priv_p = find_planned(&plan, 302);

        assert_eq!(
            pub_p.action,
            ChannelAction::MemberOnly,
            "desynced public child must be MemberOnly"
        );
        assert_eq!(
            priv_p.action,
            ChannelAction::Unchanged,
            "desynced private child must be Unchanged"
        );
    }

    // --- nominated unverified channel that is private still gets its role (special-channel exception) ---

    #[test]
    fn nominated_unverified_channel_that_is_private_still_gets_its_role() {
        let mut cfg = cfg();
        cfg.unverified_channels.insert(DiscordChannelId(400));

        // Private channel: @everyone VIEW denied, NO Unverified allow
        let private_unverified = chan(
            400,
            ChannelKind::Text,
            None,
            vec![ow(
                OverwriteTarget::Role(cfg.everyone),
                Permissions::empty(),
                VIEW,
            )],
        );
        let plan = resolve_plan(&[private_unverified], &cfg, true, now());
        let p = find_planned(&plan, 400);

        assert_eq!(
            p.action,
            ChannelAction::UnverifiedOnly,
            "nominated unverified must be UnverifiedOnly regardless of current visibility"
        );
        assert!(
            p.writes,
            "nominated private unverified channel must generate a write"
        );
        assert!(
            role_can_view(
                &p.final_overwrites,
                cfg.everyone,
                true,
                &[cfg.unverified_role]
            ),
            "Unverified role must be able to view after the plan (special-channel exception)"
        );
    }

    // --- mod channel in exclude is untouched ---

    #[test]
    fn mod_channel_in_exclude_is_untouched() {
        let mut cfg = cfg();
        cfg.exclude_channels.insert(DiscordChannelId(500));

        // Public excluded channel
        let mod_chan = chan(500, ChannelKind::Text, None, vec![]);
        let plan = resolve_plan(&[mod_chan], &cfg, true, now());
        let p = find_planned(&plan, 500);

        assert_eq!(
            p.action,
            ChannelAction::Excluded,
            "excluded channel must be Excluded"
        );
        assert!(!p.writes, "excluded channel must not generate a write");
    }

    // --- verification_breaches empty for a resolved plan ---

    #[test]
    fn verification_breaches_empty_for_a_resolved_plan() {
        let mut cfg = cfg();
        cfg.unverified_channels.insert(DiscordChannelId(600));

        // A public channel and a nominated unverified channel
        let public_ch = chan(601, ChannelKind::Text, None, vec![]);
        let unverified_ch = chan(600, ChannelKind::Text, None, vec![]);
        let plan = resolve_plan(&[public_ch, unverified_ch], &cfg, true, now());

        let breaches = verification_breaches(&plan, &cfg);
        assert!(
            breaches.is_empty(),
            "a properly resolved plan must have no verification breaches, got: {breaches:?}"
        );
    }

    // --- verification_breaches flags a hand-built member-only unverified channel ---

    #[test]
    fn verification_breaches_flags_a_hand_built_member_only_unverified_channel() {
        let mut cfg = cfg();
        cfg.unverified_channels.insert(DiscordChannelId(700));

        // Wrongly built: the unverified channel is MemberOnly (no Unverified role allow)
        let member_only_ows = lockdown_member(&[], cfg.everyone, cfg.member_role);
        let wrong_channel = PlannedChannel {
            id: DiscordChannelId(700),
            name: "unverified-but-wrong".into(),
            kind: ChannelKind::Text,
            parent_id: None,
            position: 0,
            action: ChannelAction::MemberOnly,
            everyone_view_before: true,
            current_overwrites: vec![],
            final_overwrites: member_only_ows,
            allow_roles: vec![cfg.member_role],
            writes: true,
        };
        let plan = ChannelPlan {
            guild_id: DiscordGuildId(1),
            channels: vec![wrong_channel],
            counts: PlanCounts::default(),
            resolved_at: now(),
        };

        let breaches = verification_breaches(&plan, &cfg);
        assert_eq!(
            breaches,
            vec![DiscordChannelId(700)],
            "guard must flag the wrongly-classified unverified channel"
        );
    }

    // --- idempotent rerun is unchanged ---

    #[test]
    fn idempotent_rerun_is_unchanged() {
        let cfg = cfg();
        // A public channel
        let public_ch = chan(800, ChannelKind::Text, None, vec![]);
        let plan1 = resolve_plan(&[public_ch], &cfg, true, now());
        let p1 = find_planned(&plan1, 800);

        // Feed the final overwrites back as a fresh channel
        let after_first_run = chan(800, ChannelKind::Text, None, p1.final_overwrites.clone());
        let plan2 = resolve_plan(&[after_first_run], &cfg, true, now());
        let p2 = find_planned(&plan2, 800);

        assert!(
            !p2.writes,
            "second run on already-locked channel must be a no-op (no write)"
        );
    }

    // --- desync_report flags only out-of-sync children ---

    #[test]
    fn desync_report_flags_only_out_of_sync_children() {
        let cfg = cfg();
        // A category with an @everyone deny
        let cat_ows = vec![ow(
            OverwriteTarget::Role(cfg.everyone),
            Permissions::empty(),
            VIEW,
        )];
        let cat = chan(900, ChannelKind::Category, None, cat_ows.clone());
        // Synced child: same overwrites as parent
        let synced = chan(901, ChannelKind::Text, Some(900), cat_ows.clone());
        // Desynced child: different overwrites
        let desynced = chan(
            902,
            ChannelKind::Text,
            Some(900),
            vec![ow(
                OverwriteTarget::Role(DiscordRoleId(10)),
                VIEW,
                Permissions::empty(),
            )],
        );

        let report = desync_report(&[cat, synced, desynced]);

        assert_eq!(
            report.out_of_sync.len(),
            1,
            "only the desynced child must be reported"
        );
        assert_eq!(report.out_of_sync[0].0, DiscordChannelId(902));
        assert_eq!(report.out_of_sync[0].2, DiscordChannelId(900));
    }

    // Additional: verify the restrict path produces correct final_overwrites for
    // dues_expired channel so desync_report tests are paired.
    #[test]
    fn dues_expired_channel_gets_correct_restrict_overwrites() {
        let mut cfg = cfg();
        cfg.dues_expired_channels.insert(DiscordChannelId(1000));

        let ch = chan(1000, ChannelKind::Text, None, vec![]);
        let plan = resolve_plan(&[ch], &cfg, true, now());
        let p = find_planned(&plan, 1000);

        assert_eq!(p.action, ChannelAction::ExpiredOnly);
        assert!(p.writes);
        // @everyone must be denied VIEW
        let expected = restrict(&[], &cfg, cfg.dues_expired_role);
        assert!(
            overwrites_equal(&p.final_overwrites, &expected),
            "dues-expired channel final overwrites must match restrict output"
        );
    }
}
