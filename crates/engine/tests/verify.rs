//! Behaviour suite for the verify verbs (`engine::verify::Member`).
//!
//! Cast: Sonic is the moderator; Tails is known only by handle (backfill), Knuckles a linked
//! member whose handle drifted (repair), Shadow someone we do not know (Unverified), Eggman a
//! handle claimed by a different account (conflict). The facade is a single in-memory fake the
//! world holds across a run, so each step reads the resulting roles, markers, write-backs, and
//! audit rows; the fake's audit can be made unavailable, so the suite runs offline.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use cucumber::{World as _, given, then, when};

use engine::backends::discord::Role;
use engine::store::MemberRecord;
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use engine::verify::{
    HealAction, Located, Member, MemberError, MemberRead, MemberWrite, Target, VerifyOutcome,
};

use domain::MigsStatus;

const SONIC: u64 = 1;

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

fn st_id(name: &str) -> String {
    format!("st-{}", name.to_lowercase())
}

fn role_by_name(name: &str) -> Role {
    match name {
        "Member" => Role::Member,
        "Unverified" => Role::Unverified,
        "DuesExpired" => Role::DuesExpired,
        other => panic!("unknown role {other}"),
    }
}

/// A `MemberRecord` for `name` in good standing, linked to `id` (or `None`) under `handle`.
fn record(name: &str, handle: DiscordHandle, id: Option<DiscordUserId>) -> MemberRecord {
    MemberRecord {
        st_user_id: StUserId(st_id(name)),
        discord_user_id: id,
        discord_handle: Some(handle),
        email: Email(format!("{}@b.test", name.to_lowercase())),
        full_name: Some(name.to_owned()),
        standing: Some(MigsStatus::MemberInGoodStanding),
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    }
}

/// One in-memory member facade: a seeded roster for the reads, plus a recorder for every write,
/// with failure injection for the audit and the role write. Stands in for the four backends in
/// the verb-policy tests.
#[derive(Default)]
struct FakeMembers {
    by_id: HashMap<u64, MemberRecord>,
    by_handle: HashMap<String, MemberRecord>,
    by_email: HashMap<String, Vec<MemberRecord>>,
    roles: Mutex<HashMap<u64, Vec<Role>>>,
    markers: Mutex<HashSet<u64>>,
    unlinked: Mutex<HashSet<u64>>,
    audit_rows: Mutex<Vec<serde_json::Value>>,
    stamped: Mutex<Option<DiscordUserId>>,
    stamp_note: Mutex<Option<String>>,
    deleted: Mutex<Option<DiscordUserId>>,
    /// (st_user_id -> written id (None for a handle-only update) / handle): the source
    /// write-back self_heal performs.
    pushes: Mutex<Vec<(String, Option<DiscordUserId>, DiscordHandle)>>,
    /// (st_user_id -> id/handle): the cache write-through self_heal performs.
    links: Mutex<Vec<(String, DiscordUserId, DiscordHandle)>>,
    audit_available: bool,
    assign_fails: bool,
    marker_fails: bool,
}

impl FakeMembers {
    fn new() -> Self {
        Self {
            audit_available: true,
            ..Default::default()
        }
    }

    /// Seed a record into the id/handle/email maps the reads consult.
    fn seed(&mut self, rec: MemberRecord) {
        if let Some(id) = rec.discord_user_id {
            self.by_id.insert(id.0, rec.clone());
        }
        if let Some(h) = rec.discord_handle.clone() {
            self.by_handle.insert(h.0, rec.clone());
        }
        self.by_email
            .entry(rec.email.as_str().to_owned())
            .or_default()
            .push(rec);
    }

    fn seed_roles(&mut self, id: DiscordUserId, held: Vec<Role>) {
        self.roles.get_mut().unwrap().insert(id.0, held);
    }

    fn roles_of(&self, id: DiscordUserId) -> Vec<Role> {
        self.roles
            .lock()
            .unwrap()
            .get(&id.0)
            .cloned()
            .unwrap_or_default()
    }

