//! Behavior suite for the mock Solidarity Tech server. Protagonist: Botonio, the
//! bot, reading a staging roster the mock fabricates - Sonic in good standing and
//! Tails lapsed. Drives the real `SolidarityTechHttp` client against a spawned
//! mock over loopback, so run it where loopback TCP is permitted (the same
//! constraint as the `solidarity_tech_mock` contract suite). Scenarios live in
//! `tests/features/roster_mock/`.

use cucumber::{World as _, given, then, when};
use secrecy::SecretString;

use backends::solidarity_tech::{SolidarityTechClient, SolidarityTechHttp, SolidarityTechMember};
use domain::MigsStatus;

/// Sonic's Discord account - the good-standing member.
const SONIC_ID: u64 = 1001;
/// Tails's Discord account - the lapsed member.
const TAILS_ID: u64 = 1002;

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct RosterWorld {
    client: Option<SolidarityTechHttp>,
    members: Vec<SolidarityTechMember>,
}

impl RosterWorld {
    async fn new() -> Self {
        Self {
            client: None,
            members: Vec::new(),
        }
    }

    fn member(&self, discord_id: u64) -> &SolidarityTechMember {
        self.members
            .iter()
            .find(|m| m.discord_user_id.map(|d| d.0) == Some(discord_id))
            .expect("member present in the roster")
    }
}

// `cucumber::World` requires `Debug`, but `SolidarityTechHttp` is not `Debug`;
// print only what is inspectable (the contract suite's World does the same).
impl std::fmt::Debug for RosterWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RosterWorld")
            .field("has_client", &self.client.is_some())
            .field("members", &self.members.len())
            .finish()
    }
}

#[given("the mock serves Sonic in good standing and Tails as lapsed")]
async fn mock_serves(world: &mut RosterWorld) {
    let personas = format!("{SONIC_ID}=good_standing,{TAILS_ID}=lapsed");
    let addr = mock_st::spawn("127.0.0.1:0", &personas)
        .await
        .expect("mock binds");
    world.client = Some(SolidarityTechHttp::with_base_url(
        format!("http://{addr}"),
        SecretString::from("test-token".to_string()),
    ));
}

#[when("Botonio reads the member list")]
async fn read_list(world: &mut RosterWorld) {
    let client = world.client.as_ref().expect("the mock was started");
    let page = client
        .members_in_list_page("any-list", None)
        .await
        .expect("read succeeds");
    world.members = page.members;
}

#[then("Botonio sees two members")]
async fn two_members(world: &mut RosterWorld) {
    assert_eq!(world.members.len(), 2, "expected Sonic and Tails");
}

#[then("Sonic is in good standing")]
async fn sonic_good(world: &mut RosterWorld) {
    assert_eq!(
        world.member(SONIC_ID).membership_standing,
        Some(MigsStatus::MemberInGoodStanding)
    );
}

#[then("Tails has lapsed")]
async fn tails_lapsed(world: &mut RosterWorld) {
    assert_eq!(
        world.member(TAILS_ID).membership_standing,
        Some(MigsStatus::Lapsed)
    );
}

#[tokio::main]
async fn main() {
    RosterWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/roster_mock")
        .await;
}
