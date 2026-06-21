// This behavior suite `#[path]`-includes `../src/lookup.rs`. Because an integration
// test compiles with `cfg(test)`, that pulls in the module's own `#[cfg(test)]`
// unit-test helpers, which are dead in this `harness = false` binary, and the suite
// asserts on `LookupOutcome` variants without reading their payloads. Allow the
// resulting dead-code / unused-import noise here only; the bin compilation of the
// same module is unaffected.
#![allow(dead_code, unused_imports)]

//! Behavior suite for the moderator lookup decision core (`src/lookup.rs`).
//!
//! Cast: **Sonic** is the moderator, **Eggman** a non-moderator snoop, **Tails** a
//! member with a record, **Shadow** a member with none. Mocks only - an in-memory
//! store, a recording audit sink, and the real rate limiter - so the suite runs on
//! a plain offline `cargo test` with no database or gateway. Scenarios live in
//! `tests/features/lookup/`.

// The bot is a binary crate with no lib target, so pull the self-contained decision
// core straight into this test binary. `#[path]` resolves against `tests/`.
#[path = "../src/lookup.rs"]
mod lookup;

use std::convert::Infallible;
use std::sync::Mutex;

use cucumber::{World as _, given, then, when};

use engine::store::{InMemoryStore, Index, MemberRecord, OverrideLog};
use engine::util::{DiscordUserId, Email, StUserId};

use lookup::{LookupOutcome, RateLimiter, lookup};

// Fixed ids for the cast.
const SONIC: u64 = 1;
const EGGMAN: u64 = 666;
const TAILS: u64 = 2;
const SHADOW: u64 = 3;
const KNUCKLES: u64 = 4;

fn id_for(name: &str) -> DiscordUserId {
    let raw = match name {
        "Sonic" => SONIC,
        "Eggman" => EGGMAN,
        "Tails" => TAILS,
        "Shadow" => SHADOW,
        "Knuckles" => KNUCKLES,
        other => panic!("unknown actor {other}"),
    };
    DiscordUserId(raw)
}

/// A minimal record for a member with the given id.
fn record_for(id: DiscordUserId) -> MemberRecord {
    MemberRecord {
        st_user_id: StUserId(format!("st-{}", id.0)),
        discord_user_id: Some(id),
        discord_handle: None,
        email: Email(format!("member-{}@b.test", id.0)),
        full_name: None,
        standing: None,
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    }
}

/// A recording audit sink: it remembers every `(actor, subject, action, detail)`.
#[derive(Default, Debug)]
struct RecordingAudit {
    rows: Mutex<Vec<(DiscordUserId, DiscordUserId, String, serde_json::Value)>>,
}

#[async_trait::async_trait]
impl engine::audit::AuditLog for RecordingAudit {
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

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct LookupWorld {
    moderators: std::collections::HashSet<u64>,
    records: Vec<MemberRecord>,
    overrides: Vec<(DiscordUserId, DiscordUserId)>,
    audit: RecordingAudit,
    last: Option<LookupOutcome>,
    last_actor: Option<DiscordUserId>,
    last_subject: Option<DiscordUserId>,
}

impl LookupWorld {
    async fn new() -> Self {
        Self {
            moderators: std::collections::HashSet::new(),
            records: Vec::new(),
            overrides: Vec::new(),
            audit: RecordingAudit::default(),
            last: None,
            last_actor: None,
            last_subject: None,
        }
    }

