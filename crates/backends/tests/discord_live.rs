#![cfg(feature = "live-discord")]
//! Live Discord behavior suite - Cucumber. Protagonist: **Vector**.
//!
//! These scenarios hit a real **test** guild through the bot token and build
//! only behind the `live-discord` feature. Run with:
//!
//! ```text
//! cargo test --features live-discord --test discord_live
//! ```
//!
//! Requires a populated `.env` (`DISCORD_BOT_TOKEN`, `DISCORD_GUILD_ID`,
//! `DISCORD_TEST_USER_ID`, and the optional `DISCORD_TEST_CHANNEL_ID`). The bot
//! must have Manage Roles + Manage Channels and its own role must sit above all
//! three status roles in the guild hierarchy. Every scenario mutates real guild
//! state, so it points at the **test** guild, never production, and the write
//! scenarios round-trip - read state, write, restore - leaving the guild as they
//! found it. Scenarios share one guild, so the runner pins
//! `max_concurrent_scenarios(1)` to keep them from overlapping.

use std::fmt;
use std::time::{Duration, Instant};

use cucumber::{World as _, given, then, when};

use backends::discord::{DiscordChannel, DiscordClient, DiscordHttp, MemberRoles, Role};
use backends::util::{DiscordChannelId, DiscordUserId, DryRun};

/// Reads a required env var, panicking with the var name if absent - a missing
/// credential should fail the live run loudly rather than silently skip.
fn require_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| panic!("required env var {key} is not set for live tests"))
}

/// The configured test user, parsed from `DISCORD_TEST_USER_ID`.
fn test_user_id() -> DiscordUserId {
    DiscordUserId(
        require_env("DISCORD_TEST_USER_ID")
            .parse()
            .expect("DISCORD_TEST_USER_ID must be a u64"),
    )
}

/// The configured throwaway test channel, parsed from `DISCORD_TEST_CHANNEL_ID`.
fn test_channel_id() -> DiscordChannelId {
    DiscordChannelId(
        require_env("DISCORD_TEST_CHANNEL_ID")
            .parse()
            .expect("DISCORD_TEST_CHANNEL_ID must be a u64"),
    )
}

