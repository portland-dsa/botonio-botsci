//! Behaviour suite for the dues-reminder planner (`engine::reminders::plan` +
//! `select_milestone`).
//!
//! Cast: Sonic is the selection-outline protagonist; Tails and Amy appear in the
//! gate scenarios. Each scenario seeds one `InMemoryStore` and calls `plan` over it,
//! or calls `select_milestone` directly for the outline.

use chrono::NaiveDate;
use cucumber::{World as _, given, then, when};

use domain::{DiscordGuildId, MigsStatus};

use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::reminders::plan as reminder_plan;
use engine::reminders::{ExpiryStatus, Milestone, ReminderPlan, select_milestone};
use engine::store::{GraceStore, InMemoryStore, Index, MemberRecord, OptOutSource, ReminderStore};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The fixed guild used for all `plan` gate scenarios.
const GUILD: DiscordGuildId = DiscordGuildId(1);

/// A stable "today" for all scenarios so xdate arithmetic is deterministic.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 6, 24).unwrap()
}

// ---------------------------------------------------------------------------
// Actor helpers
// ---------------------------------------------------------------------------

fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Sonic" => 1,
        "Tails" => 2,
        "Amy" => 3,
        other => panic!("unknown actor {other}"),
    };
    (DiscordUserId(raw), DiscordHandle(name.to_lowercase()))
}

