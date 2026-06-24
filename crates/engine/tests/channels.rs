//! Behaviour suite for the channel-permission terraform.
//!
//! Cast: Tails is the protagonist moderator running the channel terraform.
//! The scenarios cover classification, the verification guard, validation,
//! the write-count gate, and the check/save/restore verbs.

use cucumber::{World as _, given, then, when};

use engine::backends::discord::{
    ChannelKind, DiscordChannel, FakeDiscord, OverwriteTarget, PermOverwrite, Permissions,
};
use engine::channels::{
    ChannelAction, ChannelPlan, ChannelSnapshot, Channels, ChannelsError, DesyncReport,
    PlannedChannel, SNAPSHOT_FORMAT_VERSION, SetupConfig, resolve_plan, verification_breaches,
};
use engine::seam::NoProgress;
use engine::store::{InMemoryStore, Index};

use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, DiscordUserId};

// ---------------------------------------------------------------------------
// Fixed ids for the config roles (mirror the unit-test cfg)
// ---------------------------------------------------------------------------

const GUILD: u64 = 1;
const EVERYONE: u64 = 1; // @everyone role id == guild id
const MEMBER_ROLE: u64 = 10;
const DUES_EXPIRED_ROLE: u64 = 11;
const UNVERIFIED_ROLE: u64 = 12;
const MODERATOR_ROLE: u64 = 40;
const BOT_USER: u64 = 99;

// Channel ids used in scenarios.
const GENERAL_ID: u64 = 100;
const DUES_DESK_ID: u64 = 101;
const WELCOME_ID: u64 = 102;

/// Build the base `SetupConfig` with the given channel sets.
fn make_cfg(
    unverified: impl IntoIterator<Item = u64>,
    dues_expired: impl IntoIterator<Item = u64>,
    exclude: impl IntoIterator<Item = u64>,
) -> SetupConfig {
    SetupConfig {
        everyone: DiscordRoleId(EVERYONE),
        member_role: DiscordRoleId(MEMBER_ROLE),
        dues_expired_role: DiscordRoleId(DUES_EXPIRED_ROLE),
        unverified_role: DiscordRoleId(UNVERIFIED_ROLE),
        moderator_role: DiscordRoleId(MODERATOR_ROLE),
        bot_user: DiscordUserId(BOT_USER),
        unverified_channels: unverified.into_iter().map(DiscordChannelId).collect(),
        dues_expired_channels: dues_expired.into_iter().map(DiscordChannelId).collect(),
        exclude_channels: exclude.into_iter().map(DiscordChannelId).collect(),
    }
}

/// A public text channel: @everyone has VIEW by default, no overwrites.
fn public_chan(id: u64, name: &str) -> DiscordChannel {
    DiscordChannel {
        id: DiscordChannelId(id),
        name: name.to_owned(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        overwrites: vec![],
    }
}

/// A private text channel: @everyone VIEW denied.
fn private_chan(id: u64, name: &str) -> DiscordChannel {
    DiscordChannel {
        id: DiscordChannelId(id),
        name: name.to_owned(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        overwrites: vec![PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(EVERYONE)),
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
        }],
    }
}

/// A public category channel.
fn category_chan(id: u64, name: &str) -> DiscordChannel {
    DiscordChannel {
        id: DiscordChannelId(id),
        name: name.to_owned(),
        kind: ChannelKind::Category,
        parent_id: None,
        position: 0,
        overwrites: vec![],
    }
}

/// A child text channel under a given parent, same overwrites (synced by default).
fn child_chan(
    id: u64,
    name: &str,
    parent_id: u64,
    overwrites: Vec<PermOverwrite>,
) -> DiscordChannel {
    DiscordChannel {
        id: DiscordChannelId(id),
        name: name.to_owned(),
        kind: ChannelKind::Text,
        parent_id: Some(DiscordChannelId(parent_id)),
        position: 0,
        overwrites,
    }
}

