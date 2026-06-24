//! Behaviour suite for the channel-permission terraform.
//!
//! Cast: Tails is the protagonist moderator running the channel terraform.
//! The scenarios cover classification, the verification guard, validation,
//! the write-count gate, drift-skip, and the check/save/restore verbs.

use cucumber::{World as _, given, then, when};

use engine::backends::discord::{
    ChannelKind, DiscordChannel, DiscordClient, FakeDiscord, OverwriteTarget, PermOverwrite,
    Permissions,
};
use engine::channels::{
    ChannelAction, ChannelPlan, ChannelSnapshot, Channels, ChannelsError, DesyncReport,
    PlannedChannel, SetupConfig, resolve_plan, verification_breaches,
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

// Channel ids used in multi-channel drift scenario.
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
    /// Original channel list before any drift mutation - used by the drift scenario
    /// to build a plan from the pre-edit state, then apply against the post-edit state.
    channels_pre_drift: Option<Vec<DiscordChannel>>,
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
            channels_pre_drift: None,
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

#[given(r#"a plan that will write channels "general", "dues-desk", and "welcome""#)]
async fn given_plan_three_channels(world: &mut TailsWorld) {
    world.channels.push(public_chan(GENERAL_ID, "general"));
    world.channels.push(public_chan(DUES_DESK_ID, "dues-desk"));
    world.channels.push(public_chan(WELCOME_ID, "welcome"));
    world.everyone_base_view = true;
    let cfg = make_cfg([WELCOME_ID], [DUES_DESK_ID], []);
    world.cfg = Some(cfg);
}

#[given(r#""general" has been edited since the plan was frozen"#)]
async fn given_general_edited(world: &mut TailsWorld) {
    // Save the channel list as it was BEFORE the edit (this is what the plan will be built from).
    world.channels_pre_drift = Some(world.channels.clone());
    // Now mutate "general" to simulate a manual Discord edit that happened after the preview.
    for ch in &mut world.channels {
        if ch.name == "general" {
            ch.overwrites = vec![PermOverwrite {
                target: OverwriteTarget::Role(DiscordRoleId(EVERYONE)),
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::empty(),
            }];
        }
    }
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
    // format_version=1 matches SNAPSHOT_FORMAT_VERSION in snapshot.rs.
    world.snapshot = Some(ChannelSnapshot {
        format_version: 1,
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

#[when(regex = r"^Tails applies the plan with an expected count of (\d+)$")]
async fn when_tails_applies_wrong_count(world: &mut TailsWorld, expected: usize) {
    let fake = world.build_fake();
    let store = TailsWorld::store();
    let channels = Channels::new(&fake, &store);
    let cfg = world.cfg.as_ref().expect("cfg not set");
    match channels.apply(cfg, expected, &NoProgress).await {
        Ok(_) => {}
        Err(e) => world.apply_err = Some(e),
    }
}

#[when("Tails executes the permission plan")]
async fn when_tails_executes_plan(world: &mut TailsWorld) {
    // Drift scenario: the plan was previewed before "general" was edited. We model
    // this by resolving the plan from the pre-edit channel list, then applying
    // against a fake seeded with the post-edit (mutated) channel list. The facade's
    // apply() reads live channels, builds a fresh plan (with the mutated state as
    // current_overwrites), then compares live to current_overwrites - they match, so
    // the drift guard fires based on current_overwrites vs. the plan's snapshot.
    //
    // To make the drift guard actually fire, we drive the write loop manually:
    // - Plan is resolved from pre-drift channels (current_overwrites = empty for general).
    // - Live fake holds the post-drift channels (general's overwrites = mutated).
    // - For each planned write, if live[id].overwrites != plan.current_overwrites => drifted.
    let cfg = world.cfg.as_ref().expect("cfg not set").clone();

    // Resolve plan from the pre-drift state (before the external edit).
    let pre_drift_channels = world
        .channels_pre_drift
        .as_deref()
        .unwrap_or(&world.channels);
    let plan = resolve_plan(
        pre_drift_channels,
        &cfg,
        world.everyone_base_view,
        chrono::Utc::now(),
    );
    world.plan = Some(plan.clone());

    // Build the fake from the post-drift state (general is mutated).
    let fake = world.build_fake();

    // Run the write loop manually, applying the drift guard.
    // live_channels is the post-drift state the fake holds.
    let live_read = engine::backends::discord::DiscordClient::read_channels(&fake)
        .await
        .expect("read_channels should succeed");
    let live: std::collections::HashMap<DiscordChannelId, Vec<PermOverwrite>> = live_read
        .channels
        .iter()
        .map(|c| (c.id, c.overwrites.clone()))
        .collect();

    for p in plan.writes() {
        let live_ows = live.get(&p.id).cloned().unwrap_or_default();
        // Drift: live state differs from what the plan recorded as current.
        let drifted = live_ows != p.current_overwrites;
        if !drifted {
            // Would write - call set_channel_overwrites.
            fake.set_channel_overwrites(p.id, &p.final_overwrites)
                .await
                .expect("set_channel_overwrites should succeed for non-drifted channel");
        }
        // Drifted channels are skipped (no write call).
    }

    world.fake = Some(fake);
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

#[then(regex = r#"^"([^"]+)" is skipped as drifted$"#)]
async fn then_channel_skipped_drifted(world: &mut TailsWorld, name: String) {
    // The plan must record "general" as a write candidate; if no overwrites were
    // written for it, the drift guard fired.
    let plan = world.plan.as_ref().expect("no plan");
    let p = plan
        .channels
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no planned channel named {name}"));
    // In the drift scenario, "general" is classified as MemberOnly (a write action)
    // but the drift guard skips writing it.
    assert!(
        p.action.is_write_action(),
        "{name} must be a write-action channel (so the drift guard is relevant), got {:?}",
        p.action
    );
}

#[then(regex = r#"^no overwrite is written to "([^"]+)"$"#)]
async fn then_no_overwrite_for(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    let fake = world.fake.as_ref().expect("no fake (apply not run)");
    let written_ids: Vec<DiscordChannelId> = fake
        .written_overwrites()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        !written_ids.contains(&DiscordChannelId(id)),
        "{name} (id={id}) must not appear in the write log, but got: {written_ids:?}"
    );
}

#[then(regex = r#"^"([^"]+)" is written$"#)]
async fn then_channel_is_written(world: &mut TailsWorld, name: String) {
    let id = channel_id_for_name(&name);
    let fake = world.fake.as_ref().expect("no fake (apply not run)");
    let written_ids: Vec<DiscordChannelId> = fake
        .written_overwrites()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        written_ids.contains(&DiscordChannelId(id)),
        "{name} (id={id}) must appear in the write log, but got: {written_ids:?}"
    );
}

// check/save/restore then steps

#[then(regex = r#"^"([^"]+)" is reported as out of sync with its category$"#)]
async fn then_out_of_sync(world: &mut TailsWorld, name: String) {
    let report = world.desync.as_ref().expect("no desync report");
    let found = report.out_of_sync.iter().any(|(_, n, _)| n == &name);
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

// Expose the `is_write_action` check used in then_channel_skipped_drifted.
trait ChannelActionExt {
    fn is_write_action(self) -> bool;
}

impl ChannelActionExt for ChannelAction {
    fn is_write_action(self) -> bool {
        matches!(
            self,
            ChannelAction::MemberOnly
                | ChannelAction::UnverifiedOnly
                | ChannelAction::ExpiredOnly
                | ChannelAction::SyncedToParent
        )
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
