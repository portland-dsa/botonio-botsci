//! Behaviour suite for the verify orchestrator (`engine::verify::verify`).
//!
//! Cast: Sonic is the moderator; Tails is known only by handle (backfill), Knuckles a
//! linked member whose handle drifted (repair), Shadow someone we do not know
//! (Unverified), Eggman a handle claimed by a different account (conflict). The roster
//! is an in-memory store; Solidarity Tech and Discord are mocks wired to capture cells;
//! the audit log is a recording double that can be made unavailable - so the suite runs
//! offline.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use cucumber::{World as _, given, then, when};

use engine::backends::discord::{MemberRoles, MockDiscordClient, Role};
use engine::backends::solidarity_tech::{MockSolidarityTechClient, SolidarityTechMember};
use engine::store::{InMemoryStore, Index};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use engine::verify::{VerifyError, VerifyOutcome, verify};

use domain::MigsStatus;

/// The boxed future an `async_trait`-desugared mock method returns.
fn ready_ok<T, E>(v: T) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send>>
where
    T: Send + 'static,
    E: 'static,
{
    Box::pin(async move { Ok(v) })
}

const SONIC: u64 = 1; // the moderator running /verify

/// The fixed id and current handle for each target.
fn actor(name: &str) -> (DiscordUserId, DiscordHandle) {
    let raw = match name {
        "Tails" => 2,
        "Knuckles" => 3,
        "Shadow" => 4,
        "Eggman" => 5,
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
        _action: &str,
        detail: serde_json::Value,
    ) -> Result<(), AuditUnavailable> {
        if !self.available {
            return Err(AuditUnavailable);
        }
        self.rows.lock().unwrap().push(detail);
        Ok(())
    }
}

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct VerifyWorld {
    members: Vec<SolidarityTechMember>,
    audit: CapturingAudit,
    /// The role passed to `set_role`, captured from the Discord mock.
    assigned_role: Arc<Mutex<Option<Role>>>,
    /// Which write-backs ran ("identity"/"handle"), captured from the Solidarity Tech mock.
    st_writes: Arc<Mutex<Vec<&'static str>>>,
    last: Option<Result<VerifyOutcome, VerifyError>>,
}

impl VerifyWorld {
    async fn new() -> Self {
        Self {
            members: Vec::new(),
            audit: CapturingAudit {
                available: true,
                rows: Arc::new(Mutex::new(Vec::new())),
            },
            assigned_role: Arc::new(Mutex::new(None)),
            st_writes: Arc::new(Mutex::new(Vec::new())),
            last: None,
        }
    }

    async fn run(&mut self, target: DiscordUserId, handle: DiscordHandle) {
        let store = InMemoryStore::new(Index::build(self.members.clone()));

        let assigned = self.assigned_role.clone();
        let mut discord = MockDiscordClient::new();
        discord
            .expect_member_roles()
            .returning(|_| ready_ok(MemberRoles::default()));
        discord.expect_set_role().returning(move |_u, _cur, role| {
            *assigned.lock().unwrap() = Some(role);
            ready_ok(())
        });

        let w_identity = self.st_writes.clone();
        let w_handle = self.st_writes.clone();
        let mut st = MockSolidarityTechClient::new();
        st.expect_set_discord_identity().returning(move |_, _, _| {
            w_identity.lock().unwrap().push("identity");
            ready_ok(())
        });
        st.expect_set_discord_handle().returning(move |_, _| {
            w_handle.lock().unwrap().push("handle");
            ready_ok(())
        });

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

#[when(regex = r"^Sonic verifies (\w+)$")]
async fn sonic_verifies(world: &mut VerifyWorld, name: String) {
    let (id, handle) = actor(&name);
    world.run(id, handle).await;
}

#[then(regex = r"^(\w+) is assigned the (\w+) role$")]
async fn assigned_the_role(world: &mut VerifyWorld, _name: String, role: String) {
    let expected = match role.as_str() {
        "Member" => Role::Member,
        "Unverified" => Role::Unverified,
        "DuesExpired" => Role::DuesExpired,
        other => panic!("unknown role {other}"),
    };
    assert_eq!(*world.assigned_role.lock().unwrap(), Some(expected));
}

#[then(regex = r"^(\w+)'s Discord identity is written back to our records$")]
async fn identity_written(world: &mut VerifyWorld, _name: String) {
    assert!(world.st_writes.lock().unwrap().contains(&"identity"));
}

#[then(regex = r"^(\w+)'s handle is written back to our records$")]
async fn handle_written(world: &mut VerifyWorld, _name: String) {
    assert!(world.st_writes.lock().unwrap().contains(&"handle"));
}

#[then("nothing is written back to our records")]
async fn nothing_written(world: &mut VerifyWorld) {
    assert!(world.st_writes.lock().unwrap().is_empty());
}

#[then("the verification is refused")]
async fn refused(world: &mut VerifyWorld) {
    assert!(matches!(world.last, Some(Ok(VerifyOutcome::Conflict))));
    assert!(world.assigned_role.lock().unwrap().is_none());
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
async fn not_assigned(world: &mut VerifyWorld, _name: String) {
    assert!(world.assigned_role.lock().unwrap().is_none());
    assert!(matches!(world.last, Some(Err(VerifyError::Audit(_)))));
}

#[tokio::main]
async fn main() {
    VerifyWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/verify")
        .await;
}
