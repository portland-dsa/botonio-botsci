//! Behaviour suite for the dues-reminder planner (`engine::reminders::plan` +
//! `select_milestone`).
//!
//! Cast: Sonic is the outline protagonist (milestone selection); Tails and Amy are the
//! gate scenarios' members. Each scenario seeds one `InMemoryStore` and either calls
//! `select_milestone` directly (selection outline) or drives `plan` over the store
//! (gate scenarios).

use chrono::NaiveDate;
use cucumber::{World as _, given, then, when};

use domain::{DiscordGuildId, MigsStatus};

use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::reminders::plan as reminder_plan;
use engine::reminders::{Milestone, ReminderPlan, select_milestone};
use engine::store::{GraceStore, InMemoryStore, Index, MemberRecord, OptOutSource, ReminderStore};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The fixed guild used for all `plan` gate scenarios.
const GUILD: DiscordGuildId = DiscordGuildId(1);

/// A stable "today" for gate scenarios so xdate arithmetic is deterministic.
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
    timely: bool,
    selection_result: Option<Option<Milestone>>,

    // --- gate scenario state (seeded into InMemoryStore before `plan` runs) ---
    records: Vec<MemberRecord>,
    /// (guild, id, until, granted_by) for active graces to seed.
    graces: Vec<(DiscordUserId, NaiveDate)>,
    /// (guild, id, xdate) for snoozes to seed.
    snoozes: Vec<(DiscordUserId, NaiveDate)>,
    /// ids to opt out.
    opt_outs: Vec<DiscordUserId>,
    plan_result: Option<ReminderPlan>,
}

impl std::fmt::Debug for RemindersWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemindersWorld")
            .field("days_until", &self.days_until)
            .field("last_sent", &self.last_sent)
            .field("timely", &self.timely)
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

impl RemindersWorld {
    async fn new() -> Self {
        Self {
            days_until: 0,
            last_sent: None,
            timely: true,
            selection_result: None,
            records: Vec::new(),
            graces: Vec::new(),
            snoozes: Vec::new(),
            opt_outs: Vec::new(),
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
        for (id, cycle_xdate) in &self.snoozes {
            store.set_snooze(GUILD, *id, *cycle_xdate).await.unwrap();
        }
        for id in &self.opt_outs {
            store
                .opt_out(GUILD, *id, OptOutSource::Member)
                .await
                .unwrap();
        }

        let result = reminder_plan(&store, GUILD, today(), self.timely)
            .await
            .unwrap();
        self.plan_result = Some(result);
    }
}

// ---------------------------------------------------------------------------
// Selection outline steps
// ---------------------------------------------------------------------------

#[given(regex = r"^Sonic's membership lapses in (-?\d+) days$")]
async fn sonic_lapses_in(world: &mut RemindersWorld, days: i64) {
    world.days_until = days;
}

#[given(regex = r"^he was last sent the (\w+) reminder$")]
async fn last_sent(world: &mut RemindersWorld, milestone_str: String) {
    world.last_sent = match milestone_str.as_str() {
        "none" => None,
        "Days30" => Some(Milestone::Days30),
        "Days14" => Some(Milestone::Days14),
        "Day1" => Some(Milestone::Day1),
        "Expired" => Some(Milestone::Expired),
        other => panic!("unknown milestone token {other}"),
    };
}

#[given(regex = r"^the sweep is (timely|delayed)$")]
async fn sweep_timeliness(world: &mut RemindersWorld, mode: String) {
    world.timely = mode == "timely";
}

#[then(regex = r"^the reminder due is (\w+)$")]
async fn reminder_due_is(world: &mut RemindersWorld, milestone_str: String) {
    let result = select_milestone(world.days_until, world.last_sent, world.timely);
    world.selection_result = Some(result);
    let expected: Option<Milestone> = match milestone_str.as_str() {
        "none" => None,
        "Days30" => Some(Milestone::Days30),
        "Days14" => Some(Milestone::Days14),
        "Day1" => Some(Milestone::Day1),
        "Expired" => Some(Milestone::Expired),
        other => panic!("unknown milestone token {other}"),
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

#[given(regex = r"^Amy's membership lapses in (\d+) days$")]
async fn amy_lapses_in(world: &mut RemindersWorld, days: i64) {
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

#[given(regex = r"^Amy is snoozed for this cycle$")]
async fn amy_snoozed(world: &mut RemindersWorld) {
    let (id, _) = actor("Amy");
    // The xdate must match whatever Amy's record has.
    let xdate = world
        .records
        .iter()
        .find(|r| r.discord_user_id == Some(id))
        .and_then(|r| r.expires)
        .expect("Amy's record must be seeded before snooze");
    world.snoozes.push((id, xdate));
}

#[given(regex = r"^Tails has no expiry date$")]
async fn tails_no_expiry(world: &mut RemindersWorld) {
    world.records.push(record_with_xdate("Tails", None, true));
}

#[given(regex = r"^Tails is unlinked$")]
async fn tails_unlinked(world: &mut RemindersWorld) {
    // A record with no Discord id - unlinked.
    let xdate = today() + chrono::Duration::days(14);
    world
        .records
        .push(record_with_xdate("Tails", Some(xdate), false));
}

#[given(regex = r"^Amy's last cycle was for a different xdate$")]
async fn amy_stale_cycle(world: &mut RemindersWorld) {
    // Seed a ReminderCycleState with an old xdate so the planner treats it as reset.
    // We model this by storing a snooze for a cycle that differs from Amy's current xdate.
    // The plan logic: if `state.cycle_xdate != xdate`, reset. So we put a snooze on a
    // past xdate - that snooze is for a stale cycle and will be ignored.
    let (id, _) = actor("Amy");
    let stale_xdate = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    world.snoozes.push((id, stale_xdate));
}

// ---------------------------------------------------------------------------
// Gate scenario When steps
// ---------------------------------------------------------------------------

#[when(regex = r"^the reminder sweep is planned$")]
async fn reminder_sweep_planned(world: &mut RemindersWorld) {
    world.timely = true;
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

#[then(regex = r"^Tails is due the Expired reminder$")]
async fn tails_due_expired(world: &mut RemindersWorld) {
    let plan = world.plan_result.as_ref().expect("plan not run");
    assert!(
        is_due(plan, "Tails", Milestone::Expired),
        "expected Tails to be due Expired; plan: {:?}",
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

#[then(regex = r"^Amy is due the (\w+) reminder$")]
async fn amy_due_milestone(world: &mut RemindersWorld, milestone_str: String) {
    let milestone = match milestone_str.as_str() {
        "Days30" => Milestone::Days30,
        "Days14" => Milestone::Days14,
        "Day1" => Milestone::Day1,
        "Expired" => Milestone::Expired,
        other => panic!("unknown milestone {other}"),
    };
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