    fn has_marker(&self, id: DiscordUserId) -> bool {
        self.markers.lock().unwrap().contains(&id.0)
    }

    /// Total source + cache write-backs - the analogue of the old `st.writes()` plus cache count.
    fn write_backs(&self) -> usize {
        self.pushes.lock().unwrap().len() + self.links.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl MemberRead for FakeMembers {
    async fn lookup(
        &self,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<Located, MemberError> {
        if let Some(r) = self.by_id.get(&id.0) {
            return Ok(Located::ById(r.clone()));
        }
        Ok(match self.by_handle.get(&handle.0) {
            Some(r) => Located::ByHandle(r.clone()),
            None => Located::Unknown,
        })
    }

    async fn find_by_email(&self, email: &Email) -> Result<Vec<MemberRecord>, MemberError> {
        Ok(self
            .by_email
            .get(email.as_str())
            .cloned()
            .unwrap_or_default())
    }

    async fn held_roles(&self, id: DiscordUserId) -> Result<Vec<Role>, MemberError> {
        Ok(self.roles_of(id))
    }
}

#[async_trait::async_trait]
impl MemberWrite for FakeMembers {
    async fn assign_role(&self, id: DiscordUserId, role: Role) -> Result<(), MemberError> {
        if self.assign_fails {
            return Err(MemberError::Discord("assign failing".into()));
        }
        // Model "set to exactly this role": the strip-stale dance is DataStore's concern.
        self.roles.lock().unwrap().insert(id.0, vec![role]);
        Ok(())
    }

    async fn strip_roles(&self, id: DiscordUserId, roles: &[Role]) -> Result<(), MemberError> {
        // Honor the slice: remove exactly the roles passed, leaving any others, so a verb that
        // strips the wrong set is caught rather than masked.
        if let Some(held) = self.roles.lock().unwrap().get_mut(&id.0) {
            held.retain(|r| !roles.contains(r));
        }
        Ok(())
    }

    async fn unlink(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.unlinked.lock().unwrap().insert(id.0);
        Ok(())
    }

    async fn stamp_override(
        &self,
        target: DiscordUserId,
        _approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), MemberError> {
        let mut stamped = self.stamped.lock().unwrap();
        if stamped.is_none() {
            *stamped = Some(target);
            *self.stamp_note.lock().unwrap() = note;
        }
        Ok(())
    }

    async fn delete_override(&self, target: DiscordUserId) -> Result<(), MemberError> {
        *self.deleted.lock().unwrap() = Some(target);
        Ok(())
    }

    async fn set_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError> {
        if self.marker_fails {
            return Err(MemberError::Discord("marker write failing".into()));
        }
        self.markers.lock().unwrap().insert(id.0);
        Ok(())
    }

    async fn clear_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.markers.lock().unwrap().remove(&id.0);
        Ok(())
    }

    async fn record(
        &self,
        _actor: DiscordUserId,
        _subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), MemberError> {
        if !self.audit_available {
            return Err(MemberError::Audit("audit unavailable".into()));
        }
        let mut row = detail;
        if let Some(obj) = row.as_object_mut() {
            obj.insert("action".to_string(), serde_json::json!(action));
        }
        self.audit_rows.lock().unwrap().push(row);
        Ok(())
    }

    async fn push_identity(
        &self,
        st: &StUserId,
        heal: &HealAction,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError> {
        // Record the resulting identity the source learns, by heal kind. UpdateHandle writes
        // only the handle (no id); BackfillId writes the full identity.
        let written = match heal {
            HealAction::UpdateHandle(h) => (st.as_str().to_owned(), None, h.clone()),
            HealAction::BackfillId(bid) => (st.as_str().to_owned(), Some(*bid), handle.clone()),
            HealAction::None => return Ok(()),
        };
        self.pushes.lock().unwrap().push(written);
        Ok(())
    }

    async fn link_cache(
        &self,
        st: &StUserId,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError> {
        self.links
            .lock()
            .unwrap()
            .push((st.as_str().to_owned(), id, handle.clone()));
        Ok(())
    }
}

impl engine::verify::Heal for FakeMembers {}

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct VerifyWorld {
    members: Vec<MemberRecord>,
    held_roles: Vec<Role>,
    audit_available: bool,
    discord_fails: bool,
    marker_fails: bool,
    last: Option<Result<VerifyOutcome, MemberError>>,
    forget_result: Option<Result<(), MemberError>>,
    last_target: Option<DiscordUserId>,
    fake: Option<FakeMembers>,
}

impl std::fmt::Debug for VerifyWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyWorld")
            .field("members", &self.members.len())
            .field("held_roles", &self.held_roles)
            .finish_non_exhaustive()
    }
}

impl VerifyWorld {
    async fn new() -> Self {
        Self {
            members: Vec::new(),
            held_roles: Vec::new(),
            audit_available: true,
            discord_fails: false,
            marker_fails: false,
            last: None,
            forget_result: None,
            last_target: None,
            fake: None,
        }
    }

    /// Build and seed a fake from the accumulated givens, targeting `target`.
    fn build_fake(&self, target: DiscordUserId) -> FakeMembers {
        let mut fake = FakeMembers::new();
        for m in &self.members {
            fake.seed(m.clone());
        }
        fake.seed_roles(target, self.held_roles.clone());
        fake.audit_available = self.audit_available;
        fake.assign_fails = self.discord_fails;
        fake.marker_fails = self.marker_fails;
        fake
    }

    async fn run(&mut self, target: DiscordUserId, handle: DiscordHandle) {
        self.last_target = Some(target);
        let fake = self.build_fake(target);
        let result = Member::new(&fake, Target { id: target, handle })
            .verify(DiscordUserId(SONIC))
            .await;
        self.last = Some(result);
        self.fake = Some(fake);
    }

    async fn run_by_email(&mut self, target: DiscordUserId, handle: DiscordHandle, email: Email) {
        self.last_target = Some(target);
        let fake = self.build_fake(target);
        let result = Member::new(&fake, Target { id: target, handle })
            .verify_by_email(DiscordUserId(SONIC), email)
            .await;
        self.last = Some(result);
        self.fake = Some(fake);
    }

    async fn run_override(
        &mut self,
        target: DiscordUserId,
        handle: DiscordHandle,
        note: Option<String>,
    ) {
        self.last_target = Some(target);
        let fake = self.build_fake(target);
        Member::new(&fake, Target { id: target, handle })
            .override_approve(DiscordUserId(SONIC), note)
            .await
            .expect("override_approve should succeed in the override scenario");
        self.fake = Some(fake);
    }

    async fn run_forget(&mut self, target: DiscordUserId, handle: DiscordHandle) {
        self.last_target = Some(target);
        let fake = self.build_fake(target);
        self.forget_result = Some(
            Member::new(&fake, Target { id: target, handle })
                .forget(DiscordUserId(SONIC))
                .await,
        );
        self.fake = Some(fake);
    }
}

#[given(regex = r"^(\w+) is in our records by handle with no Discord id$")]
async fn handle_only(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    world.members.push(record(&name, handle, None));
}

#[given(regex = r"^(\w+) is in our records linked to his Discord id, under an old handle$")]
async fn linked_old_handle(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    world
        .members
        .push(record(&name, DiscordHandle("old-handle".into()), Some(id)));
}

#[given(regex = r"^(\w+) is not in our records$")]
async fn absent(_world: &mut VerifyWorld, _name: String) {}

#[given(regex = r"^(\w+)'s handle is on record for a different account$")]
async fn handle_conflict(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    world
        .members
        .push(record(&name, handle, Some(DiscordUserId(999))));
}

#[given("the audit log is unavailable")]
async fn audit_unavailable(world: &mut VerifyWorld) {
    world.audit_available = false;
}

#[given(regex = r"^(\w+) also holds the (\w+) role$")]
async fn also_holds(world: &mut VerifyWorld, _name: String, role: String) {
    world.held_roles.push(role_by_name(&role));
}

#[given("assigning roles is failing")]
async fn role_writes_failing(world: &mut VerifyWorld) {
    world.discord_fails = true;
}

#[given("the override marker write is failing")]
async fn marker_writes_failing(world: &mut VerifyWorld) {
    world.marker_fails = true;
}

#[given(regex = r"^(\w+) was hand-approved by override$")]
async fn was_overridden(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.members.push(record(&name, handle, Some(id)));
    world.held_roles.push(Role::Member);
}

#[when(regex = r"^Sonic verifies (\w+)$")]
async fn sonic_verifies(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.run(id, handle).await;
}

#[when(regex = r"^Sonic verifies (\w+) by email$")]
async fn sonic_verifies_by_email(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    let email = Email(format!("{}@b.test", name.to_lowercase()));
    world.run_by_email(id, handle, email).await;
}

#[when(regex = r"^Sonic overrides (\w+)$")]
async fn sonic_overrides(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.run_override(id, handle, None).await;
}

#[when(regex = r#"^Sonic overrides (\w+) with the reason "([^"]*)"$"#)]
async fn sonic_overrides_with_reason(world: &mut VerifyWorld, name: String, reason: String) {
    let (id, handle) = actor(&name);
    world.run_override(id, handle, Some(reason)).await;
}

#[when(regex = r"^Sonic forgets (\w+)$")]
async fn sonic_forgets(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.run_forget(id, handle).await;
    assert!(matches!(world.forget_result, Some(Ok(()))));
}

#[then(regex = r"^(\w+) is assigned the (\w+) role$")]
async fn assigned_the_role(world: &mut VerifyWorld, name: String, role: String) {
    let (id, _) = actor(&name);
    assert!(
        world
            .fake
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
    let pushes = world.fake.as_ref().unwrap().pushes.lock().unwrap();
    assert!(
        pushes
            .iter()
            .any(|(st, wid, _)| *st == st_id(&name) && *wid == Some(id)),
        "{name}'s id was written back to the source"
    );
}

#[then(regex = r"^(\w+)'s handle is written back to our records$")]
async fn handle_written(world: &mut VerifyWorld, name: String) {
    let (_, handle) = actor(&name);
    let pushes = world.fake.as_ref().unwrap().pushes.lock().unwrap();
    assert!(
        pushes
            .iter()
            .any(|(st, _, h)| *st == st_id(&name) && *h == handle),
        "{name}'s handle was written back to the source"
    );
}

#[then(regex = r"^the (\w+) and (\w+) roles are stripped from (\w+)$")]
async fn roles_stripped(world: &mut VerifyWorld, first: String, second: String, name: String) {
    let (id, _) = actor(&name);
    let (stale_a, stale_b) = (role_by_name(&first), role_by_name(&second));
    let held = world.fake.as_ref().unwrap().roles_of(id);
    assert!(!held.contains(&stale_a), "{first} stripped");
    assert!(!held.contains(&stale_b), "{second} stripped");
    assert!(
        held.iter().any(|r| *r != stale_a && *r != stale_b),
        "the granted role must survive the strip"
    );
}

#[then("nothing is written back to our records")]
async fn nothing_written(world: &mut VerifyWorld) {
    assert_eq!(
        world.fake.as_ref().unwrap().write_backs(),
        0,
        "no source or cache write-back"
    );
}

#[then("the verification is refused")]
async fn refused(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Ok(VerifyOutcome::Conflict))));
    let target = world.last_target.expect("a verify run recorded the target");
    assert!(
        world.fake.as_ref().unwrap().roles_of(target).is_empty(),
        "a refused verification grants no role"
    );
}