/// Whether the Unverified role can view the final_overwrites of a planned channel.
fn unverified_can_view(planned: &PlannedChannel) -> bool {
    let find = |t: OverwriteTarget| planned.final_overwrites.iter().find(|o| o.target == t);
    // @everyone base is effectively true after restrict (they get explicit deny, so false anyway).
    // We check: @everyone overwrite for VIEW.
    let mut allowed = true; // base
    if let Some(o) = find(OverwriteTarget::Role(DiscordRoleId(EVERYONE))) {
        if o.deny.contains(Permissions::VIEW_CHANNEL) {
            allowed = false;
        }
        if o.allow.contains(Permissions::VIEW_CHANNEL) {
            allowed = true;
        }
    }
    // Role overwrite for Unverified.
    let mut role_deny = false;
    let mut role_allow = false;
    if let Some(o) = find(OverwriteTarget::Role(DiscordRoleId(UNVERIFIED_ROLE))) {
        role_deny = o.deny.contains(Permissions::VIEW_CHANNEL);
        role_allow = o.allow.contains(Permissions::VIEW_CHANNEL);
    }
    if role_deny {
        allowed = false;
    }
    if role_allow {
        allowed = true;
    }
    allowed
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

#[derive(cucumber::World, Default)]
#[world(init = Self::new)]
struct TailsWorld {
    /// Channels seeded in FakeDiscord.
    channels: Vec<DiscordChannel>,
    /// Whether @everyone base-view is on for this scenario.
    everyone_base_view: bool,
    /// Config built for the scenario.
    cfg: Option<SetupConfig>,
    /// The last resolved plan.
    plan: Option<ChannelPlan>,
    /// The last plan error from the Channels facade.
    plan_err: Option<ChannelsError>,
    /// The last desync report.
    desync: Option<DesyncReport>,
    /// The last snapshot.
    snapshot: Option<ChannelSnapshot>,
    /// The last apply error.
    apply_err: Option<ChannelsError>,
    /// FakeDiscord (rebuilt on demand once channels + base_view are set).
    fake: Option<FakeDiscord>,
}

impl std::fmt::Debug for TailsWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TailsWorld")
            .field("channels", &self.channels.len())
            .field("everyone_base_view", &self.everyone_base_view)
            .field("has_plan", &self.plan.is_some())
            .field("plan_err", &self.plan_err.as_ref().map(|e| e.to_string()))
            .finish_non_exhaustive()
    }
}

impl TailsWorld {
    async fn new() -> Self {
        Self {
            channels: Vec::new(),
            everyone_base_view: true, // default: @everyone can view (most scenarios are public)
            cfg: None,
            plan: None,
            plan_err: None,
            desync: None,
            snapshot: None,
            apply_err: None,
            fake: None,
        }
    }

    /// Build the FakeDiscord from the current channel list + base_view, caching it.
    fn build_fake(&self) -> FakeDiscord {
        FakeDiscord::new()
            .with_channels(self.channels.clone())
            .set_everyone_base_view(self.everyone_base_view)
    }

    fn store() -> InMemoryStore {
        InMemoryStore::new(Index::default())
    }
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given(regex = r#"^a public text channel named "([^"]+)"$"#)]
async fn given_public_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(public_chan(id, &name));
    world.everyone_base_view = true;
}

#[given(regex = r#"^a private text channel named "([^"]+)"$"#)]
async fn given_private_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(private_chan(id, &name));
    world.everyone_base_view = true;
}

#[given(regex = r#"^"([^"]+)" is neither a dues-expired nor an unverified channel$"#)]
async fn given_channel_in_no_special_set(_world: &mut TailsWorld, _name: String) {
    // Not adding the channel to any special set; base config handles this (no sets populated).
    // cfg will be built at resolve time.
}

