//! Live suite for the Solidarity Tech backend - Espio round-trips against the
//! real API. Scenarios live in `tests/features/solidarity_tech_live/`.
//!
//! This target builds only behind the `live-solidarity-tech` feature, so a bare
//! `cargo test` never compiles or runs it. The reads are safe; the single write
//! is a no-op - it stamps the member's *existing* Discord identity back onto
//! them, leaving the record unchanged - and is opt-in.
//!
//! Required env vars (supplied out of band in `.env`, never committed):
//!   SOLIDARITY_TECH_TOKEN      - ST API token with read/write on users
//!   ST_LIVE_EMAIL              - email of a stable, dedicated test member
//!   ST_LIVE_ALLOW_NOOP_WRITE   - leave unset for read-only; set to "1" to
//!                                exercise the PUT (no-op) write path
//!
//! Run with:
//!   cargo test --features live-solidarity-tech --test solidarity_tech_live

#![cfg(feature = "live-solidarity-tech")]

use std::fmt;

use cucumber::{World as _, given, then, when};

use backends::solidarity_tech::{SolidarityTechClient, SolidarityTechHttp, SolidarityTechMember};
use backends::util::{DiscordHandle, DiscordUserId, DryRun, Email};
use secrecy::SecretString;

fn require_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("required env var {key} is not set"))
}

/// Per-scenario state Espio drives against the live API: the real client, the
/// test member's email, and the records read before and after the no-op write.
#[derive(cucumber::World)]
#[world(init = Self::new)]
struct LiveWorld {
    client: SolidarityTechHttp,
    email: Option<Email>,
    /// The member read by a `given` step, carried into the write `when`.
    before: Option<SolidarityTechMember>,
    /// The identity written back, kept so a `then` can confirm it persisted.
    written: Option<(DiscordHandle, DiscordUserId)>,
    /// Set once the no-op write is sanctioned for this run.
    noop_allowed: bool,
}

impl LiveWorld {
    async fn new() -> Self {
        dotenvy::dotenv().ok();
        // The live suite must reach the real API. A developer may have
        // SOLIDARITY_TECH_BASE_URL set in .env to point the bot at the local mock;
        // ignore it and build against the real URL explicitly, so the suite is
        // resilient to that.
        if std::env::var_os("SOLIDARITY_TECH_BASE_URL").is_some() {
            tracing::warn!(
                api = SolidarityTechHttp::API_BASE_URL,
                "ignoring SOLIDARITY_TECH_BASE_URL; the live suite uses the real API"
            );
        }
        let token = SecretString::from(
            backends::from_credstore_or_env("solidarity_tech_token", "SOLIDARITY_TECH_TOKEN")
                .expect("SOLIDARITY_TECH_TOKEN is not set"),
        );
        let client =
            SolidarityTechHttp::with_base_url(SolidarityTechHttp::API_BASE_URL.into(), token);
        Self {
            client,
            email: None,
            before: None,
            written: None,
            noop_allowed: false,
        }
    }

    fn email(&self) -> &Email {
        self.email
            .as_ref()
            .expect("no ST_LIVE_EMAIL was established for this scenario")
    }
}

impl fmt::Debug for LiveWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveWorld")
            .field("email", &self.email)
            .field("has_before", &self.before.is_some())
            .field("noop_allowed", &self.noop_allowed)
            .finish_non_exhaustive()
    }
}

// ==============================================================================
// GIVEN
// ==============================================================================

#[given("the live Solidarity Tech credentials and a known member email")]
async fn credentials_and_email(world: &mut LiveWorld) {
    world.email = Some(
        require_env("ST_LIVE_EMAIL")
            .parse()
            .expect("ST_LIVE_EMAIL is not a valid email"),
    );
}

#[given("the live Solidarity Tech credentials and a member who already has a Discord identity")]
async fn member_with_identity(world: &mut LiveWorld) {
    let email: Email = require_env("ST_LIVE_EMAIL")
        .parse()
        .expect("ST_LIVE_EMAIL is not a valid email");
    let member = world
        .client
        .find_members(Some(&email), None)
        .await
        .expect("pre-read failed")
        .into_iter()
        .next()
        .expect("test record not found before write - confirm ST_LIVE_EMAIL");

    // A true no-op needs the member's current identity to write back; without
    // one there is nothing safe to round-trip.
    assert!(
        member.discord_handle.is_some() && member.discord_user_id.is_some(),
        "test member has no existing Discord identity to write back - \
         set discord-handle / discord-user-id on ST_LIVE_EMAIL first"
    );

    world.email = Some(email);
    world.before = Some(member);
}

#[given("no-op writes are allowed for this run")]
async fn noop_allowed(world: &mut LiveWorld) {
    if std::env::var("ST_LIVE_ALLOW_NOOP_WRITE").as_deref() == Ok("1") {
        world.noop_allowed = true;
    } else {
        // Honor the opt-in: without it, skip so the scenario reports as such
        // rather than touching the live record.
        panic!("set ST_LIVE_ALLOW_NOOP_WRITE=1 to enable the no-op write scenario");
    }
}

// ==============================================================================
// WHEN
// ==============================================================================

#[when("Espio finds the known member by email")]
async fn finds_known_member(world: &mut LiveWorld) {
    let email = world.email().clone();
    let members = world
        .client
        .find_members(Some(&email), None)
        .await
        .expect("find_members failed");
    world.before = members.into_iter().next();
}

#[when("Espio writes that same Discord identity back")]
async fn writes_identity_back(world: &mut LiveWorld) {
    let member = world
        .before
        .as_ref()
        .expect("no member was read before the write");
    let handle = member
        .discord_handle
        .clone()
        .expect("member lost its discord_handle between read and write");
    let user_id = member
        .discord_user_id
        .expect("member lost its discord_user_id between read and write");

    world
        .client
        .set_discord_identity(member.id.as_str(), &handle, user_id, DryRun(false))
        .await
        .expect("set_discord_identity failed");

    world.written = Some((handle, user_id));
}

// ==============================================================================
// THEN
// ==============================================================================

#[then("the known member is returned")]
async fn member_returned(world: &mut LiveWorld) {
    let member = world.before.as_ref().expect(
        "expected at least one match for ST_LIVE_EMAIL - if none, confirm the email filter param",
    );
    assert_eq!(
        &member.email,
        world.email(),
        "returned member's email does not match the query"
    );
}

#[then("the write succeeds and the record is unchanged")]
async fn write_unchanged(world: &mut LiveWorld) {
    let (handle, user_id) = world.written.clone().expect("no write was performed");
    let email = world.email().clone();

    let after = world
        .client
        .find_members(Some(&email), None)
        .await
        .expect("post-read failed")
        .into_iter()
        .next()
        .expect("test record not found after write");

    assert_eq!(
        after.discord_handle.as_ref(),
        Some(&handle),
        "discord_handle not persisted as written"
    );
    assert_eq!(
        after.discord_user_id,
        Some(user_id),
        "discord_user_id not persisted as written"
    );
}

#[tokio::main]
async fn main() {
    LiveWorld::cucumber()
        .init_tracing()
        .max_concurrent_scenarios(1)
        .fail_on_skipped()
        .run_and_exit("tests/features/solidarity_tech_live")
        .await;
}
