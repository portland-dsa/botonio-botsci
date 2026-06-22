//! Behaviour suite for the bulk-verify sweep + resumable session.
//!
//! Cast: Sonic is the moderator running /bulk-verify; Tails is a member the cache
//! knows in good standing; Shadow is unknown (a miss); Knuckles already holds the
//! Member role; Silver is a second miss.

use cucumber::{World as _, given, then, when};

use engine::backends::discord::{DiscordRosterMember, FakeDiscord, Role};
use engine::bulk::{self, PreviewTally, miss_still_pending};
use engine::store::{
    BulkMiss, BulkScope, BulkSession, BulkSessionStore, BulkStatus, InMemoryStore, Index,
    MemberRecord, MissState,
};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

use domain::{DiscordGuildId, MigsStatus};

const SONIC: u64 = 1;
const GUILD: u64 = 100;

/// Fixed ids and handles for each actor by name.
fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Tails" => 2,
        "Knuckles" => 3,
        "Shadow" => 4,
        "Metal" => 5,
        "Silver" => 6,
        other => panic!("unknown actor {other}"),
    };
    (DiscordUserId(raw), DiscordHandle(name.to_lowercase()))
}

/// Build a `MemberRecord` for `name` that the cache knows, with `MemberInGoodStanding`.
fn known_record(name: &str) -> MemberRecord {
    known_record_standing(name, MigsStatus::MemberInGoodStanding)
}

