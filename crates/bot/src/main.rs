//! The always-on Discord bot - currently a read-only membership card plus help.

#![forbid(unsafe_code)]

mod commands;
mod config;
mod data;
mod error;
mod guild_guard;
mod notify;
mod ping;
mod render;

use std::sync::Arc;

use engine::backends::Clients;
use engine::backends::solidarity_tech::SolidarityTechHttp;
use engine::store::{InMemoryStore, build_index};

use crate::config::BotConfig;
use crate::data::Data;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load a local `.env` if present, before anything reads the environment
    // (`BotConfig::from_env` / `Clients::from_env`). Absent in production, where
    // the environment is supplied by the service manager - hence the ignored error.
    let _ = dotenvy::dotenv();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,botonio_botsci=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // Staging-only: when `SOLIDARITY_TECH_MOCK` is set, stand up the in-process mock
    // Solidarity Tech server bound to the host:port of `SOLIDARITY_TECH_BASE_URL` -
    // the same URL the client reads below - so staging serves fabricated members from
    // a single address that cannot drift. Production sets neither var, so this is inert.
    if std::env::var_os("SOLIDARITY_TECH_MOCK").is_some() {
        let base = std::env::var("SOLIDARITY_TECH_BASE_URL").map_err(|_| {
            anyhow::anyhow!("SOLIDARITY_TECH_MOCK is set but SOLIDARITY_TECH_BASE_URL is not")
        })?;
        let personas = std::env::var("SOLIDARITY_TECH_MOCK_PERSONAS").unwrap_or_default();
        let addr = mock_st::spawn(base.trim_start_matches("http://"), &personas).await?;
        tracing::warn!(%addr, "mock Solidarity Tech server active (staging fabricated data)");
    }

    let cfg = BotConfig::from_env()?;
    let clients = Clients::from_env().await?;
    // Today the bot reads only Solidarity Tech (for the index); Discord is driven by the
    // gateway token below. Share the one ST client with the refresh task via an `Arc`
    // rather than building a second full `Clients`.
    let solidarity_tech = Arc::new(clients.solidarity_tech);

    // First index build BEFORE serving - a bot that can't answer a card isn't ready.
    tracing::info!("building initial member index...");
    let index = build_index(solidarity_tech.as_ref(), &cfg.discord_list_id).await?;
    let store = Arc::new(InMemoryStore::new(index));
    tracing::info!("initial member index built");

    // Background refresh loop, owned by the bot (it shares the ST client + the runtime).
    spawn_refresh_loop(
        store.clone(),
        solidarity_tech.clone(),
        cfg.refresh_interval,
        cfg.discord_list_id.clone(),
    );

    let guild_id = cfg.guild_id;
    let token = secrecy::ExposeSecret::expose_secret(&cfg.token).to_owned();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            // Guild allowlist (defense in depth): commands are registered only to the
            // configured guild, but re-check every invocation so an interaction from any
            // other guild (or a DM) is refused even if the bot is added elsewhere.
            command_check: Some(|ctx| {
                Box::pin(async move {
                    Ok(ctx.guild_id().map(|g| g.get()) == Some(ctx.data().config.guild_id))
                })
            }),
            on_error: |e| Box::pin(error::on_error(e)),
            // No prefix commands exist; turn off mention-as-prefix so an @-mention
            // isn't parsed as a (missing) command - that path logs a spurious
            // "didn't recognize command name" warning. Our `event_handler` answers
            // the mention instead.
            prefix_options: poise::PrefixFrameworkOptions {
                mention_as_prefix: false,
                ..Default::default()
            },
            // Reply to any message that @-mentions the bot (see `ping`).
            event_handler: |ctx, event, _framework, data| {
                Box::pin(async move {
                    match event {
                        serenity::all::FullEvent::Message { new_message } => {
                            ping::on_message(ctx, new_message, data).await?;
                        }
                        // Refuse to operate anywhere but the configured home guild:
                        // leave any other server the moment we see it (whether just
                        // added or already present at connect). Defense in depth
                        // beneath the per-interaction command/ping guards.
                        serenity::all::FullEvent::GuildCreate { guild, is_new } => {
                            let home = serenity::all::GuildId::new(data.config.guild_id);
                            crate::guild_guard::on_guild_create(
                                &*ctx.http, guild.id, home, *is_new,
                            )
                            .await;
                        }
                        _ => {}
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                let gid = serenity::all::GuildId::new(guild_id);
                poise::builtins::register_in_guild(ctx, &framework.options().commands, gid).await?;
                tracing::info!("commands registered; bot is ready to serve");
                // The first index build already finished before `client.start()`, so
                // reaching this point means gateway-ready AND index-built - exactly the
                // condition systemd should treat as READY=1.
                crate::notify::ready();
                Ok(Data {
                    config: cfg,
                    store,
                    bot_user_id: ready.user.id,
                })
            })
        })
        .build();

    // GUILD_MESSAGES is needed to receive messages (to spot @-mentions); it is not a
    // privileged intent, and MESSAGE_CONTENT stays off - we read only mention metadata.
    // GUILD_MEMBERS (privileged) is deliberately NOT requested yet: member data
    // comes from the interaction payload and the index is built from Solidarity Tech,
    // not the gateway roster. The future implementation that caches the roster / handles
    // guild_member_add re-adds it.
    let intents =
        serenity::all::GatewayIntents::GUILDS | serenity::all::GatewayIntents::GUILD_MESSAGES;
    let mut client = serenity::all::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;

    // Keep the watchdog satisfied (no-op if systemd configured no watchdog).
    if let Some(interval) = notify::watchdog_interval() {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                notify::watchdog_ping();
            }
        });
    }

    // Close the gateway cleanly on SIGTERM/SIGINT (Ctrl-C) so `systemctl stop` and
    // deploy restarts drain in-flight interactions instead of being SIGKILLed.
    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received; closing the gateway");
        notify::stopping();
        shard_manager.shutdown_all().await;
    });

    client.start().await?;
    Ok(())
}

/// Resolve when the process is asked to terminate.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

/// On non-Unix (local Windows development), Ctrl-C is the shutdown signal.
#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// The refresh task shares the gateway's Solidarity Tech client (an `Arc`) and the
/// `Arc<InMemoryStore>` - nothing else.
fn spawn_refresh_loop(
    store: Arc<InMemoryStore>,
    solidarity_tech: Arc<SolidarityTechHttp>,
    interval: std::time::Duration,
    list_id: String,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // consume the immediate first tick (index already fresh)
        loop {
            ticker.tick().await;
            match build_index(solidarity_tech.as_ref(), &list_id).await {
                Ok(idx) => {
                    store.swap(idx);
                    tracing::info!("member index refreshed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "member index refresh failed; keeping last good index");
                }
            }
        }
    });
}
