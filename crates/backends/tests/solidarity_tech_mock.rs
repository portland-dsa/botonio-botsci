//! Contract suite for the Solidarity Tech backend - the offline wire contract,
//! pinned against a `wiremock` server. Protagonist: **Espio**, the Solidarity
//! Tech client. Scenarios live in `tests/features/solidarity_tech_contract/`.
//!
//! Each scenario mounts mocks on Espio's `MockServer`, drives a real
//! `SolidarityTechHttp` pointed at `server.uri()`, then asserts on the stashed
//! result and verifies the request shape the client actually sent. The
//! custom-property keys in the bodies here ("discord-handle" / "discord-user-id")
//! match the serde renames on the client's custom-property struct; if the live
//! org's keys differ, those renames and these literals move together.
//!
//! The suite's support code is split into sibling modules: [`harness`] (the
//! `EspioWorld` constructor, accessor, and `Debug` impl), [`fixtures`] (JSON
//! response builders), and the step definitions in `steps_members` /
//! `steps_resilience`. Cucumber discovers step functions by attribute at compile
//! time, so the `mod` declarations below are all the wiring they need.

// The support modules live in a subdirectory so Cargo does not treat each as a
// separate integration-test target (only top-level `tests/*.rs` are targets).
// An integration-test root resolves `mod` paths against its own directory
// (`tests/`), so each needs an explicit `#[path]` into the subdirectory.
#[path = "solidarity_tech_mock/fixtures.rs"]
mod fixtures;
#[path = "solidarity_tech_mock/harness.rs"]
mod harness;
#[path = "solidarity_tech_mock/steps_members.rs"]
mod steps_members;
#[path = "solidarity_tech_mock/steps_resilience.rs"]
mod steps_resilience;

use cucumber::World as _;
use wiremock::MockServer;

use backends::solidarity_tech::{
    CustomUserProperty, SolidarityTechError, SolidarityTechHttp, SolidarityTechMember,
};

pub(crate) const TOKEN: &str = "test-token-abc";
pub(crate) const MEMBER_ID: &str = "4242";

/// What a `when` step parked for a `then` to inspect.
pub(crate) enum Outcome {
    Members(Result<Vec<SolidarityTechMember>, SolidarityTechError>),
    Write(Result<(), SolidarityTechError>),
    Properties(Result<Vec<CustomUserProperty>, SolidarityTechError>),
}

/// Per-scenario state Espio drives: the mock API, the client aimed at it, and
/// the last call's outcome. Constructed by [`EspioWorld::new`](harness) in the
/// `harness` module.
#[derive(cucumber::World)]
#[world(init = Self::new)]
pub(crate) struct EspioWorld {
    pub(crate) server: MockServer,
    pub(crate) client: SolidarityTechHttp,
    pub(crate) last: Option<Outcome>,
    /// Wall-clock elapsed across the paced sequential-request scenario.
    pub(crate) elapsed: Option<std::time::Duration>,
}

#[tokio::main]
async fn main() {
    EspioWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/solidarity_tech_contract")
        .await;
}
