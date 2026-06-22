// This behaviour suite `#[path]`-includes `../src/refresh.rs` for its `Cooldown`. Because
// an integration test compiles with `cfg(test)`, that also pulls in the module's own
// `#[cfg(test)]` unit tests and the `refresh_once` helper (unused here), all dead in this
// `harness = false` binary. Allow the resulting dead-code / unused-import noise here only;
// the bin compilation of the same module is unaffected.
#![allow(dead_code, unused_imports)]

//! Behaviour suite for the `/refresh-cache` cooldown (`src/refresh.rs`).
//!
//! Cast: **Sonic** is the moderator running /refresh-cache. The suite drives the pure
//! [`Cooldown`] over a hand-advanced clock - no database, no gateway, no real sweep - so it
//! runs on a plain offline `cargo test`. Scenarios live in `tests/features/refresh_cache/`.

#[path = "../src/refresh.rs"]
mod refresh;

use std::time::{Duration, Instant};

use cucumber::{World as _, then, when};

use refresh::{Cooldown, REFRESH_COOLDOWN};

#[derive(cucumber::World)]
#[world(init = Self::new)]
struct RefreshWorld {
    cooldown: Cooldown,
    /// A hand-advanced clock: `now` starts at `base` and steps forward by the "N seconds
    /// later" the scenario names, so the policy is exercised without sleeping.
    now: Instant,
    /// The result of the most recent refresh attempt.
    last: Option<Result<(), Duration>>,
}

impl std::fmt::Debug for RefreshWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshWorld")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl RefreshWorld {
    async fn new() -> Self {
        Self {
            cooldown: Cooldown::new(REFRESH_COOLDOWN),
            now: Instant::now(),
            last: None,
        }
    }
}

#[when("Sonic refreshes the cache")]
async fn refreshes(world: &mut RefreshWorld) {
    world.last = Some(world.cooldown.check(world.now));
}

#[when(regex = r"^Sonic refreshes the cache again (\d+) seconds later$")]
async fn refreshes_later(world: &mut RefreshWorld, secs: u64) {
    world.now += Duration::from_secs(secs);
    world.last = Some(world.cooldown.check(world.now));
}

#[then("the refresh is allowed")]
async fn allowed(world: &mut RefreshWorld) {
    assert_eq!(world.last, Some(Ok(())), "the refresh should be allowed");
}

#[then(regex = r"^the refresh is refused with (\d+) seconds left$")]
async fn refused(world: &mut RefreshWorld, secs: u64) {
    assert_eq!(
        world.last,
        Some(Err(Duration::from_secs(secs))),
        "the refresh should be refused with {secs}s left"
    );
}

#[tokio::main]
async fn main() {
    RefreshWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/refresh_cache")
        .await;
}
