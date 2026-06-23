//! Behaviour suite for the scheduled-scan plan + mass-demote tripwire (`engine::scan::plan`).
//!
//! Cast: the scan reconciles a roster of Sonic-cast members. Sonic (good standing) and Tails
//! (lapsed) are named actors for the mix scenario; Ghost is linked in the cache to a different
//! account (conflict); Rouge is linked but her record has no standing (malformed - left
//! untouched). Bulk scenarios use anonymous numeric members.

use cucumber::{World as _, given, then, when};

use engine::backends::discord::DiscordRosterMember;
use engine::scan::{ScanPlan, ScanThreshold, ScanVerdict, plan};
use engine::store::{InMemoryStore, Index, MemberRecord};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

use domain::MigsStatus;

// The fixed threshold from the migrated plan_tests.
const THRESH: ScanThreshold = ScanThreshold {
    percent: 20,
    floor: 5,
};

/// Fixed ids and handles for named actors.
fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Sonic" => 1,
        "Tails" => 2,
        "Knuckles" => 3,
        "Shadow" => 4,
        "Ghost" => 8,
        "Rouge" => 7,
        other => panic!("unknown actor {other}"),
    };
    (DiscordUserId(raw), DiscordHandle(name.to_lowercase()))
}

/// Build a roster member for `name` with the given held roles.
fn roster_member(name: &str, held: Vec<domain::Role>) -> DiscordRosterMember {
    let (id, handle) = actor(name);
    DiscordRosterMember {
        id,
        handle,
        held,
        bot: false,
    }
}

/// Build an anonymous roster member holding the given roles.
fn anon_member(id: u64, held: Vec<domain::Role>) -> DiscordRosterMember {
    DiscordRosterMember {
        id: DiscordUserId(id),
        handle: DiscordHandle(format!("m{id}")),
        held,
        bot: false,
    }
}

/// Build a cache record for `name` with the given standing.
fn record(name: &str, standing: MigsStatus) -> MemberRecord {
    let (id, handle) = actor(name);
    MemberRecord {
        st_user_id: StUserId(format!("st-{}", name.to_lowercase())),
        discord_user_id: Some(id),
        discord_handle: Some(handle),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        full_name: Some(name.to_owned()),
        standing: Some(standing),
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    }
}

/// Build a cache record for `name` with no standing (malformed).
fn malformed_record(name: &str) -> MemberRecord {
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

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct ScanWorld {
    roster: Vec<DiscordRosterMember>,
    known: Vec<MemberRecord>,
    plan: Option<ScanPlan>,
}

impl std::fmt::Debug for ScanWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanWorld")
            .field("roster_len", &self.roster.len())
            .field("known_len", &self.known.len())
            .field("plan", &self.plan)
            .finish()
    }
}

impl ScanWorld {
    async fn new() -> Self {
        Self {
            roster: Vec::new(),
            known: Vec::new(),
            plan: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given(regex = r"^(\w+) is in the roster holding no role, known to us as a Member$")]
async fn roster_known_member(world: &mut ScanWorld, name: String) {
    world.roster.push(roster_member(&name, vec![]));
    world
        .known
        .push(record(&name, MigsStatus::MemberInGoodStanding));
}

#[given(regex = r"^(\w+) is in the roster holding the Member role, known to us as Dues Expired$")]
async fn roster_known_dues_expired(world: &mut ScanWorld, name: String) {
    world
        .roster
        .push(roster_member(&name, vec![domain::Role::Member]));
    world.known.push(record(&name, MigsStatus::Lapsed));
}

#[given(regex = r"^(\d+) members hold the Member role, none known to us$")]
async fn n_members_with_member_role_unknown(world: &mut ScanWorld, count: u64) {
    for i in 1..=count {
        world
            .roster
            .push(anon_member(100 + i, vec![domain::Role::Member]));
    }
}

#[given(regex = r"^(\d+) members hold a managed role, none known to us$")]
async fn n_members_managed_role_unknown(world: &mut ScanWorld, count: u64) {
    for i in 1..=count {
        // Alternate between Member and DuesExpired so both managed roles are represented.
        let role = if i % 2 == 0 {
            domain::Role::DuesExpired
        } else {
            domain::Role::Member
        };
        world.roster.push(anon_member(200 + i, vec![role]));
    }
}

#[given(regex = r"^(\w+) is bound in the cache to a different account but holds the Member role$")]
async fn member_conflict(world: &mut ScanWorld, name: String) {
    let (_, handle) = actor(&name);
    // The roster member has the name's handle but a different id (id 1) than the cache record.
    world.roster.push(DiscordRosterMember {
        id: DiscordUserId(1),
        handle: handle.clone(),
        held: vec![domain::Role::Member],
        bot: false,
    });
    // The cache binds that handle to a different id (the actor's canonical id).
    let (canonical_id, _) = actor(&name);
    world.known.push(MemberRecord {
        st_user_id: StUserId(format!("st-{}", name.to_lowercase())),
        discord_user_id: Some(canonical_id),
        discord_handle: Some(handle),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        full_name: Some(name.to_owned()),
        standing: Some(MigsStatus::MemberInGoodStanding),
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    });
}

#[given(
    regex = r"^(\w+) is in the roster holding the Member role, known to us but with no membership status$"
)]
async fn roster_known_malformed(world: &mut ScanWorld, name: String) {
    world
        .roster
        .push(roster_member(&name, vec![domain::Role::Member]));
    world.known.push(malformed_record(&name));
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when("the scan plans a pass")]
async fn scan_plans_a_pass(world: &mut ScanWorld) {
    let store = InMemoryStore::new(Index::from_records(world.known.clone()));
    world.plan = Some(plan(&store, &world.roster, THRESH).await.unwrap());
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

#[then(regex = r"^the scan scans (\d+) members?$")]
async fn scan_scans(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.scanned, count, "scanned count mismatch");
}

#[then(regex = r"^the scan would change (\d+) members?$")]
async fn scan_would_change(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.changes.len(), count, "changes count mismatch");
}

#[then(regex = r"^the scan counts (\d+) demotion(?:s)?$")]
async fn scan_counts_demotions(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.demotions, count, "demotion count mismatch");
}

#[then(regex = r"^the scan counts (\d+) miss(?:es)?$")]
async fn scan_counts_misses(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.misses, count, "miss count mismatch");
}

#[then(regex = r"^the scan counts (\d+) conflict(?:s)?$")]
async fn scan_counts_conflicts(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.conflicts, count, "conflict count mismatch");
}

#[then(regex = r"^the scan counts (\d+) malformed record(?:s)?$")]
async fn scan_counts_malformed(world: &mut ScanWorld, count: usize) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.malformed, count, "malformed count mismatch");
}

#[then("the scan proceeds")]
async fn scan_proceeds(world: &mut ScanWorld) {
    let p = world.plan.as_ref().expect("no plan");
    assert_eq!(p.verdict, ScanVerdict::Proceed, "expected Proceed verdict");
}

#[then("the scan aborts")]
async fn scan_aborts(world: &mut ScanWorld) {
    let p = world.plan.as_ref().expect("no plan");
    assert!(
        matches!(p.verdict, ScanVerdict::Abort { .. }),
        "expected Abort verdict, got {:?}",
        p.verdict
    );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    ScanWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/scan")
        .await;
}
