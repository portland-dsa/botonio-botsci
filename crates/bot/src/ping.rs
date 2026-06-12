//! Replies to any message that @-mentions the bot.
//!
//! Receive-only: the mention is read from the message's `mentions` metadata (which
//! the gateway delivers without the MESSAGE_CONTENT intent), so this never reads
//! message text and never becomes a write path to guild state.

use serenity::all::{Context, Message};

use crate::data::{Data, Error};

/// What the bot says when it is pinged.
const REPLY: &str = "The old drive is dying, and the new drive struggles to be born; now is the time of monsters. And you pinged me, monster";

/// Reply to a non-bot message in the served guild that mentions us.
pub async fn on_message(ctx: &Context, message: &Message, data: &Data) -> Result<(), Error> {
    // Never answer other bots (or ourselves) - that is how reply loops start.
    if message.author.bot {
        return Ok(());
    }
    // Only in the guild we serve (mirrors the slash-command guild allowlist), and
    // only when we are actually @-mentioned.
    if message.guild_id.map(|g| g.get()) != Some(data.config.guild_id) {
        return Ok(());
    }
    if !message.mentions_user_id(data.bot_user_id) {
        return Ok(());
    }
    // `reply` threads the answer onto their message without an extra ping.
    message.reply(ctx, REPLY).await?;
    Ok(())
}