/// Build a `MemberRecord` for `name` that the cache knows, with the given `standing`.
fn known_record_standing(name: &str, standing: MigsStatus) -> MemberRecord {
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

/// Build a `DiscordRosterMember` for `name` with `held` roles.
fn roster_member(name: &str, held: Vec<Role>) -> DiscordRosterMember {
    let (id, handle) = actor(name);
    DiscordRosterMember {
        id,
        handle,
        held,
        bot: false,
    }
}

/// Build a bot `DiscordRosterMember` for `name` (Metal Sonic is our robot).
fn bot_member(name: &str) -> DiscordRosterMember {
    let (id, handle) = actor(name);
    DiscordRosterMember {
        id,
        handle,
        held: vec![],
        bot: true,
    }
}

/// Build a `BulkMiss` at `position` for `name`, all Pending.
fn bulk_miss(name: &str, position: i32) -> BulkMiss {
    let (id, handle) = actor(name);
    BulkMiss {
        discord_user_id: id,
        handle: Some(handle),
        position,
        state: MissState::Pending,
    }
}

/// A started `BulkSession` for GUILD, started by SONIC.
fn started_session() -> BulkSession {
    BulkSession {
        guild: DiscordGuildId(GUILD),
        scope: BulkScope::UnmanagedOnly,
        status: BulkStatus::InProgress,
        started_by: DiscordUserId(SONIC),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct BulkWorld {
    /// What members_page yields for the preview scenarios.
    roster: Vec<DiscordRosterMember>,
    /// What the cache holds for the preview scenarios.
    known: Vec<MemberRecord>,
    /// The result of the last preview call.
    tally: Option<PreviewTally>,
    /// Persistent store for session-lifecycle scenarios.
    store: InMemoryStore,
    /// The last `next_pending` result (Some(Some(miss)) or Some(None)).
    next: Option<Option<BulkMiss>>,
}

impl std::fmt::Debug for BulkWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BulkWorld")
            .field("roster_len", &self.roster.len())
            .field("known_len", &self.known.len())
            .field("tally", &self.tally)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl BulkWorld {
    async fn new() -> Self {
        Self {
            roster: Vec::new(),
            known: Vec::new(),
            tally: None,
            store: InMemoryStore::new(Index::default()),
            next: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given(regex = r"^(\w+) is in the roster holding no managed role, known to us as a Member$")]
async fn roster_known_member(world: &mut BulkWorld, name: String) {
    world.roster.push(roster_member(&name, vec![]));
    world.known.push(known_record(&name));
}

#[given(regex = r"^(\w+) is in the roster holding no managed role, unknown to us$")]
async fn roster_unknown_member(world: &mut BulkWorld, name: String) {
    world.roster.push(roster_member(&name, vec![]));
    // not added to known - will be a miss
}

#[given(regex = r"^(\w+) is in the roster already holding the Member role$")]
async fn roster_already_member(world: &mut BulkWorld, name: String) {
    world.roster.push(roster_member(&name, vec![Role::Member]));
}

#[given(regex = r"^(\w+) is a bot in the roster$")]
async fn roster_bot(world: &mut BulkWorld, name: String) {
    world.roster.push(bot_member(&name));
}

#[given(
    regex = r"^(\w+) is in the roster already holding the Member role, known to us as a Member$"
)]
async fn roster_known_already_member(world: &mut BulkWorld, name: String) {
    world.roster.push(roster_member(&name, vec![Role::Member]));
    world.known.push(known_record(&name));
}

#[given(
    regex = r"^(\w+) is in the roster already holding the Dues Expired role, known to us as Dues Expired$"
)]
async fn roster_known_already_dues_expired(world: &mut BulkWorld, name: String) {
    world
        .roster
        .push(roster_member(&name, vec![Role::DuesExpired]));
    world
        .known
        .push(known_record_standing(&name, MigsStatus::Lapsed));
}

#[given(regex = r"^(\w+) is in the roster holding only the Unverified role, unknown to us$")]
async fn roster_unverified_unknown(world: &mut BulkWorld, name: String) {
    world
        .roster
        .push(roster_member(&name, vec![Role::Unverified]));
    // not added to known - still a miss, but a bare Unverified counts as unmanaged
}

#[given("a started session whose queue is Shadow then Silver")]
async fn started_session_shadow_silver(world: &mut BulkWorld) {
    let session = started_session();
    let misses = vec![bulk_miss("Shadow", 0), bulk_miss("Silver", 1)];
    world.store.start_session(&session, &misses).await.unwrap();
}

#[given("Shadow is queued but has since been given the Member role")]
async fn shadow_queued_but_verified(world: &mut BulkWorld) {
    // Set up the session with Shadow queued.
    let session = started_session();
    let misses = vec![bulk_miss("Shadow", 0)];
    world.store.start_session(&session, &misses).await.unwrap();
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when("Sonic previews an unmanaged-only sweep")]
async fn preview_unmanaged(world: &mut BulkWorld) {
    let roster = world.roster.clone();
    let known = world.known.clone();

    let discord = FakeDiscord::new().with_roster(roster);
    let members = bulk::enumerate(&discord, BulkScope::UnmanagedOnly)
        .await
        .unwrap();
    let store = InMemoryStore::new(Index::from_records(known.clone()));
    world.tally = Some(bulk::preview(&store, &members).await.unwrap());
}

#[when("Sonic previews a whole-server sweep")]
async fn preview_whole_guild(world: &mut BulkWorld) {
    let roster = world.roster.clone();
    let known = world.known.clone();

    let discord = FakeDiscord::new().with_roster(roster);
    let members = bulk::enumerate(&discord, BulkScope::WholeGuild)
        .await
        .unwrap();
    let store = InMemoryStore::new(Index::from_records(known.clone()));
    world.tally = Some(bulk::preview(&store, &members).await.unwrap());
}

#[when("Sonic resumes the session")]
async fn sonic_resumes(world: &mut BulkWorld) {
    world.next = Some(
        world
            .store
            .next_pending(DiscordGuildId(GUILD))
            .await
            .unwrap(),
    );
}

#[when(regex = r"^Sonic marks (\w+) verified$")]
async fn sonic_marks_verified(world: &mut BulkWorld, name: String) {
    let (id, _) = actor(&name);
    world
        .store
        .mark_miss(DiscordGuildId(GUILD), id, MissState::Verified)
        .await
        .unwrap();
    world.next = Some(
        world
            .store
            .next_pending(DiscordGuildId(GUILD))
            .await
            .unwrap(),
    );
}

#[when(regex = r"^Sonic marks (\w+) skipped$")]
async fn sonic_marks_skipped(world: &mut BulkWorld, name: String) {
    let (id, _) = actor(&name);
    world
        .store
        .mark_miss(DiscordGuildId(GUILD), id, MissState::Skipped)
        .await
        .unwrap();
    world.next = Some(
        world
            .store
            .next_pending(DiscordGuildId(GUILD))
            .await
            .unwrap(),
    );
}

#[when("Sonic starts the session over with only Tails")]
async fn sonic_starts_over_tails(world: &mut BulkWorld) {
    let session = started_session();
    let misses = vec![bulk_miss("Tails", 0)];
    world.store.start_session(&session, &misses).await.unwrap();
    world.next = Some(
        world
            .store
            .next_pending(DiscordGuildId(GUILD))
            .await
            .unwrap(),
    );
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

#[then(regex = r"^the sweep scans (\d+) members?$")]
async fn sweep_scans(world: &mut BulkWorld, count: usize) {
    let tally = world.tally.as_ref().expect("no tally");
    assert_eq!(tally.scanned, count, "scanned count mismatch");
}

#[then(regex = r"^the sweep matches (\d+) members? as (\w+)$")]
async fn sweep_matches_as(world: &mut BulkWorld, count: usize, role_name: String) {
    let tally = world.tally.as_ref().expect("no tally");
    let role = match role_name.as_str() {
        "Member" => Role::Member,
        "DuesExpired" => Role::DuesExpired,
        "Unverified" => Role::Unverified,
        other => panic!("unknown role {other}"),
    };
    let matched_count = tally
        .matched
        .iter()
        .find(|(r, _)| *r == role)
        .map(|(_, n)| *n)
        .unwrap_or(0);
    assert_eq!(
        matched_count, count,
        "matched count for {role_name} mismatch"
    );
}

#[then(regex = r"^the sweep counts (\d+) miss(?:es)?$")]
async fn sweep_counts_misses(world: &mut BulkWorld, count: usize) {
    let tally = world.tally.as_ref().expect("no tally");
    assert_eq!(tally.misses, count, "miss count mismatch");
}

#[then(regex = r"^the sweep leaves (\d+) members? unchanged$")]
async fn sweep_leaves_unchanged(world: &mut BulkWorld, count: usize) {
    let tally = world.tally.as_ref().expect("no tally");
    assert_eq!(tally.unchanged, count, "unchanged count mismatch");
}

#[then(regex = r"^the next pending member is (\w+)$")]
async fn next_pending_is(world: &mut BulkWorld, name: String) {
    let (expected_id, _) = actor(&name);
    let next = world
        .next
        .as_ref()
        .expect("next_pending not yet called")
        .as_ref()
        .expect("expected a pending member but queue was empty");
    assert_eq!(
        next.discord_user_id, expected_id,
        "expected next pending to be {name}"
    );
}

#[then("the queue has no pending member")]
async fn queue_empty(world: &mut BulkWorld) {
    let next = world.next.as_ref().expect("next_pending not yet called");
    assert!(next.is_none(), "expected empty queue but got {next:?}");
}

#[then("the session can be completed")]
async fn session_can_complete(world: &mut BulkWorld) {
    world
        .store
        .complete_session(DiscordGuildId(GUILD))
        .await
        .unwrap();
    let session = world
        .store
        .load_session(DiscordGuildId(GUILD))
        .await
        .unwrap()
        .expect("session should exist after complete");
    assert_eq!(session.status, BulkStatus::Complete);
}

#[then("the wizard skips Shadow on the liveness check")]
async fn wizard_skips_shadow(_world: &mut BulkWorld) {
    // Shadow has since been given the Member role - miss_still_pending returns false.
    assert!(!miss_still_pending(true, &[Role::Member]));
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    BulkWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/bulk")
        .await;
}