#[then("the verification is recorded in the audit log")]
async fn recorded(world: &mut VerifyWorld) {
    assert_eq!(
        world
            .fake
            .as_ref()
            .unwrap()
            .audit_rows
            .lock()
            .unwrap()
            .len(),
        1
    );
}

#[then("the conflict is recorded in the audit log")]
async fn conflict_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["outcome"], "conflict");
}

#[then(regex = r"^(\w+) is not assigned any role$")]
async fn not_assigned(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(world.fake.as_ref().unwrap().roles_of(id).is_empty());
    assert!(matches!(world.last, Some(Err(MemberError::Audit(_)))));
}

#[then("the verification fails with an error")]
async fn fails_with_error(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Err(MemberError::Discord(_)))));
}

#[then("the audit log records the attempt and its failure")]
async fn attempt_and_failure_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["outcome"], "verified");
    assert_eq!(rows[1]["outcome"], "verify_failed");
}

#[then(regex = r"^the email lookup finds no record$")]
async fn email_not_found(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Ok(VerifyOutcome::NotFound))));
    let target = world.last_target.expect("a verify run recorded the target");
    assert!(
        world.fake.as_ref().unwrap().roles_of(target).is_empty(),
        "not-found grants no role"
    );
    assert_eq!(
        world.fake.as_ref().unwrap().write_backs(),
        0,
        "not-found assigns nothing, so no write-back"
    );
}

