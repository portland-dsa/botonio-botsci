//! Behaviour suite for the staging `/strip-roles` reset (`Member::strip`).
//!
//! Cast: Sonic is the moderator running /strip-roles; Knuckles was hand-approved (an
//! override stamp plus the marker); Tails is a plain member the cache knows; Shadow holds
//! a stale override marker with no stamp behind it. Mocks only - the real `DataStore` over
//! a `FakeDiscord` and an `InMemoryStore` plus a recording audit sink - so the suite runs
//! on a plain offline `cargo test` with no database or gateway. Scenarios live in
//! `tests/features/strip/`.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Mutex;

use cucumber::{World as _, given, then, when};

use domain::DiscordGuildId;
use engine::audit::AuditLog;
use engine::backends::discord::{DiscordClient, FakeDiscord, Role, roles::MarkerRole};
use engine::backends::solidarity_tech::FakeSolidarityTech;
use engine::store::{InMemoryStore, Index, MemberRecord, MemberStore, OverrideLog};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use engine::verify::{DataStore, Member, StripOutcome, Target};

const SONIC: u64 = 1;

/// Fixed id and handle for each actor by name.
fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Knuckles" => 3,
        "Tails" => 2,
        "Shadow" => 4,
        other => panic!("unknown actor {other}"),
    };
    (DiscordUserId(raw), DiscordHandle(name.to_lowercase()))
}

/// A cache record linking `name` to their Discord id, so a strip can observe whether the
/// link survives.
fn known_record(name: &str) -> MemberRecord {
    let (id, handle) = actor(name);
    MemberRecord {
        st_user_id: StUserId(format!("st-{}", name.to_lowercase())),
        discord_user_id: Some(id),
        discord_handle: Some(handle),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        full_name: Some(name.to_owned()),
        standing: None,
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    }
}

/// A recording audit sink: it remembers every `(actor, subject, action, detail)`.
#[derive(Default)]
struct RecordingAudit {
    rows: Mutex<Vec<(DiscordUserId, DiscordUserId, String, serde_json::Value)>>,
}

#[async_trait::async_trait]
impl AuditLog for RecordingAudit {
    type Error = Infallible;
    async fn record(
        &self,
        actor: DiscordUserId,
        subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), Infallible> {
        self.rows
            .lock()
            .unwrap()
            .push((actor, subject, action.to_owned(), detail));
        Ok(())
    }
}

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct StripWorld {
    /// Managed status roles each actor holds, by id.
    held: HashMap<u64, Vec<Role>>,
    /// Actors seeded with an override marker (with or without a stamp behind it).
    markers: HashSet<u64>,
    /// Actors carrying a hand-approval stamp - the "overridden" set.
    overridden: HashSet<u64>,
    /// Cache records to seed, so a strip can be checked for keeping or clearing the link.
    known: Vec<MemberRecord>,
    /// The Discord state after the strip, kept for the role/marker assertions.
    discord: Option<FakeDiscord>,
    /// The cache after the strip, kept for the link/stamp assertions.
    store: Option<InMemoryStore>,
    /// The audit sink after the strip.
    audit: Option<RecordingAudit>,
    /// The outcome of the last strip.
    outcome: Option<StripOutcome>,
}

impl std::fmt::Debug for StripWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripWorld")
            .field("held", &self.held)
            .field("markers", &self.markers)
            .field("overridden", &self.overridden)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

impl StripWorld {
    async fn new() -> Self {
        Self {
            held: HashMap::new(),
            markers: HashSet::new(),
            overridden: HashSet::new(),
            known: Vec::new(),
            discord: None,
            store: None,
            audit: None,
            outcome: None,
        }
    }