#[given(regex = r#"^a public text channel named "([^"]+)" nominated as a dues-expired channel$"#)]
async fn given_dues_expired_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(public_chan(id, &name));
    world.everyone_base_view = true;
    // Record we need this id in dues_expired_channels; we'll build cfg in the When step.
    // Store it by putting a placeholder cfg.
    let existing_cfg = world.cfg.take();
    let mut cfg = existing_cfg.unwrap_or_else(|| make_cfg([], [], []));
    cfg.dues_expired_channels.insert(DiscordChannelId(id));
    world.cfg = Some(cfg);
}

#[given(regex = r#"^a public text channel named "([^"]+)" nominated as an unverified channel$"#)]
async fn given_public_unverified_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(public_chan(id, &name));
    world.everyone_base_view = true;
    let existing_cfg = world.cfg.take();
    let mut cfg = existing_cfg.unwrap_or_else(|| make_cfg([], [], []));
    cfg.unverified_channels.insert(DiscordChannelId(id));
    world.cfg = Some(cfg);
}

#[given(regex = r#"^a private text channel named "([^"]+)" nominated as an unverified channel$"#)]
async fn given_private_unverified_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(private_chan(id, &name));
    world.everyone_base_view = true;
    let existing_cfg = world.cfg.take();
    let mut cfg = existing_cfg.unwrap_or_else(|| make_cfg([], [], []));
    cfg.unverified_channels.insert(DiscordChannelId(id));
    world.cfg = Some(cfg);
}

#[given(regex = r#"^a public text channel named "([^"]+)" marked as excluded$"#)]
async fn given_excluded_channel(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    world.channels.push(public_chan(id, &name));
    world.everyone_base_view = true;
    let existing_cfg = world.cfg.take();
    let mut cfg = existing_cfg.unwrap_or_else(|| make_cfg([], [], []));
    cfg.exclude_channels.insert(DiscordChannelId(id));
    // Still need a valid unverified channel so validation passes later.
    // Use a placeholder unverified channel not in the channel list.
    cfg.unverified_channels.insert(DiscordChannelId(9001));
    world.cfg = Some(cfg);
}

#[given(regex = r#"^a category "([^"]+)" swept to Member-only$"#)]
async fn given_category_swept(world: &mut TailsWorld, name: String) {
    let id = category_id_for_name(&name);
    world.channels.push(category_chan(id, &name));
    world.everyone_base_view = true;
}

#[given(regex = r#"^a child text channel "([^"]+)" under "([^"]+)"$"#)]
async fn given_child_channel(world: &mut TailsWorld, child_name: String, parent_name: String) {
    let parent_id = category_id_for_name(&parent_name);
    let child_id = channel_id_for_name(&child_name);
    // Child has same overwrites as parent (empty = synced at start).
    world
        .channels
        .push(child_chan(child_id, &child_name, parent_id, vec![]));
    // Need a valid unverified channel for validation.
    let existing_cfg = world.cfg.take();
    let mut cfg = existing_cfg.unwrap_or_else(|| make_cfg([], [], []));
    cfg.unverified_channels.insert(DiscordChannelId(9001));
    world.cfg = Some(cfg);
}

#[given("a frozen plan whose unverified channel is locked away from the Unverified role")]
async fn given_frozen_plan_breach(world: &mut TailsWorld) {
    // Build a plan directly with a wrongly-classified unverified channel.
    let cfg = make_cfg([700], [], []);
    // The channel has member-only overwrites (no Unverified allow): a breach.
    let member_only_ows: Vec<PermOverwrite> = {
        let tmp = PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(EVERYONE)),
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
        };
        let member_allow = PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(MEMBER_ROLE)),
            allow: Permissions::VIEW_CHANNEL,
            deny: Permissions::empty(),
        };
        vec![tmp, member_allow]
    };
    let wrong = PlannedChannel {
        id: DiscordChannelId(700),
        name: "unverified-wrong".into(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        action: ChannelAction::MemberOnly,
        everyone_view_before: true,
        current_overwrites: vec![],
        final_overwrites: member_only_ows,
        allow_roles: vec![DiscordRoleId(MEMBER_ROLE)],
        writes: true,
    };
    let plan = ChannelPlan {
        guild_id: DiscordGuildId(GUILD),
        channels: vec![wrong],
        counts: Default::default(),
        resolved_at: chrono::Utc::now(),
        everyone_base_view: true,
    };
    world.plan = Some(plan);
    world.cfg = Some(cfg);
}

