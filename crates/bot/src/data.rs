//! The poise per-bot state and the command type aliases.

use std::sync::Arc;

use persistence::{Auditor, PgStore};
use serenity::all::UserId;

use crate::config::BotConfig;
use crate::error::BotError;
use crate::lookup::RateLimiter;

/// Shared state every command receives via `Context`.
pub struct Data {
    pub config: BotConfig,
    pub store: Arc<PgStore>,
    pub auditor: Arc<Auditor>,
    pub rate_limiter: Arc<RateLimiter>,
    /// The bot's own user id, captured once at `ready`. Lets the message handler
    /// detect an @-mention synchronously, without a per-message lookup of the
    /// current user (and without needing the serenity `cache` feature).
    pub bot_user_id: UserId,
}

pub type Error = BotError;
pub type Context<'a> = poise::Context<'a, Data, Error>;
