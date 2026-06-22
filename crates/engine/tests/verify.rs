//! Behaviour suite for the verify orchestrator (`engine::verify::verify`).
//!
//! Cast: Sonic is the moderator; Tails is known only by handle (backfill), Knuckles a
//! linked member whose handle drifted (repair), Shadow someone we do not know
//! (Unverified), Eggman a handle claimed by a different account (conflict). The roster
//! is an in-memory store; Solidarity Tech and Discord are state-based fakes the world
//! holds across a run, so each step reads the resulting roles, markers, and write-backs;
//! the audit log is a recording double that can be made unavailable - so the suite runs
//! offline.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use cucumber::{World as _, given, then, when};

use engine::backends::discord::{DiscordOp, FakeDiscord, Role};
use engine::backends::solidarity_tech::{FakeSolidarityTech, SolidarityTechMember};
use engine::store::{IdentityWrite, InMemoryStore, Index, MemberRecord, MemberStore, OverrideLog};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use engine::verify::{
    VerifyError, VerifyOutcome, forget_member, override_approve, verify, verify_by_email,
};

use domain::MigsStatus;

const SONIC: u64 = 1; // the moderator running /verify

/// The fixed id and current handle for each target.
fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Tails" => 2,
        "Knuckles" => 3,
        "Shadow" => 4,
        "Eggman" => 5,
        "Silver" => 6,
        other => panic!("unknown actor {other}"),
    };
    (DiscordUserId(raw), DiscordHandle(name.to_lowercase()))
}

fn member(name: &str, handle: DiscordHandle, id: Option<DiscordUserId>) -> SolidarityTechMember {
    SolidarityTechMember {
        id: StUserId(format!("st-{}", name.to_lowercase())),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        first_name: Some(name.to_owned()),
        discord_handle: Some(handle),
        discord_user_id: id,
        membership_standing: Some(MigsStatus::MemberInGoodStanding),
        ..Default::default()
    }
}

/// A recording audit sink that can be made unavailable to exercise the fail-closed path.
#[derive(Debug, Clone)]
struct CapturingAudit {
    available: bool,
    rows: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[derive(Debug, thiserror::Error)]
#[error("audit unavailable")]
struct AuditUnavailable;

#[async_trait::async_trait]
impl engine::audit::AuditLog for CapturingAudit {
    type Error = AuditUnavailable;
    async fn record(
        &self,
        _actor: DiscordUserId,
        _subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), AuditUnavailable> {
        if !self.available {
            return Err(AuditUnavailable);
        }
        let mut row = detail;
        if let Some(obj) = row.as_object_mut() {
            obj.insert("action".to_string(), serde_json::json!(action));
        }
        self.rows.lock().unwrap().push(row);
        Ok(())
    }
}

/// A recording override-log double that captures stamp calls and enforces insert-once,
/// and captures delete calls.
#[derive(Debug, Clone)]
struct CapturingOverrides {
    /// The subject whose stamp was recorded, if any.
    stamped: Arc<Mutex<Option<DiscordUserId>>>,
    /// The note recorded with the first stamp, if any.
    note: Arc<Mutex<Option<String>>>,
    /// The subject whose override was deleted, if any.
    deleted: Arc<Mutex<Option<DiscordUserId>>>,
}

#[async_trait::async_trait]
impl OverrideLog for CapturingOverrides {
    type Error = std::convert::Infallible;

    async fn stamp_override(
        &self,
        subject: DiscordUserId,
        _approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), std::convert::Infallible> {
        // Insert-once: only record the first stamp (and its note).
        let mut guard = self.stamped.lock().unwrap();
        if guard.is_none() {
            *guard = Some(subject);
            *self.note.lock().unwrap() = note;
        }
        Ok(())
    }

    async fn get_override(
        &self,
        _subject: DiscordUserId,
    ) -> Result<Option<engine::store::OverrideRecord>, std::convert::Infallible> {
        Ok(None)
    }

