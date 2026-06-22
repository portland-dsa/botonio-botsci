pub mod bulk_verify;
pub mod card;
pub mod forget;
pub mod help;
mod reclick;
pub mod setup;
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
        setup::setup(),
        help::help(),
    ];
    // The member-reset command is a testing affordance, gated to an environment that sets
    // BOT_FORGET_COMMAND; it is absent everywhere the var is unset.
    if std::env::var("BOT_FORGET_COMMAND").is_ok() {
        commands.push(forget::forget());
        tracing::info!("forget command enabled");
    }
    commands
}