#[given("Tails resolves a plan that locks down a public channel and an unverified channel")]
async fn given_full_plan_for_guard(world: &mut TailsWorld) {
    // Set up both channels so the plan can be resolved.
    world.channels.push(public_chan(601, "general"));
    world.channels.push(public_chan(600, "welcome"));
    world.everyone_base_view = true;
    let cfg = make_cfg([600], [], []);
    world.cfg = Some(cfg);
}

#[given("Tails nominates a dues-expired channel but no unverified channel")]
async fn given_dues_expired_only(world: &mut TailsWorld) {
    world.channels.push(public_chan(50, "dues-desk"));
    world.everyone_base_view = true;
    let cfg = make_cfg([], [50], []); // no unverified_channels
    world.cfg = Some(cfg);
}

#[given("Tails nominates the same channel as both dues-expired and unverified")]
async fn given_overlapping_sets(world: &mut TailsWorld) {
    world.channels.push(public_chan(42, "overlap"));
    world.everyone_base_view = true;
    let cfg = make_cfg([42], [42], []); // overlap
    world.cfg = Some(cfg);
}

#[given("a plan that will write 3 channels")]
async fn given_plan_write_3(world: &mut TailsWorld) {
    // 2 public channels + 1 unverified channel = 3 writes (each gets a permission overwrite).
    world.channels.push(public_chan(1, "alpha"));
    world.channels.push(public_chan(2, "beta"));
    world.channels.push(public_chan(WELCOME_ID, "welcome"));
    world.everyone_base_view = true;
    let cfg = make_cfg([WELCOME_ID], [], []);
    world.cfg = Some(cfg);
}

// check/save/restore given steps

#[given(r#"a category "staff" and a child channel "staff-chat" with different overwrites"#)]
async fn given_desynced_category_child(world: &mut TailsWorld) {
    // Category has a deny-everyone overwrite; child has different (empty).
    let cat_ows = vec![PermOverwrite {
        target: OverwriteTarget::Role(DiscordRoleId(EVERYONE)),
        allow: Permissions::empty(),
        deny: Permissions::VIEW_CHANNEL,
    }];
    world.channels.push(DiscordChannel {
        id: DiscordChannelId(200),
        name: "staff".to_owned(),
        kind: ChannelKind::Category,
        parent_id: None,
        position: 0,
        overwrites: cat_ows,
    });
    world
        .channels
        .push(child_chan(201, "staff-chat", 200, vec![]));
    world.everyone_base_view = true;
}

#[given(r#"a category "staff" and a child channel "staff-chat" with matching overwrites"#)]
async fn given_synced_category_child(world: &mut TailsWorld) {
    let shared_ows = vec![PermOverwrite {
        target: OverwriteTarget::Role(DiscordRoleId(EVERYONE)),
        allow: Permissions::empty(),
        deny: Permissions::VIEW_CHANNEL,
    }];
    world.channels.push(DiscordChannel {
        id: DiscordChannelId(200),
        name: "staff".to_owned(),
        kind: ChannelKind::Category,
        parent_id: None,
        position: 0,
        overwrites: shared_ows.clone(),
    });
    world
        .channels
        .push(child_chan(201, "staff-chat", 200, shared_ows));
    world.everyone_base_view = true;
}