    async fn delete_override(
        &self,
        subject: DiscordUserId,
    ) -> Result<(), std::convert::Infallible> {
        *self.deleted.lock().unwrap() = Some(subject);
        Ok(())
    }
}

fn role_by_name(name: &str) -> Role {
    match name {
        "Member" => Role::Member,
        "Unverified" => Role::Unverified,
        "DuesExpired" => Role::DuesExpired,
        other => panic!("unknown role {other}"),
    }
}

fn st_id(name: &str) -> String {
    format!("st-{}", name.to_lowercase())
}

/// Wraps the in-memory store to count identity write-throughs, so the fail-closed
/// scenario can assert nothing reached the cache either - not only that no Solidarity Tech
/// write ran. Reads delegate unchanged.
struct CountingStore {
    inner: InMemoryStore,
    links: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl MemberStore for CountingStore {
    type Error = Infallible;
    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Infallible> {
        self.inner.by_discord_id(id).await
    }
    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Infallible> {
        self.inner.by_handle(handle).await
    }
}

#[async_trait::async_trait]
impl IdentityWrite for CountingStore {
    type Error = Infallible;
    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Infallible> {
        *self.links.lock().unwrap() += 1;
        self.inner
            .link_identity(st_user_id, discord_id, handle)
            .await
    }

    async fn unlink_by_discord_id(&self, discord_id: DiscordUserId) -> Result<(), Infallible> {
        self.inner.unlink_by_discord_id(discord_id).await
    }
}

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct VerifyWorld {
    members: Vec<SolidarityTechMember>,
    audit: CapturingAudit,
    /// Managed roles the target already holds when verify runs (the Discord fake returns
    /// these from `member_roles`). Empty unless a scenario stacks stale roles.
    held_roles: Vec<Role>,
    /// How many cache write-throughs (`link_identity`) ran, captured from [`CountingStore`].
    cache_writes: Arc<Mutex<usize>>,
    /// When set, the Discord fake's `set_role` returns an error, to exercise the failed
    /// role-write path.
    discord_fails: bool,
    /// The override-log double, capturing stamp calls for the override scenario.
    overrides: CapturingOverrides,
    last: Option<Result<VerifyOutcome, VerifyError>>,
    /// The result of `forget_member` (returns `()`, not a `VerifyOutcome`).
    forget_result: Option<Result<(), VerifyError>>,
    /// Whether the cache link was cleared (the member is no longer found by id).
    unlink_cleared: bool,
    /// The Discord fake after a run, for asserting resulting roles and markers.
    discord: Option<FakeDiscord>,
    /// The Solidarity Tech fake after a run, for asserting identity write-backs.
    st: Option<FakeSolidarityTech>,
}

impl VerifyWorld {
    async fn new() -> Self {
        Self {
            members: Vec::new(),
            audit: CapturingAudit {
                available: true,
                rows: Arc::new(Mutex::new(Vec::new())),
            },
            held_roles: Vec::new(),
            cache_writes: Arc::new(Mutex::new(0)),
            discord_fails: false,
            overrides: CapturingOverrides {
                stamped: Arc::new(Mutex::new(None)),
                note: Arc::new(Mutex::new(None)),
                deleted: Arc::new(Mutex::new(None)),
            },
            last: None,
            forget_result: None,
            unlink_cleared: false,
            discord: None,
            st: None,
        }
    }

    async fn run(&mut self, target: DiscordUserId, handle: DiscordHandle) {
        let store = CountingStore {
            inner: InMemoryStore::new(Index::build(self.members.clone())),
            links: self.cache_writes.clone(),
        };
        let mut discord = FakeDiscord::new().with_roles(target, self.held_roles.clone());
        if self.discord_fails {
            discord = discord.failing(DiscordOp::SetRole);
        }
        let st = FakeSolidarityTech::new().with_members(self.members.clone());

        self.last = Some(
            verify(
                &st,
                &discord,
                &store,
                &self.audit,
                DiscordUserId(SONIC),
                target,
                handle,
            )
            .await,
        );
        self.discord = Some(discord);
        self.st = Some(st);
    }

    async fn run_by_email(&mut self, target: DiscordUserId, handle: DiscordHandle, email: Email) {
        let store = CountingStore {
            inner: InMemoryStore::new(Index::build(self.members.clone())),
            links: self.cache_writes.clone(),
        };
        let mut discord = FakeDiscord::new().with_roles(target, self.held_roles.clone());
        if self.discord_fails {
            discord = discord.failing(DiscordOp::SetRole);
        }
        let st = FakeSolidarityTech::new().with_members(self.members.clone());

        self.last = Some(
            verify_by_email(
                &st,
                &discord,
                &store,
                &self.audit,
                DiscordUserId(SONIC),
                target,
                handle,
                email,
            )
            .await,
        );
        self.discord = Some(discord);
        self.st = Some(st);
    }

    async fn run_override(&mut self, target: DiscordUserId, note: Option<String>) {
        let discord = FakeDiscord::new().with_roles(target, self.held_roles.clone());

        override_approve(
            &discord,
            &self.overrides,
            &self.audit,
            DiscordUserId(SONIC),
            target,
            note,
        )
        .await
        .expect("override_approve should succeed in the override scenario");

        self.discord = Some(discord);
    }