    /// Run one lookup of `target` by `invoker`, remembering the outcome.
    async fn run(&mut self, invoker: DiscordUserId, target: DiscordUserId) {
        let store = InMemoryStore::new(Index::from_records(self.records.clone()));
        for (subject, approver) in &self.overrides {
            store
                .stamp_override(*subject, *approver, None)
                .await
                .unwrap();
        }
        let limiter = RateLimiter::new(10);
        self.last_actor = Some(invoker);
        self.last_subject = Some(target);
        self.last = Some(
            lookup(
                &store,
                &store,
                &self.audit,
                &limiter,
                invoker,
                target,
                self.moderators.contains(&invoker.0),
            )
            .await,
        );
    }
}

#[given(regex = r"^(\w+) is a moderator$")]
async fn is_moderator(world: &mut LookupWorld, name: String) {
    world.moderators.insert(id_for(&name).0);
}

#[given(regex = r"^(\w+) is not a moderator$")]
async fn is_not_moderator(world: &mut LookupWorld, name: String) {
    world.moderators.remove(&id_for(&name).0);
}

#[given(regex = r"^(\w+) is a member with a record$")]
async fn member_with_record(world: &mut LookupWorld, name: String) {
    world.records.push(record_for(id_for(&name)));
}

#[given(regex = r"^(\w+) is a member with no record$")]
async fn member_without_record(_world: &mut LookupWorld, _name: String) {
    // No record inserted: the store simply will not find them.
}

#[when(regex = r"^(\w+) looks up (\w+)$")]
async fn looks_up(world: &mut LookupWorld, actor: String, target: String) {
    let (a, t) = (id_for(&actor), id_for(&target));
    world.run(a, t).await;
}

#[when(regex = r"^(\w+) looks up (\w+) 11 times$")]
async fn looks_up_eleven(world: &mut LookupWorld, actor: String, target: String) {
    let (a, t) = (id_for(&actor), id_for(&target));
    // One store/limiter for the whole burst, so the eleventh trips the ceiling.
    let store = InMemoryStore::new(Index::from_records(world.records.clone()));
    let limiter = RateLimiter::new(10);
    let is_mod = world.moderators.contains(&a.0);
    world.last_actor = Some(a);
    world.last_subject = Some(t);
    let mut last = None;
    for _ in 0..11 {
        last = Some(lookup(&store, &store, &world.audit, &limiter, a, t, is_mod).await);
    }
    world.last = last;
}

#[then("the card is shown")]
async fn card_shown(world: &mut LookupWorld) {
    assert!(matches!(
        world.last,
        Some(LookupOutcome::Card(_)) | Some(LookupOutcome::SelfCard(Some(_)))
    ));
}

#[then("a not-found reply is shown")]
async fn not_found(world: &mut LookupWorld) {
    assert!(matches!(world.last, Some(LookupOutcome::NotFound)));
}

#[then("the lookup is refused for lack of permission")]
async fn refused(world: &mut LookupWorld) {
    assert!(matches!(world.last, Some(LookupOutcome::NotModerator)));
}

#[then("the eleventh lookup is rate-limited")]
async fn rate_limited(world: &mut LookupWorld) {
    assert!(matches!(world.last, Some(LookupOutcome::RateLimited)));
}

#[then(regex = r#"^one audit row records the outcome "(\w+)"$"#)]
async fn one_audit_row(world: &mut LookupWorld, outcome: String) {
    let rows = world.audit.rows.lock().unwrap();
    assert_eq!(rows.len(), 1, "expected exactly one audit row");
    assert_eq!(rows[0].0, world.last_actor.unwrap());
    assert_eq!(rows[0].1, world.last_subject.unwrap());
    assert_eq!(rows[0].2, "card_lookup");
    assert_eq!(rows[0].3, serde_json::json!({ "outcome": outcome }));
}

#[then("no audit row is written")]
async fn no_audit_row(world: &mut LookupWorld) {
    assert!(world.audit.rows.lock().unwrap().is_empty());
}

#[then("ten audit rows are written")]
async fn ten_audit_rows(world: &mut LookupWorld) {
    assert_eq!(world.audit.rows.lock().unwrap().len(), 10);
}

#[given(regex = r"^(\w+) is a manually-verified member approved by (\w+)$")]
async fn manually_verified(world: &mut LookupWorld, name: String, mod_name: String) {
    world.overrides.push((id_for(&name), id_for(&mod_name)));
}

#[then("the override card is shown")]
async fn override_card_shown(world: &mut LookupWorld) {
    assert!(matches!(
        world.last,
        Some(LookupOutcome::OverrideCard(_)) | Some(LookupOutcome::SelfOverride(_))
    ));
}

#[tokio::main]
async fn main() {
    LookupWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/lookup")
        .await;
}