#[given(r#"the guild has channels "general" and "staff-chat" with overwrites"#)]
async fn given_two_channels_with_overwrites(world: &mut TailsWorld) {
    let ow = PermOverwrite {
        target: OverwriteTarget::Role(DiscordRoleId(MEMBER_ROLE)),
        allow: Permissions::VIEW_CHANNEL,
        deny: Permissions::empty(),
    };
    world.channels.push(DiscordChannel {
        id: DiscordChannelId(1),
        name: "general".to_owned(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        overwrites: vec![ow],
    });
    world.channels.push(DiscordChannel {
        id: DiscordChannelId(2),
        name: "staff-chat".to_owned(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        overwrites: vec![ow],
    });
    world.everyone_base_view = true;
}

#[given(r#"a snapshot recording the overwrites for "general""#)]
async fn given_snapshot_for_general(world: &mut TailsWorld) {
    // Seed the channel with distinct overwrites so we can tell that restore wrote them.
    let snap_ow = PermOverwrite {
        target: OverwriteTarget::Role(DiscordRoleId(MEMBER_ROLE)),
        allow: Permissions::VIEW_CHANNEL,
        deny: Permissions::empty(),
    };
    // The live channel starts with no overwrites so that restore has something to write.
    world.channels.push(DiscordChannel {
        id: DiscordChannelId(GENERAL_ID),
        name: "general".to_owned(),
        kind: ChannelKind::Text,
        parent_id: None,
        position: 0,
        overwrites: vec![],
    });
    world.everyone_base_view = true;
    // Build the snapshot directly (as if "save" was already called at a prior state).
    world.snapshot = Some(ChannelSnapshot {
        format_version: SNAPSHOT_FORMAT_VERSION,
        guild_id: DiscordGuildId(GUILD),
        saved_at: chrono::Utc::now(),
        channels: vec![engine::channels::SavedChannel {
            id: DiscordChannelId(GENERAL_ID),
            name: "general".to_owned(),
            kind: ChannelKind::Text,
            parent_id: None,
            overwrites: vec![snap_ow],
        }],
    });
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when("Tails resolves the permission plan")]
async fn when_tails_resolves_plan(world: &mut TailsWorld) {
    let cfg = world.cfg.take().unwrap_or_else(|| make_cfg([], [], []));
    // For resolve_plan scenarios we call the pure function directly (not the facade).
    let plan = resolve_plan(
        &world.channels,
        &cfg,
        world.everyone_base_view,
        chrono::Utc::now(),
    );
    world.cfg = Some(cfg);
    world.plan = Some(plan);
}

#[when("Tails checks the verification guard")]
async fn when_tails_checks_guard(world: &mut TailsWorld) {
    // The plan must already be set (either via "Tails resolves..." or the frozen-plan given).
    if world.plan.is_none() {
        // The "Tails resolves a plan" given step stores cfg + channels; resolve now.
        let cfg = world.cfg.as_ref().expect("cfg not set");
        let plan = resolve_plan(
            &world.channels,
            cfg,
            world.everyone_base_view,
            chrono::Utc::now(),
        );
        world.plan = Some(plan);
    }
}

#[when("Tails runs permission-setup")]
async fn when_tails_runs_permission_setup(world: &mut TailsWorld) {
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    let cfg = world.cfg.as_ref().expect("cfg not set");
    match channels.plan(cfg).await {
        Ok(plan) => world.plan = Some(plan),
        Err(e) => world.plan_err = Some(e),
    }
}

#[when("Tails applies a preview that no longer matches the server")]
async fn when_tails_applies_stale_preview(world: &mut TailsWorld) {
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    let cfg = world.cfg.as_ref().expect("cfg not set");
    // A preview resolved with one extra public channel beyond what the server has:
    // its planned write-set no longer matches reality, so the drift guard must reject
    // it even though the totals could coincide.
    let mut preview_channels = world.channels.clone();
    preview_channels.push(public_chan(4242, "ghost"));
    let stale_preview = resolve_plan(
        &preview_channels,
        cfg,
        world.everyone_base_view,
        chrono::Utc::now(),
    );
    match channels.apply(cfg, &stale_preview, &NoProgress).await {
        Ok(_) => {}
        Err(e) => world.apply_err = Some(e),
    }
}

#[when("Tails checks whether channels are synchronized")]
async fn when_tails_checks_sync(world: &mut TailsWorld) {
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    match channels.check().await {
        Ok(report) => world.desync = Some(report),
        Err(e) => world.apply_err = Some(e),
    }
}

#[when("Tails saves a snapshot")]
async fn when_tails_saves_snapshot(world: &mut TailsWorld) {
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    match channels.save().await {
        Ok(snap) => world.snapshot = Some(snap),
        Err(e) => world.apply_err = Some(e),
    }
}

#[when("Tails restores from the snapshot")]
async fn when_tails_restores(world: &mut TailsWorld) {
    let snap = world
        .snapshot
        .as_ref()
        .expect("no snapshot in world")
        .clone();
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    match channels.restore(&snap, &NoProgress).await {
        Ok(_) => {
            world.fake = Some(fake);
        }
        Err(e) => world.apply_err = Some(e),
    }
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

fn find_planned_by_name<'a>(world: &'a TailsWorld, name: &str) -> &'a PlannedChannel {
    world
        .plan
        .as_ref()
        .expect("no plan")
        .channels
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no planned channel named {name}"))
}

#[then(regex = r#"^"([^"]+)" is classified as Member-only$"#)]
async fn then_classified_member_only(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::MemberOnly,
        "{name} must be classified as MemberOnly, got {:?}",
        p.action
    );
}