#[then(regex = r"^the not-found lookup is recorded in the audit log$")]
async fn not_found_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["outcome"], "not_found");
    assert_eq!(rows[0]["method"], "email");
}

#[then("the email verification is recorded with method email")]
async fn email_verification_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["method"], "email");
    assert_eq!(rows[0]["outcome"], "verified");
}

#[then(regex = r#"^the approval stamp records the reason "([^"]*)"$"#)]
async fn stamp_records_reason(world: &mut VerifyWorld, reason: String) {
    assert_eq!(
        world
            .fake
            .as_ref()
            .unwrap()
            .stamp_note
            .lock()
            .unwrap()
            .as_deref(),
        Some(reason.as_str())
    );
}

#[then(regex = r"^the override marker is assigned to (\w+)$")]
async fn override_marker_assigned(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(world.fake.as_ref().unwrap().has_marker(id));
}

#[then(regex = r"^the override marker is not assigned to (\w+)$")]
async fn override_marker_not_assigned(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(!world.fake.as_ref().unwrap().has_marker(id));
}

#[then("the marker failure is recorded in the audit log")]
async fn marker_failure_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["outcome"] == "override_marker_failed"),
        "expected an override_marker_failed audit row; rows: {rows:?}"
    );
}

#[then("the approval stamp is recorded")]
async fn approval_stamp_recorded(world: &mut VerifyWorld) {
    assert!(
        world
            .fake
            .as_ref()
            .unwrap()
            .stamped
            .lock()
            .unwrap()
            .is_some()
    );
}

#[then("the override is recorded in the audit log with method override")]
async fn override_audit_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["method"] == "override" && r["outcome"] == "override"),
        "expected an audit row with method=override and outcome=override; rows: {rows:?}"
    );
}

#[then(regex = r"^(\w+)'s managed roles are stripped$")]
async fn forget_roles_stripped(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(
        world.fake.as_ref().unwrap().roles_of(id).is_empty(),
        "all roles stripped"
    );
}

#[then(regex = r"^(\w+)'s cache link is cleared$")]
async fn cache_cleared(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert!(
        world
            .fake
            .as_ref()
            .unwrap()
            .unlinked
            .lock()
            .unwrap()
            .contains(&id.0)
    );
}

#[then(regex = r"^(\w+)'s override stamp is deleted$")]
async fn stamp_deleted(world: &mut VerifyWorld, name: String) {
    let (id, _) = actor(&name);
    assert_eq!(
        *world.fake.as_ref().unwrap().deleted.lock().unwrap(),
        Some(id)
    );
}

#[then("the reset is recorded in the audit log")]
async fn forget_recorded(world: &mut VerifyWorld) {
    let rows = world.fake.as_ref().unwrap().audit_rows.lock().unwrap();
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
