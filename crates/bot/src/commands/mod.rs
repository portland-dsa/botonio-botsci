pub mod bulk_verify;
pub mod card;
pub mod channels;
pub mod forget;
pub mod grace;
pub mod help;
mod reclick;
pub mod refresh_cache;
pub mod reminders;
pub mod setup;
pub mod strip_roles;
pub mod verify;

use crate::data::{Data, Error};

/// Every command the bot registers.
pub fn all() -> Vec<poise::Command<Data, Error>> {
    let mut commands = vec![
        card::membership_card(),
        card::membership_card_menu(),
        card::lookup(),
        verify::verify(),
        bulk_verify::bulk_verify(),
        channels::channels(),
        grace::grace(),
        refresh_cache::refresh_cache(),
        reminders::reminders(),
        setup::setup(),
        help::help(),
    ];
    // The test-send subcommand is a staging affordance pushed onto the reminders parent's
    // subcommand list when BOT_REMINDER_TEST_SEND is set; it is absent everywhere the var is unset.
    if std::env::var("BOT_REMINDER_TEST_SEND").is_ok()
        && let Some(cmd) = commands.iter_mut().find(|c| c.name == "reminders")
    {
        cmd.subcommands.push(reminders::test_send());
        tracing::info!("reminders test-send subcommand enabled");
    }
    // The member-reset command is a testing affordance, gated to an environment that sets
    // BOT_FORGET_COMMAND; it is absent everywhere the var is unset.
    if std::env::var("BOT_FORGET_COMMAND").is_ok() {
        commands.push(forget::forget());
        tracing::info!("forget command enabled");
    }
    // Likewise the bulk role-strip: a staging testing affordance gated to an environment
    // that sets BOT_STRIP_ROLES_COMMAND; absent everywhere the var is unset.
    if std::env::var("BOT_STRIP_ROLES_COMMAND").is_ok() {
        commands.push(strip_roles::strip_roles());
        tracing::info!("strip-roles command enabled");
    }
    commands
}