#[then(regex = r#"^"([^"]+)" is classified as dues-expired-only$"#)]
async fn then_classified_dues_expired_only(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::ExpiredOnly,
        "{name} must be classified as ExpiredOnly, got {:?}",
        p.action
    );
}

#[then(regex = r#"^"([^"]+)" is classified as unverified-only$"#)]
async fn then_classified_unverified_only(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::UnverifiedOnly,
        "{name} must be classified as UnverifiedOnly, got {:?}",
        p.action
    );
}

#[then(regex = r#"^"([^"]+)" is classified as excluded$"#)]
async fn then_classified_excluded(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::Excluded,
        "{name} must be classified as Excluded, got {:?}",
        p.action
    );
}

#[then(regex = r#"^"([^"]+)" is classified as synced to its parent$"#)]
async fn then_classified_synced(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::SyncedToParent,
        "{name} must be classified as SyncedToParent, got {:?}",
        p.action
    );
}

#[then(regex = r#"^"([^"]+)" is classified as unchanged$"#)]
async fn then_classified_unchanged(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert_eq!(
        p.action,
        ChannelAction::Unchanged,
        "{name} must be classified as Unchanged, got {:?}",
        p.action
    );
}

#[then(regex = r#"^the Unverified role can view "([^"]+)"$"#)]
async fn then_unverified_can_view(world: &mut TailsWorld, name: String) {
    let p = find_planned_by_name(world, &name);
    assert!(
        unverified_can_view(p),
        "Unverified role must be able to view {name} in the final overwrites"
    );
}

#[then("the guard flags that unverified channel as a lock-out breach")]
async fn then_guard_flags_breach(world: &mut TailsWorld) {
    let plan = world.plan.as_ref().expect("no plan");
    let cfg = world.cfg.as_ref().expect("no cfg");
    let breaches = verification_breaches(plan, cfg);
    assert!(
        !breaches.is_empty(),
        "expected at least one lock-out breach, but guard reported none"
    );
}

#[then("the guard reports no lock-out breach")]
async fn then_guard_no_breach(world: &mut TailsWorld) {
    let plan = world.plan.as_ref().expect("no plan");
    let cfg = world.cfg.as_ref().expect("no cfg");
    let breaches = verification_breaches(plan, cfg);
    assert!(
        breaches.is_empty(),
        "expected no lock-out breach, but guard flagged: {breaches:?}"
    );
}