/// Polls the target user's highest-priority managed status role until it matches
/// `expected`, returning the resolved status, or panics after ~20s.
///
/// This reads the single-member `get_member` endpoint (via `member_roles`), not
/// the bulk member list (`list_members`): the bulk list is eventually consistent
/// and can lag tens of seconds behind a role write, which makes any poll against
/// it flaky, whereas `get_member` reflects the change within a second or two.
/// `member_roles().held` is in `Role::ALL` priority order, so its first element
/// is the equivalent of `DiscordMember::current_status`.
async fn wait_for_status(
    client: &DiscordHttp,
    user: DiscordUserId,
    expected: Option<Role>,
) -> Option<Role> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let roles = client
            .member_roles(user)
            .await
            .expect("member_roles failed");
        let current = roles.held.first().copied();
        if current == expected {
            return current;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for status == {expected:?}; last seen {current:?}");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// The per-scenario state Vector drives: the live client (built lazily on the
/// first step that needs it) plus whatever a `when` step reads back for a `then`
/// to assert on.
#[derive(cucumber::World)]
#[world(init = Self::new)]
struct DiscordWorld {
    /// The live client, constructed on first use by [`DiscordWorld::client`].
    client: Option<DiscordHttp>,

    // Read results captured by `when` steps.
    members: Option<Vec<backends::discord::DiscordMember>>,
    roles: Option<MemberRoles>,
    channels: Option<Vec<DiscordChannel>>,
    role_ids: Option<Vec<u64>>,

    // Write round-trip bookkeeping.
    /// The status the test user held before a role write, for the `then` to
    /// confirm it was restored.
    original_status: Option<Option<Role>>,
    /// The status the test user held when a no-op scenario primed a known role.
    primed_status: Option<Role>,
}

impl DiscordWorld {
    fn new() -> Self {
        // Load `.env` once per World so the live credentials are present before
        // any step reads them. Missing required vars surface later, in the step
        // that builds the client or parses the test ids.
        dotenvy::dotenv().ok();
        let _ = tracing_subscriber::fmt::try_init();
        Self {
            client: None,
            members: None,
            roles: None,
            channels: None,
            role_ids: None,
            original_status: None,
            primed_status: None,
        }
    }

    /// The live client, built from the same env vars the suite documents on
    /// first call and reused thereafter. Panics if construction fails (a bad or
    /// missing token, or status role names absent from the test guild).
    async fn client(&mut self) -> &DiscordHttp {
        if self.client.is_none() {
            let http = DiscordHttp::from_env().await.expect(
                "DiscordHttp::from_env failed - check DISCORD_BOT_TOKEN, DISCORD_GUILD_ID, and \
                 the status role names on the test guild",
            );
            self.client = Some(http);
        }
        self.client.as_ref().expect("client just built")
    }
}

// `cucumber::World` requires `Debug`, but `DiscordHttp` holds a `SecretString`
// and is intentionally not `Debug` (the token must never be printed). Report only
// whether the client was built and elide it.
impl fmt::Debug for DiscordWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordWorld")
            .field("client_built", &self.client.is_some())
            .field("members", &self.members.as_ref().map(Vec::len))
            .field("channels", &self.channels.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

// ==============================================================================
// GIVEN - the live credentials and the designated test targets
// ==============================================================================

#[given("the live Discord credentials and a test guild")]
async fn given_guild(world: &mut DiscordWorld) {
    // Force the client to build now, so a credential problem fails the `given`.
    world.client().await;
}

#[given("the live Discord credentials and a test user")]
async fn given_user(world: &mut DiscordWorld) {
    world.client().await;
    // Parse the test user id eagerly so a missing/invalid var fails here.
    let _ = test_user_id();
}

#[given("the live Discord credentials and a test channel")]
async fn given_channel(world: &mut DiscordWorld) {
    world.client().await;
    let _ = test_channel_id();
}

#[given("the live Discord credentials and a test user already at a known status")]
async fn given_user_known_status(world: &mut DiscordWorld) {
    let user = test_user_id();
    let client = world.client().await;

    // Read the member's current status, then ensure they sit at a definite,
    // known status so the no-op scenario has something to re-apply. Set Member
    // when they have none; otherwise leave their current status in place.
    let current = client
        .list_members()
        .await
        .expect("list_members failed")
        .into_iter()
        .find(|m| m.id == user)
        .unwrap_or_else(|| panic!("DISCORD_TEST_USER_ID {user} not in test guild"))
        .current_status;

    let known = match current {
        Some(role) => role,
        None => {
            client
                .set_role(user, None, Role::Member, DryRun::LIVE)
                .await
                .expect("priming set_role failed");
            wait_for_status(client, user, Some(Role::Member)).await;
            Role::Member
        }
    };

    // Remember both what the user started as (to restore) and the known status
    // the no-op will re-apply.
    world.original_status = Some(current);
    world.primed_status = Some(known);
}

// ==============================================================================
// WHEN - Vector reads and round-trips writes
// ==============================================================================

#[when("Vector lists the guild members")]
async fn when_list_members(world: &mut DiscordWorld) {
    let members = world
        .client()
        .await
        .list_members()
        .await
        .expect("list_members failed");
    world.members = Some(members);
}

#[when("Vector reads the test user's roles")]
async fn when_read_roles(world: &mut DiscordWorld) {
    let user = test_user_id();
    let roles = world
        .client()
        .await
        .member_roles(user)
        .await
        .expect("member_roles failed");
    world.roles = Some(roles);
}

#[when("Vector lists the channels and role ids")]
async fn when_list_channels_and_roles(world: &mut DiscordWorld) {
    let client = world.client().await;
    let channels = client.list_channels().await.expect("list_channels failed");
    let role_ids = client.list_role_ids().await.expect("list_role_ids failed");
    world.channels = Some(channels);
    world.role_ids = Some(role_ids);
}

#[when("Vector sets and then restores the test user's status role")]
async fn when_round_trip_role(world: &mut DiscordWorld) {
    let user = test_user_id();
    let client = world.client().await;

    let original = client
        .list_members()
        .await
        .expect("list_members failed")
        .into_iter()
        .find(|m| m.id == user)
        .unwrap_or_else(|| panic!("DISCORD_TEST_USER_ID {user} not in test guild"))
        .current_status;

    // Flip to a different status, confirm it took, then restore the original.
    let flip_to = match original {
        Some(Role::DuesExpired) => Role::Member,
        _ => Role::DuesExpired,
    };
    client
        .set_role(user, original, flip_to, DryRun::LIVE)
        .await
        .expect("set_role flip failed");
    let after_flip = wait_for_status(client, user, Some(flip_to)).await;

    // Restore: back to the original status, or to Member if they held none
    // (the bare `set_role` API always lands on a status; restoring to "no
    // status" is the cleanup scenario's job, not this round-trip's).
    client
        .set_role(
            user,
            after_flip,
            original.unwrap_or(Role::Member),
            DryRun::LIVE,
        )
        .await
        .expect("set_role restore failed");
    wait_for_status(client, user, Some(original.unwrap_or(Role::Member))).await;

    // Record the pre-run status only after the client borrow ends, for the
    // `then` to confirm the round-trip restored it.
    world.original_status = Some(original);
}

#[when("Vector sets that same status role again")]
async fn when_set_same_role(world: &mut DiscordWorld) {
    let user = test_user_id();
    let known = world
        .primed_status
        .expect("given step did not prime a known status");
    let client = world.client().await;

    // Re-applying the status the member already holds must be a cheap no-op and
    // must not error.
    client
        .set_role(user, Some(known), known, DryRun::LIVE)
        .await
        .expect("no-op set_role returned an error");
}

// ==============================================================================
// THEN - assert on the captured reads and confirm writes round-tripped
// ==============================================================================

#[then("the members are returned with their current managed status")]
async fn then_members_returned(world: &mut DiscordWorld) {
    let members = world.members.as_ref().expect("no members were listed");
    assert!(
        !members.is_empty(),
        "test guild should have at least one member"
    );
    for m in members {
        assert!(!m.handle.as_str().is_empty(), "every member has a handle");
        assert!(
            !m.display_name.is_empty(),
            "display_name fallback should resolve"
        );
        // `current_status`, when present, must be one of the three managed roles.
        if let Some(status) = m.current_status {
            assert!(Role::ALL.contains(&status), "unexpected status {status:?}");
        }
    }
}

#[then("the test user's roles are returned")]
async fn then_roles_returned(world: &mut DiscordWorld) {
    let roles = world.roles.as_ref().expect("no roles were read");
    // Can't assert specific roles on an arbitrary test guild, but every name
    // should be non-empty (or a numeric fallback id).
    for name in &roles.all_names {
        assert!(!name.is_empty(), "role name should never be empty");
    }
    // Held managed roles must all be one of the three.
    for held in &roles.held {
        assert!(Role::ALL.contains(held), "unexpected held role {held:?}");
    }
}

#[then("the channels and role ids are returned")]
async fn then_channels_and_roles_returned(world: &mut DiscordWorld) {
    let channels = world.channels.as_ref().expect("no channels were listed");
    assert!(
        !channels.is_empty(),
        "test guild should have at least one channel"
    );
    for c in channels {
        assert!(!c.name.is_empty(), "every channel has a name");
    }
    let role_ids = world.role_ids.as_ref().expect("no role ids were listed");
    // Every guild has at least the @everyone role (id == guild id).
    assert!(!role_ids.is_empty(), "guild should have at least @everyone");
}

#[then("the role write succeeds and the test user ends as they began")]
async fn then_role_round_tripped(world: &mut DiscordWorld) {
    let user = test_user_id();
    let original = world
        .original_status
        .expect("no original status was captured");
    let client = world.client().await;
    // The restore in the `when` step targeted the original status (or Member if
    // they held none); confirm the guild ended where it began.
    let expected = Some(original.unwrap_or(Role::Member));
    let ended = wait_for_status(client, user, expected).await;
    assert_eq!(ended, expected, "test user did not end as they began");
}

#[then("no role change is made")]
async fn then_no_role_change(world: &mut DiscordWorld) {
    let user = test_user_id();
    let known = world
        .primed_status
        .expect("given step did not prime a known status");
    let original = world.original_status; // copy out before borrowing the client
    let client = world.client().await;
    // The member still holds exactly the status they were primed at.
    let current = wait_for_status(client, user, Some(known)).await;
    assert_eq!(current, Some(known), "no-op set_role changed the status");

    // Restore the user to whatever they originally held: if the given step
    // primed Member onto a previously-roleless user, strip it back to none.
    if let Some(original) = original
        && original.is_none()
        && known == Role::Member
    {
        client
            .remove_roles(user, &Role::ALL, DryRun::LIVE)
            .await
            .expect("restore remove_roles failed");
        wait_for_status(client, user, None).await;
    }
}

#[tokio::main]
async fn main() {
    // One real guild backs every scenario, so they must never overlap; pin
    // single-concurrency. `fail_on_skipped` makes a missing step a failure, not
    // a silent skip.
    DiscordWorld::cucumber()
        .max_concurrent_scenarios(1)
        .fail_on_skipped()
        .run_and_exit("tests/features/discord_live")
        .await;
}