    async fn run_forget(&mut self, target: DiscordUserId) {
        let store = InMemoryStore::new(Index::build(self.members.clone()));
        let discord = FakeDiscord::new().with_roles(target, self.held_roles.clone());

        // Pre-stamp so the delete is observable.
        self.overrides
            .stamp_override(target, DiscordUserId(SONIC), None)
            .await
            .unwrap();

        self.forget_result = Some(
            forget_member(
                &discord,
                &store,
                &self.overrides,
                &self.audit,
                DiscordUserId(SONIC),
                target,
            )
            .await,
        );
        self.unlink_cleared = store.by_discord_id(target).await.unwrap().is_none();
        self.discord = Some(discord);
    }
}

#[given(regex = r"^(\w+) is in our records by handle with no Discord id$")]
async fn handle_only(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    world.members.push(member(&name, handle, None));
}

#[given(regex = r"^(\w+) is in our records linked to his Discord id, under an old handle$")]
async fn linked_old_handle(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    world
        .members
        .push(member(&name, DiscordHandle("old-handle".into()), Some(id)));
}

#[given(regex = r"^(\w+) is not in our records$")]
async fn absent(_world: &mut VerifyWorld, _name: String) {}

#[given(regex = r"^(\w+)'s handle is on record for a different account$")]
async fn handle_conflict(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    // The handle is on record, but bound to id 999 - a different account.
    world
        .members
        .push(member(&name, handle, Some(DiscordUserId(999))));
}

#[given("the audit log is unavailable")]
async fn audit_unavailable(world: &mut VerifyWorld) {
    world.audit.available = false;
}

#[given(regex = r"^(\w+) also holds the (\w+) role$")]
async fn also_holds(world: &mut VerifyWorld, _name: String, role: String) {
    world.held_roles.push(role_by_name(&role));
}

#[given("assigning roles is failing")]
async fn role_writes_failing(world: &mut VerifyWorld) {
    world.discord_fails = true;
}

#[when(regex = r"^Sonic verifies (\w+)$")]
async fn sonic_verifies(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.run(id, handle).await;
}

#[then(regex = r"^(\w+) is assigned the (\w+) role$")]
async fn assigned_the_role(world: &mut VerifyWorld, name: String, role: String) {
    let (id, _) = actor(&name);
    assert!(
        world
            .discord
            .as_ref()
            .unwrap()
            .roles_of(id)
            .contains(&role_by_name(&role)),
        "{name} should hold {role}"
    );
}

#[then(regex = r"^(\w+)'s Discord identity is written back to our records$")]
async fn identity_written(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    let m = world
        .st
        .as_ref()
        .unwrap()
        .get(&st_id(&name))
        .expect("member exists");
    assert_eq!(m.discord_user_id, Some(id), "{name}'s id was written back");
}

#[then(regex = r"^(\w+)'s handle is written back to our records$")]
async fn handle_written(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    let m = world
        .st
        .as_ref()
        .unwrap()
        .get(&st_id(&name))
        .expect("member exists");
    assert_eq!(
        m.discord_handle,
        Some(handle),
        "{name}'s handle was written back"
    );
}

#[then(regex = r"^the (\w+) and (\w+) roles are stripped from (\w+)$")]
async fn roles_stripped(world: &mut VerifyWorld, first: String, second: String, name: String) {
    let (id, _) = actor(&name);
    let held = world.discord.as_ref().unwrap().roles_of(id);
    assert!(!held.contains(&role_by_name(&first)), "{first} stripped");
    assert!(!held.contains(&role_by_name(&second)), "{second} stripped");
}

#[then("nothing is written back to our records")]
async fn nothing_written(world: &mut VerifyWorld) {
    // Both write paths must stay untouched: the Solidarity Tech self-heal and the
    // cache write-through. Asserting only one would let a leak through the other.
    assert_eq!(
        world.st.as_ref().unwrap().writes(),
        0,
        "no Solidarity Tech write"
    );
    assert_eq!(*world.cache_writes.lock().unwrap(), 0, "no cache write");
}

#[then("the verification is refused")]
async fn refused(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Ok(VerifyOutcome::Conflict))));
    // A conflict assigns nothing; the conflicting target holds no managed role.
    assert!(
        world
            .discord
            .as_ref()
            .is_none_or(|d| d.roles_of(actor("Eggman").0).is_empty())
    );
}