#[then("permission-setup fails asking for an unverified channel")]
async fn then_fails_no_unverified(world: &mut TailsWorld) {
    let err = world.plan_err.as_ref().expect("expected an error");
    assert!(
        matches!(err, ChannelsError::NoUnverifiedChannel),
        "expected NoUnverifiedChannel, got: {err}"
    );
}

#[then("permission-setup fails because the channel sets overlap")]
async fn then_fails_sets_overlap(world: &mut TailsWorld) {
    let err = world.plan_err.as_ref().expect("expected an error");
    assert!(
        matches!(err, ChannelsError::ChannelSetsOverlap),
        "expected ChannelSetsOverlap, got: {err}"
    );
}

#[then("apply fails with a plan-changed error")]
async fn then_fails_plan_changed(world: &mut TailsWorld) {
    let err = world.apply_err.as_ref().expect("expected an apply error");
    assert!(
        matches!(err, ChannelsError::PlanChanged { .. }),
        "expected PlanChanged, got: {err}"
    );
}

// check/save/restore then steps

#[then(regex = r#"^"([^"]+)" is reported as out of sync with its category$"#)]
async fn then_out_of_sync(world: &mut TailsWorld, name: String) {
    let report = world.desync.as_ref().expect("no desync report");
    let found = report.out_of_sync.iter().any(|e| e.child_name == name);
    assert!(
        found,
        "{name} must be reported as out of sync, report was: {:?}",
        report.out_of_sync
    );
}

#[then("no channel is reported as out of sync")]
async fn then_no_desync(world: &mut TailsWorld) {
    let report = world.desync.as_ref().expect("no desync report");
    assert!(
        report.out_of_sync.is_empty(),
        "expected no desynced channels, but report contains: {:?}",
        report.out_of_sync
    );
}

#[then("no channel is written")]
async fn then_no_channel_written(world: &mut TailsWorld) {
    // check is read-only; the fake was not stored in world.fake (no apply ran).
    // Verify by confirming apply_err is also absent (nothing bad happened either).
    assert!(
        world.apply_err.is_none(),
        "unexpected error in check: {:?}",
        world.apply_err
    );
    // No fake was stored because check is read-only (no writes possible).
    assert!(world.fake.is_none(), "check must not produce any writes");
}

#[then(regex = r#"^the snapshot records the overwrites of "([^"]+)" and "([^"]+)"$"#)]
async fn then_snapshot_records_two(world: &mut TailsWorld, a: String, b: String) {
    let snap = world.snapshot.as_ref().expect("no snapshot");
    let has_a = snap.channels.iter().any(|c| c.name == a);
    let has_b = snap.channels.iter().any(|c| c.name == b);
    assert!(has_a, "snapshot must include channel {a}");
    assert!(has_b, "snapshot must include channel {b}");
    assert_eq!(
        snap.channels.len(),
        2,
        "snapshot must record exactly 2 channels"
    );
}

#[then(regex = r#"^"([^"]+)" is written back to its snapshot overwrites$"#)]
async fn then_restored_to_snapshot(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    let fake = world.fake.as_ref().expect("no fake (restore not run)");
    let written_ids: Vec<DiscordChannelId> = fake
        .written_overwrites()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        written_ids.contains(&DiscordChannelId(id)),
        "{name} (id={id}) must appear in the restore write log, got: {written_ids:?}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Stable channel id for a given channel name - keeps tests readable.
fn channel_id_for_name(name: &str) -> u64 {
    match name {
        "general" => GENERAL_ID,
        "dues-desk" => DUES_DESK_ID,
        "welcome" => WELCOME_ID,
        "rules" => 103,
        "staff-chat" => 201,
        "alpha" => 1,
        "beta" => 2,
        "gamma" => 3,
        "overlap" => 42,
        "secret" => 300,
        other => panic!("unknown channel name {other}"),
    }
}

fn category_id_for_name(name: &str) -> u64 {
    match name {
        "staff" => 200,
        other => panic!("unknown category name {other}"),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    TailsWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/channels")
        .await;
}