    /// Run a strip of `name` over freshly-built fakes seeded from the givens, then keep the
    /// fakes so the `then` steps can read the resulting state.
    async fn strip(&mut self, name: &str) {
        let (id, handle) = actor(name);

        // Discord: seed every actor's held roles, then the stale markers.
        let mut discord = FakeDiscord::new();
        for (uid, roles) in &self.held {
            discord = discord.with_roles(DiscordUserId(*uid), roles.clone());
        }
        for uid in &self.markers {
            discord
                .assign_marker_role(DiscordUserId(*uid), MarkerRole::ManualOverride)
                .await
                .unwrap();
        }

        // Cache + override stamps.
        let store = InMemoryStore::new(Index::from_records(self.known.clone()));
        for uid in &self.overridden {
            store
                .stamp_override(DiscordUserId(*uid), DiscordUserId(SONIC), None)
                .await
                .unwrap();
        }

        let st = FakeSolidarityTech::new();
        let audit = RecordingAudit::default();
        let held = self.held.get(&id.0).cloned().unwrap_or_default();
        // The command computes this from the override log; mirror that here.
        let is_overridden = store.get_override(id).await.unwrap().is_some();

        let outcome = {
            let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));
            Member::new(&ds, Target { id, handle })
                .strip(DiscordUserId(SONIC), is_overridden, &held)
                .await
                .expect("strip should succeed over the fakes")
        };

        self.outcome = Some(outcome);
        self.discord = Some(discord);
        self.store = Some(store);
        self.audit = Some(audit);
    }
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given(regex = r"^(\w+) was hand-approved and holds the Member role and the override marker$")]
async fn overridden_member(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    world.held.insert(id.0, vec![Role::Member]);
    world.markers.insert(id.0);
    world.overridden.insert(id.0);
    world.known.push(known_record(&name));
}

#[given(regex = r"^(\w+) holds the Member role and is known to us$")]
async fn plain_known_member(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    world.held.insert(id.0, vec![Role::Member]);
    world.known.push(known_record(&name));
}

#[given(regex = r"^(\w+) holds the Unverified role and a stale override marker$")]
async fn stale_marker_member(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    world.held.insert(id.0, vec![Role::Unverified]);
    world.markers.insert(id.0);
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when(regex = r"^Sonic strips (\w+)$")]
async fn sonic_strips(world: &mut StripWorld, name: String) {
    world.strip(&name).await;
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

#[then(regex = r"^(\w+)'s managed roles are stripped$")]
async fn roles_stripped(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    let discord = world.discord.as_ref().expect("no strip ran");
    assert!(
        discord.roles_of(id).is_empty(),
        "{name} should hold no managed roles after the strip"
    );
}

#[then(regex = r"^(\w+)'s override marker is cleared$")]
async fn marker_cleared(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    let discord = world.discord.as_ref().expect("no strip ran");
    assert!(
        !discord.has_marker(id, MarkerRole::ManualOverride),
        "{name}'s marker should be cleared"
    );
}

#[then(regex = r"^(\w+)'s cache link is cleared$")]
async fn cache_link_cleared(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    let store = world.store.as_ref().expect("no strip ran");
    assert!(
        store.by_discord_id(id).await.unwrap().is_none(),
        "{name}'s cache link should be cleared"
    );
}

#[then(regex = r"^(\w+)'s cache link is intact$")]
async fn cache_link_intact(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    let store = world.store.as_ref().expect("no strip ran");
    assert!(
        store.by_discord_id(id).await.unwrap().is_some(),
        "{name}'s cache link should survive the strip"
    );
}

#[then(regex = r"^(\w+)'s override stamp is deleted$")]
async fn stamp_deleted(world: &mut StripWorld, name: String) {
    let (id, _) = actor(&name);
    let store = world.store.as_ref().expect("no strip ran");
    assert!(
        store.get_override(id).await.unwrap().is_none(),
        "{name}'s override stamp should be deleted"
    );
}

#[then("the reset is recorded in the audit log")]
async fn reset_recorded(world: &mut StripWorld) {
    let audit = world.audit.as_ref().expect("no strip ran");
    let rows = audit.rows.lock().unwrap();
    assert!(
        rows.iter()
            .any(|(_, _, action, _)| action == "member_forget"),
        "expected a member_forget audit row, got {rows:?}"
    );
}

#[then("no reset is recorded in the audit log")]
async fn no_reset_recorded(world: &mut StripWorld) {
    let audit = world.audit.as_ref().expect("no strip ran");
    assert!(
        audit.rows.lock().unwrap().is_empty(),
        "a plain strip must not write to the audit log"
    );
}

#[tokio::main]
async fn main() {
    StripWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/strip")
        .await;
}
