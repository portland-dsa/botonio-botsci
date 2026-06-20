pub mod card;
pub mod help;

use crate::data::{Data, Error};

/// Every command the bot registers.
pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![
        card::membership_card(),
        card::membership_card_menu(),
        card::lookup(),
        help::help(),
    ]
}
