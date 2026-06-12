//! Behavior suite for the home-guild guard (`src/guild_guard.rs`). Protagonist:
//! **Botonio**, the bot. The home guild is Sonic's server; the unauthorized guild
//! is Eggman's server. Mocks only - there is no live target (automating an
//! ephemeral guild and a second account to watch the bot leave is not worth it),
//! so the leave goes through a recording double. Scenarios live in
//! `tests/features/guild_guard/`.

// The bot is a binary crate with no lib target to import from, so pull the
// self-contained guard module straight into this test binary. `#[path]` resolves
// against `tests/`, hence the `../src` hop (the same trick the backend suites use
// for their support modules).
#[path = "../src/guild_guard.rs"]
mod guild_guard;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cucumber::{World as _, given, then, when};
use serenity::all::GuildId;

use guild_guard::{GuildLeaver, on_guild_create};

/// Sonic's server: the configured home guild Botonio serves.
const SONIC_SERVER: u64 = 1;
/// Eggman's server: an unauthorized guild Botonio must flee.
const EGGMAN_SERVER: u64 = 666;

/// A recording stand-in for serenity's HTTP: it remembers every guild it was asked
/// to leave, and can be told to refuse the exit (the refused-exit scenario).
#[derive(Debug, Default)]
struct RecordingLeaver {
    left: Mutex<Vec<GuildId>>,
    refuse: AtomicBool,
}

#[async_trait::async_trait]
impl GuildLeaver for RecordingLeaver {
    async fn leave(&self, guild: GuildId) -> Result<(), serenity::Error> {
        self.left.lock().unwrap().push(guild);
        if self.refuse.load(Ordering::SeqCst) {
            Err(serenity::Error::Other("eggman barred the exit"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, cucumber::World)]
#[world(init = Self::new)]
struct BotonioWorld {
    home: GuildId,
    leaver: RecordingLeaver,
}

impl BotonioWorld {
    async fn new() -> Self {
        Self {
            home: GuildId::new(SONIC_SERVER),
            leaver: RecordingLeaver::default(),
        }
    }
}

#[given("Botonio's home server is Sonic's server")]
async fn home_is_sonic(world: &mut BotonioWorld) {
    world.home = GuildId::new(SONIC_SERVER);
}

#[given("Eggman's server will refuse Botonio's exit")]
async fn eggman_refuses(world: &mut BotonioWorld) {
    world.leaver.refuse.store(true, Ordering::SeqCst);
}

#[when("Botonio receives a guild-create for Sonic's server")]
async fn create_for_sonic(world: &mut BotonioWorld) {
    on_guild_create(
        &world.leaver,
        GuildId::new(SONIC_SERVER),
        world.home,
        Some(true),
    )
    .await;
}

#[when("Botonio receives a guild-create for Eggman's server")]
async fn create_for_eggman(world: &mut BotonioWorld) {
    on_guild_create(
        &world.leaver,
        GuildId::new(EGGMAN_SERVER),
        world.home,
        Some(true),
    )
    .await;
}

#[when("Botonio receives a startup guild-create for Eggman's server")]
async fn startup_create_for_eggman(world: &mut BotonioWorld) {
    // is_new = Some(false): the guild was already present when the gateway connected.
    on_guild_create(
        &world.leaver,
        GuildId::new(EGGMAN_SERVER),
        world.home,
        Some(false),
    )
    .await;
}

#[then("Botonio does not leave any server")]
async fn left_nothing(world: &mut BotonioWorld) {
    assert!(
        world.leaver.left.lock().unwrap().is_empty(),
        "Botonio should not have left any server"
    );
}

#[then("Botonio leaves Eggman's server")]
async fn left_eggman(world: &mut BotonioWorld) {
    let left = world.leaver.left.lock().unwrap();
    assert_eq!(
        *left,
        vec![GuildId::new(EGGMAN_SERVER)],
        "Botonio should have left exactly Eggman's server"
    );
}

#[then("Botonio attempts to leave Eggman's server")]
async fn attempted_eggman(world: &mut BotonioWorld) {
    assert!(
        world
            .leaver
            .left
            .lock()
            .unwrap()
            .contains(&GuildId::new(EGGMAN_SERVER)),
        "Botonio should have attempted to leave Eggman's server"
    );
}

#[then("Botonio does not crash")]
async fn no_crash(_world: &mut BotonioWorld) {
    // Reaching this step means `on_guild_create` returned despite the refused exit -
    // the error was swallowed, not propagated or a panic.
}

#[tokio::main]
async fn main() {
    BotonioWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("tests/features/guild_guard")
        .await;
}
