//! The poise per-bot state and the command type aliases.

use std::sync::Arc;

use arc_swap::ArcSwap;
use engine::backends::discord::DiscordHttp;
use engine::backends::solidarity_tech::SolidarityTechHttp;
use engine::store::GuildConfig;
use persistence::{Auditor, PgStore};
use serenity::all::UserId;
use serenity::http::Http;

use crate::config::BotConfig;
use crate::error::BotError;
use crate::guild_config::build_role_writer;
use crate::lookup::RateLimiter;
use crate::refresh::Cooldown;

/// Shared state every command receives via `Context`.
pub struct Data {
    pub config: BotConfig,
    /// The live `/setup`-managed guild configuration, swapped atomically on each
    /// change so the moderator gate and the role-write client read the current value.
    pub guild_config: Arc<ArcSwap<GuildConfig>>,
    pub store: Arc<PgStore>,
    pub auditor: Arc<Auditor>,
    pub rate_limiter: Arc<RateLimiter>,
    /// Per-member throttle for the self-service verification flow.
    pub self_verify_limiter: Arc<RateLimiter>,
    /// The process-wide throttle for the on-demand `/refresh-cache` command.
    pub refresh_cooldown: Arc<Cooldown>,
    /// The gateway's shared HTTP, kept so the role-write client can be rebuilt from
    /// the current config on each write (see `role_writer`).
    pub http: Arc<Http>,
    /// The Solidarity Tech client, shared with the verify path and the refresh task.
    pub solidarity_tech: Arc<SolidarityTechHttp>,
    /// The bot's own user id, captured once at `ready`.
    pub bot_user_id: UserId,
    /// Whether SSO is enabled for this deploy (`BOT_SSO_ENABLED`), captured once at
    /// startup. The `/setup` panel shows the per-guild SSO toggle only when this is set -
    /// the deploy half of the two-gate model; without it the per-guild toggle is inert.
    pub sso_deploy_enabled: bool,
}

impl Data {
    /// Build the role-write client from the current config, or `None` when the three
    /// managed roles are not all configured. Cheap (a small map), so it is rebuilt per
    /// write and always reflects the latest `/setup`.
    pub fn role_writer(&self) -> Option<DiscordHttp> {
        let cfg = self.guild_config.load();
        build_role_writer(self.http.clone(), self.config.guild_id, &cfg)
    }
}

pub type Error = BotError;
pub type Context<'a> = poise::Context<'a, Data, Error>;
