//! Behavior suite for the verify wizard's modal re-click dispatch
//! (`src/commands/reclick.rs`).
//!
//! When a moderator dismisses a modal without submitting, the host buttons stay live.
//! `reclick_action` decides whether the next press reopens that modal or is handed back
//! to the wizard's main dispatch. This pins that decision offline - no gateway, no
//! serenity - the same way the lookup suite pins its decision core. The regression it
//! guards: a Skip or Stop press after a dismissed email modal used to reopen the modal
//! instead of advancing the bulk wizard.
//!
//! Cast: Ralsei is the moderator running the wizard. Scenarios live in
//! `tests/features/wizard_dispatch/`.

// The bot is a binary crate with no lib target, so pull the self-contained dispatch core
// straight into this test binary. `#[path]` resolves against `tests/`.
#[path = "../src/commands/reclick.rs"]
mod reclick;

use cucumber::{World as _, given, then, when};

use reclick::{ReclickAction, reclick_action};

// The wizard's button custom_ids, mirrored from `commands::verify` (lookup, override) and
// `commands::bulk_verify` (skip, stop). `reclick_action` only compares a press against the
// open modal's trigger for equality, so the suite maps each button label to its id here.
const LOOKUP_ID: &str = "verify_lookup_email";
const OVERRIDE_ID: &str = "verify_override_approve";
const SKIP_ID: &str = "bulk_skip";
const STOP_ID: &str = "bulk_stop";

fn button_id(label: &str) -> &'static str {
    match label {
        "Look up by email" => LOOKUP_ID,
        "Override and approve" => OVERRIDE_ID,
        "Skip" => SKIP_ID,
        "Stop" => STOP_ID,
        other => panic!("unknown button {other}"),
    }
}

#[derive(Debug, Default, cucumber::World)]
struct WizardWorld {
    /// The trigger button of the modal currently open, if any.
    open_modal_trigger: Option<&'static str>,
    /// The action decided for the most recent press.
    action: Option<ReclickAction>,
}

#[given(regex = r"^(\w+) opened the email lookup modal$")]
async fn opened_email_modal(world: &mut WizardWorld, _who: String) {
    world.open_modal_trigger = Some(LOOKUP_ID);
}

#[given(regex = r"^(\w+) opened the override reason modal$")]
async fn opened_override_modal(world: &mut WizardWorld, _who: String) {
    world.open_modal_trigger = Some(OVERRIDE_ID);
}

#[given(regex = r"^(\w+) dismissed the modal without submitting$")]
async fn dismissed_modal(_world: &mut WizardWorld, _who: String) {
    // A dismissed modal fires no event, so the modal is still considered open - its
    // trigger button stays live. This step only narrates; the next press is what matters.
}

#[when(regex = r"^(\w+) presses (.+)$")]
async fn presses(world: &mut WizardWorld, _who: String, label: String) {
    let trigger = world
        .open_modal_trigger
        .expect("a modal must be open before a press");
    world.action = Some(reclick_action(button_id(&label), trigger));
}

#[then("the press is handed off to the wizard")]
async fn handed_off(world: &mut WizardWorld) {
    assert_eq!(world.action, Some(ReclickAction::Redispatch));
}

#[then("the modal is not reopened")]
async fn not_reopened(world: &mut WizardWorld) {
    assert_ne!(world.action, Some(ReclickAction::Reopen));
}

#[then("the modal is reopened")]
async fn reopened(world: &mut WizardWorld) {
    assert_eq!(world.action, Some(ReclickAction::Reopen));
}

#[tokio::main]
async fn main() {
    WizardWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/wizard_dispatch")
        .await;
}
