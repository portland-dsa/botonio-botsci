//! The poise per-bot state and the command type aliases.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use engine::backends::discord::DiscordHttp;
use engine::backends::solidarity_tech::SolidarityTechHttp;
use engine::store::GuildConfig;
use persistence::{Auditor, PgStore};
use serenity::all::{GuildId, UserId};
use serenity::http::Http;

use crate::config::BotConfig;
use crate::error::BotError;
use crate::guild_config::managed_role_map;
use crate::lookup::RateLimiter;

/// Shared state every command receives via `Context`.
pub struct Data {
    pub config: BotConfig,
    /// The live `/setup`-managed guild configuration, swapped atomically on each
    /// change so the moderator gate and the role-write client read the current value.
    pub guild_config: Arc<ArcSwap<GuildConfig>>,
    pub store: Arc<PgStore>,
    pub auditor: Arc<Auditor>,
    pub rate_limiter: Arc<RateLimiter>,
    /// The gateway's shared HTTP, kept so the role-write client can be rebuilt from
    /// the current config on each write (see `role_writer`).
    pub http: Arc<Http>,
    /// The Solidarity Tech client, shared with the verify path and the refresh task.
    pub solidarity_tech: Arc<SolidarityTechHttp>,
    /// The bot's own user id, captured once at `ready`.
    pub bot_user_id: UserId,
}

impl Data {
    /// Build the role-write client from the current config, or `None` when the three
    /// managed roles are not all configured. Cheap (a small map), so it is rebuilt per
    /// write and always reflects the latest `/setup`.
    pub fn role_writer(&self) -> Option<DiscordHttp> {
        let cfg = self.guild_config.load();
        let map: HashMap<_, _> = managed_role_map(&cfg)?;
        let override_role = cfg
            .manual_override_role
            .map(|r| serenity::all::RoleId::new(r.0));
        Some(DiscordHttp::from_role_map(
            self.http.clone(),
            GuildId::new(self.config.guild_id),
            map,
            override_role,
        ))
    }
}

pub type Error = BotError;
pub type Context<'a> = poise::Context<'a, Data, Error>;