fn record_with_xdate(name: &str, xdate: Option<NaiveDate>, linked: bool) -> MemberRecord {
    let (id, handle) = actor(name);
    MemberRecord {
        st_user_id: StUserId(format!("st-{}", name.to_lowercase())),
        discord_user_id: if linked { Some(id) } else { None },
        discord_handle: Some(handle),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        full_name: Some(name.to_owned()),
        standing: Some(MigsStatus::MemberInGoodStanding),
        join_date: None,
        expires: xdate,
        membership_type: Some(MembershipType::Monthly),
        monthly_dues: None,
        yearly_dues: None,
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct RemindersWorld {
    // --- selection outline state ---
    days_until: i64,
    last_sent: Option<Milestone>,
    selection_result: Option<Option<Milestone>>,

    // --- gate scenario state (seeded into InMemoryStore before `plan` runs) ---
    records: Vec<MemberRecord>,
    /// (id, until) for active graces to seed.
    graces: Vec<(DiscordUserId, NaiveDate)>,
    /// ids to opt out.
    opt_outs: Vec<DiscordUserId>,
    /// ids whose reminder state should be seeded with a stale cycle_xdate so the
    /// planner treats last_sent as reset.
    stale_cycles: Vec<DiscordUserId>,
    plan_result: Option<ReminderPlan>,
}

impl std::fmt::Debug for RemindersWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemindersWorld")
            .field("days_until", &self.days_until)
            .field("last_sent", &self.last_sent)
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

impl RemindersWorld {
    async fn new() -> Self {
        Self {
            days_until: 0,
            last_sent: None,
            selection_result: None,
            records: Vec::new(),
            graces: Vec::new(),
            opt_outs: Vec::new(),
            stale_cycles: Vec::new(),
            plan_result: None,
        }
    }

    /// Build an `InMemoryStore` from accumulated gate state and run `plan`.
    async fn run_plan(&mut self) {
        let store = InMemoryStore::new(Index::from_records(self.records.clone()));

        for (id, until) in &self.graces {
            store
                .set_grace(GUILD, *id, *until, DiscordUserId(9999), None)
                .await
                .unwrap();
        }
        for id in &self.opt_outs {
            store
                .opt_out(GUILD, *id, OptOutSource::Member)
                .await
                .unwrap();
        }
        // Seed a stale cycle: record_sent on a date that cannot match the member's current
        // xdate so the planner resets last_sent to None - the "offline through a cycle" case.
        let stale_xdate = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        for id in &self.stale_cycles {
            store
                .record_sent(GUILD, *id, stale_xdate, Milestone::Renewal, 0)
                .await
                .unwrap();
        }

        let result = reminder_plan(&store, GUILD, today()).await.unwrap();
        self.plan_result = Some(result);
    }
}

// ---------------------------------------------------------------------------
// Selection outline steps
// ---------------------------------------------------------------------------

#[given(regex = r"^Sonic's dues expire in (-?\d+) days$")]
async fn sonic_dues_expire_in(world: &mut RemindersWorld, days: i64) {
    world.days_until = days;
}

#[given(regex = r"^his last sent notice is (\w+)$")]
async fn last_sent_notice(world: &mut RemindersWorld, token: String) {
    world.last_sent = match token.as_str() {
        "none" => None,
        s => {
            Some(Milestone::from_token(s).unwrap_or_else(|| panic!("unknown milestone token {s}")))
        }
    };
}

#[when(regex = r"^the reminder planner runs$")]
async fn reminder_planner_runs(world: &mut RemindersWorld) {
    let status = ExpiryStatus::from(chrono::TimeDelta::days(world.days_until));
    let result = select_milestone(status, world.last_sent);
    world.selection_result = Some(result);
}

#[then(regex = r"^he is due the (\w+) notice$")]
async fn he_is_due_notice(world: &mut RemindersWorld, token: String) {
    let result = world.selection_result.expect("planner not run yet");
    let expected: Option<Milestone> = match token.as_str() {
        "none" => None,
        s => {
            Some(Milestone::from_token(s).unwrap_or_else(|| panic!("unknown milestone token {s}")))
        }
    };
    assert_eq!(
        result, expected,
        "milestone mismatch for days_until={}",
        world.days_until
    );
}

// ---------------------------------------------------------------------------
// Gate scenario Given steps
// ---------------------------------------------------------------------------

#[given(regex = r"^Tails lapsed yesterday$")]
async fn tails_lapsed_yesterday(world: &mut RemindersWorld) {
    let xdate = today() - chrono::Duration::days(1);
    world
        .records
        .push(record_with_xdate("Tails", Some(xdate), true));
}

#[given(regex = r"^Tails has opted out of dues reminders$")]
async fn tails_opted_out(world: &mut RemindersWorld) {
    let (id, _) = actor("Tails");
    world.opt_outs.push(id);
}

#[given(regex = r"^Tails has an active grace$")]
async fn tails_active_grace(world: &mut RemindersWorld) {
    let (id, _) = actor("Tails");
    // Grace until tomorrow - active today.
    let until = today() + chrono::Duration::days(1);
    world.graces.push((id, until));
}

#[given(regex = r"^Amy's dues expire in (\d+) days$")]
async fn amy_dues_expire_in(world: &mut RemindersWorld, days: i64) {
    let xdate = today() + chrono::Duration::days(days);
    world
        .records
        .push(record_with_xdate("Amy", Some(xdate), true));
}

#[given(regex = r"^Amy's monthly dues are active$")]
async fn amy_monthly_dues_active(world: &mut RemindersWorld) {
    if let Some(rec) = world
        .records
        .iter_mut()
        .find(|r| r.discord_handle.as_ref().map(|h| h.0.as_str()) == Some("amy"))
    {
        rec.monthly_dues = Some(DuesStatus::Active);
    }
}

#[given(regex = r"^Tails has no expiry date$")]
async fn tails_no_expiry(world: &mut RemindersWorld) {
    world.records.push(record_with_xdate("Tails", None, true));
}

#[given(regex = r"^Tails is unlinked$")]
async fn tails_unlinked(world: &mut RemindersWorld) {
    // A record with no Discord id - unlinked.
    let xdate = today() + chrono::Duration::days(10);
    world
        .records
        .push(record_with_xdate("Tails", Some(xdate), false));
}

#[given(regex = r"^Amy's last cycle was for a different xdate$")]
async fn amy_stale_cycle(world: &mut RemindersWorld) {
    // The planner resets last_sent when `state.cycle_xdate != record.expires`. Seed a
    // `record_sent` on a past date so the stored cycle_xdate cannot match Amy's current
    // xdate - the planner then treats the member as if no notice was sent this cycle.
    let (id, _) = actor("Amy");
    world
        .records
        .iter()
        .find(|r| r.discord_user_id == Some(id))
        .expect("Amy's record must be seeded before stale cycle");
    world.stale_cycles.push(id);
}

// ---------------------------------------------------------------------------
// Gate scenario When steps
// ---------------------------------------------------------------------------

#[when(regex = r"^the reminder sweep is planned$")]
async fn reminder_sweep_planned(world: &mut RemindersWorld) {
    world.run_plan().await;
}

// ---------------------------------------------------------------------------
// Gate scenario Then steps
// ---------------------------------------------------------------------------

fn is_due(plan: &ReminderPlan, name: &str, milestone: Milestone) -> bool {
    let (id, _) = actor(name);
    plan.due
        .iter()
        .any(|r| r.id == id && r.milestone == milestone)
}

fn any_due(plan: &ReminderPlan, name: &str) -> bool {
    let (id, _) = actor(name);
    plan.due.iter().any(|r| r.id == id)
}

#[then(regex = r"^Tails is due the lapse notice$")]
async fn tails_due_lapse(world: &mut RemindersWorld) {
    let plan = world.plan_result.as_ref().expect("plan not run");
    assert!(
        is_due(plan, "Tails", Milestone::Lapse),
        "expected Tails to be due Lapse; plan: {:?}",
        plan
    );
}

#[then(regex = r"^Tails is due no reminder$")]
async fn tails_due_none(world: &mut RemindersWorld) {
    let plan = world.plan_result.as_ref().expect("plan not run");
    assert!(
        !any_due(plan, "Tails"),
        "expected Tails to be due no reminder; plan: {:?}",
        plan
    );
}

#[then(regex = r"^Amy is due no reminder$")]
async fn amy_due_none(world: &mut RemindersWorld) {
    let plan = world.plan_result.as_ref().expect("plan not run");
    assert!(
        !any_due(plan, "Amy"),
        "expected Amy to be due no reminder; plan: {:?}",
        plan
    );
}

#[then(regex = r"^Amy is due the (\w+) notice$")]
async fn amy_due_notice(world: &mut RemindersWorld, token: String) {
    let milestone = Milestone::from_token(token.as_str())
        .unwrap_or_else(|| panic!("unknown milestone {token}"));
    let plan = world.plan_result.as_ref().expect("plan not run");
    assert!(
        is_due(plan, "Amy", milestone),
        "expected Amy to be due {milestone:?}; plan: {:?}",
        plan
    );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    RemindersWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/reminders")
        .await;
}