#[then("the verification is recorded in the audit log")]
async fn recorded(world: &mut VerifyWorld) {
    assert_eq!(world.audit.rows.lock().unwrap().len(), 1);
}

#[then("the conflict is recorded in the audit log")]
async fn conflict_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["outcome"], "conflict");
}

#[then(regex = r"^(\w+) is not assigned any role$")]
async fn not_assigned(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(world.discord.as_ref().unwrap().roles_of(id).is_empty());
    assert!(matches!(world.last, Some(Err(VerifyError::Audit(_)))));
}

#[then("the verification fails with an error")]
async fn fails_with_error(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Err(VerifyError::Discord(_)))));
}

#[then("the audit log records the attempt and its failure")]
async fn attempt_and_failure_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    // The pre-write success row, then the reconciling failure follow-up.
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["outcome"], "verified");
    assert_eq!(rows[1]["outcome"], "verify_failed");
}

#[when(regex = r"^Sonic verifies (\w+) by email$")]
async fn sonic_verifies_by_email(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    let email = Email(format!("{}@b.test", name.to_lowercase()));
    world.run_by_email(id, handle, email).await;
}

#[then(regex = r"^the email lookup finds no record$")]
async fn email_not_found(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Ok(VerifyOutcome::NotFound))));
    // Independent check: the not-found path performs no write-back. `find_by_email` is a
    // read and does not bump `writes()`, so a stray write here would trip this.
    assert_eq!(
        world.st.as_ref().unwrap().writes(),
        0,
        "not-found assigns nothing, so no ST write-back"
    );
}

#[then(regex = r"^the not-found lookup is recorded in the audit log$")]
async fn not_found_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["outcome"], "not_found");
    assert_eq!(rows[0]["method"], "email");
}

#[then("the email verification is recorded with method email")]
async fn email_verification_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["method"], "email");
    assert_eq!(rows[0]["outcome"], "verified");
}

#[when(regex = r"^Sonic overrides (\w+)$")]
async fn sonic_overrides(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    world.run_override(id, None).await;
}

#[when(regex = r#"^Sonic overrides (\w+) with the reason "([^"]*)"$"#)]
async fn sonic_overrides_with_reason(world: &mut VerifyWorld, name: String, reason: String) {
    let (id, _) = actor(&name);
    world.run_override(id, Some(reason)).await;
}

#[then(regex = r#"^the approval stamp records the reason "([^"]*)"$"#)]
async fn stamp_records_reason(world: &mut VerifyWorld, reason: String) {
    assert_eq!(
        world.overrides.note.lock().unwrap().as_deref(),
        Some(reason.as_str())
    );
}

#[then(regex = r"^the override marker is assigned to (\w+)$")]
async fn override_marker_assigned(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(world.discord.as_ref().unwrap().has_marker(id));
}

#[then("the approval stamp is recorded")]
async fn approval_stamp_recorded(world: &mut VerifyWorld) {
    assert!(world.overrides.stamped.lock().unwrap().is_some());
}

#[then("the override is recorded in the audit log with method override")]
async fn override_audit_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["method"] == "override" && r["outcome"] == "override"),
        "expected an audit row with method=override and outcome=override; rows: {rows:?}"
    );
}

#[given(regex = r"^(\w+) was hand-approved by override$")]
async fn was_overridden(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.members.push(member(&name, handle, Some(id)));
    world.held_roles.push(Role::Member);
}

#[when(regex = r"^Sonic forgets (\w+)$")]
async fn sonic_forgets(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    world.run_forget(id).await;
    assert!(matches!(world.forget_result, Some(Ok(()))));
}

#[then(regex = r"^(\w+)'s managed roles are stripped$")]
async fn forget_roles_stripped(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(
        world.discord.as_ref().unwrap().roles_of(id).is_empty(),
        "all roles stripped"
    );
}

#[then(regex = r"^(\w+)'s cache link is cleared$")]
async fn cache_cleared(world: &mut VerifyWorld, _name: String) {
    assert!(world.unlink_cleared);
}

#[then(regex = r"^(\w+)'s override stamp is deleted$")]
async fn stamp_deleted(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert_eq!(*world.overrides.deleted.lock().unwrap(), Some(id));
}

#[then("the reset is recorded in the audit log")]
async fn forget_recorded(world: &mut VerifyWorld) {
    let rows = world.audit.rows.lock().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["action"] == "member_forget" && r["cache_unlinked"] == true)
    );
}

#[tokio::main]
async fn main() {
    VerifyWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/verify")
        .await;
}
