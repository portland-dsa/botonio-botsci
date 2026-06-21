//! The always-on Discord bot - currently a read-only membership card plus help.

#![forbid(unsafe_code)]

mod commands;
mod config;
mod data;
mod error;
mod guild_config;
mod guild_guard;
mod lookup;
mod moderator;
mod notify;
mod ping;
mod render;

use std::sync::Arc;

use arc_swap::ArcSwap;
use domain::DiscordGuildId;
use engine::backends::Clients;
use engine::backends::solidarity_tech::SolidarityTechHttp;
use engine::store::{ConfigStore, RosterWrite, sweep_roster};

use persistence::PgStore;

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

    // Migration phase: the `migrate` subcommand applies pending migrations and exits. A
    // one-shot DDL mode, kept out of the serve path below (see `run_migrate`).
    if std::env::args().nth(1).as_deref() == Some("migrate") {
        return run_migrate().await;
    }

    // Staging-only: when `SOLIDARITY_TECH_MOCK` is set to a non-falsey value, stand up the
    // in-process mock Solidarity Tech server bound to the host:port of
    // `SOLIDARITY_TECH_BASE_URL` - the same URL the client reads below - so staging serves
    // fabricated members from a single address that cannot drift. Production sets neither
    // var, so this is inert.
    let mock_enabled = std::env::var("SOLIDARITY_TECH_MOCK").is_ok_and(|v| {
        // Off for "", "0", and "false", so `=0` disables rather than enabling on presence.
        let v = v.trim();
        !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    });
    if mock_enabled {
        let base = std::env::var("SOLIDARITY_TECH_BASE_URL").map_err(|_| {
            anyhow::anyhow!("SOLIDARITY_TECH_MOCK is set but SOLIDARITY_TECH_BASE_URL is not")
        })?;
        // Reduce the base URL to its host:port authority for the listener: strip the
        // scheme (any scheme, via the "://" delimiter), then drop any path. A bind needs
        // an explicit port, so reject a base without one rather than letting the OS pick an
        // ephemeral port the Solidarity Tech client would never call.
        let authority = base
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(base.as_str())
            .split('/')
            .next()
            .unwrap_or("");
        if !authority.contains(':') {
            return Err(anyhow::anyhow!(
                "SOLIDARITY_TECH_BASE_URL ({base}) must include an explicit host:port for the mock to bind"
            ));
        }
        let personas = std::env::var("SOLIDARITY_TECH_MOCK_PERSONAS").unwrap_or_default();
        let addr = mock_st::spawn(authority, &personas).await?;
        tracing::warn!(%addr, "mock Solidarity Tech server active (staging fabricated data)");
    }

    let cfg = BotConfig::from_env()?;
    // The bot reads only Solidarity Tech, for the membership index; Discord is driven by
    // the gateway token below, and the role-write client is rebuilt from the gateway's
    // shared `Http` on each write, not from a second token here.
    // Share the ST client with the refresh task via an `Arc`.
    let solidarity_tech = Arc::new(Clients::from_env().await?.solidarity_tech);

    // One store, owning one pool, shared via Data and the refresh/watchdog tasks. The
    // runtime DSN authenticates by peer over the Unix socket - no password (the
    // long-running process holds no DB secret). Migrations have already run in the
    // ExecStartPre `migrate` phase, so the schema exists; this role holds only DML. The
    // pool tuning (warm connections + headroom) lives in PgStore::connect.
    let store = Arc::new(PgStore::connect(&cfg.db_runtime_dsn).await?);

    // Load the /setup-managed guild config into a live, atomically-swappable handle.
    // A fresh guild has no row yet, so this defaults to all-unset; the moderator gate
    // and role writes stay closed until a moderator runs /setup.
    let guild_config = Arc::new(ArcSwap::from_pointee(
        store.load_config(DiscordGuildId(cfg.guild_id)).await?,
    ));

    // The audit writer shares the runtime pool but holds the HMAC key; the limiter is
    // pure in-memory state. Both are shared with the command handlers via `Data`.
    let auditor = Arc::new(persistence::Auditor::new(
        store.pool_handle(),
        cfg.audit_hash_key.clone(),
        cfg.audit_key_id.clone(),
    ));
    let rate_limiter = Arc::new(crate::lookup::RateLimiter::new(cfg.lookup_rate_per_min));

    // First roster load before serving. A fresh, non-empty sweep is written; a failed or
    // empty initial sweep no longer hard-fails startup - the durable cache may already hold
    // a good roster from a previous run, so serve that and let the background refresh
    // recover, rather than restart-looping on a transient upstream blip.
    tracing::info!("loading initial member roster...");
    match sweep_roster(solidarity_tech.as_ref(), &cfg.discord_list_id).await {
        Ok(records) if !records.is_empty() => match store.replace_roster(records).await {
            Ok(()) => tracing::info!("initial member roster loaded"),
            // A write failure rolls back, leaving the prior cache intact and servable, so
            // treat it like a failed sweep rather than hard-failing startup. is_populated()
            // below is the real backstop - it fails only when there is genuinely nothing.
            Err(e) => tracing::warn!(
                error = %e,
                "initial roster write failed; serving the cached roster"
            ),
        },
        Ok(_) => tracing::warn!(
            "initial solidarity tech sweep returned zero members; serving the cached roster"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "initial solidarity tech sweep failed; serving the cached roster"
        ),
    }
    // But never come up unable to answer any card: if no fresh roster was loaded and the
    // durable cache is also empty, there is genuinely nothing to serve - fail so systemd
    // retries rather than starting a bot that misses every lookup.
    if !store.is_populated().await? {
        anyhow::bail!(
            "no member roster available: the initial sweep produced nothing and the cache is empty"
        );
    }

    // Background refresh loop, owned by the bot (it shares the ST client + the runtime).
    spawn_refresh_loop(
        store.clone(),
        solidarity_tech.clone(),
        cfg.refresh_interval,
        cfg.discord_list_id.clone(),
    );

    // The framework setup closure below moves `store`, `auditor`, and `rate_limiter`
    // into Data; clone the handles the watchdog needs before that happens.
    let watchdog_store = store.clone();
    let setup_auditor = auditor.clone();
    let setup_rate_limiter = rate_limiter.clone();
    let setup_solidarity_tech = solidarity_tech.clone();
    let setup_guild_config = guild_config.clone();

    let guild_id = cfg.guild_id;
    let token = secrecy::ExposeSecret::expose_secret(&cfg.token).to_owned();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            // Guild allowlist: commands are registered only to the
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
            let auditor = setup_auditor;
            let rate_limiter = setup_rate_limiter;
            let solidarity_tech = setup_solidarity_tech;
            let guild_config = setup_guild_config;
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
                    auditor,
                    rate_limiter,
                    guild_config,
                    http: ctx.http.clone(),
                    solidarity_tech,
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

    // Keep the watchdog satisfied (no-op if systemd configured no watchdog), but only
    // while the database actually answers: an unreachable or unresponsive DB then lets
    // the watchdog fire and systemd restart the unit, rather than leaving a live process
    // that cannot serve a card. The liveness probe (PgStore::ping) is bounded by a timeout
    // under the ping cadence so a momentarily saturated pool - or a hung DB - fast-fails
    // this one ping instead of stalling the loop on sqlx's connection-acquire default.
    if let Some(interval) = notify::watchdog_interval() {
        let store = watchdog_store;
        tokio::spawn(async move {
            let liveness_timeout = interval / 2; // never outlast the ping cadence
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let probe = tokio::time::timeout(liveness_timeout, store.ping()).await;
                match probe {
                    Ok(Ok(())) => notify::watchdog_ping(),
                    Ok(Err(e)) => tracing::error!(
                        error = %e,
                        "database liveness check failed; skipping watchdog ping"
                    ),
                    Err(_) => tracing::error!(
                        timeout_secs = liveness_timeout.as_secs(),
                        "database liveness check timed out; skipping watchdog ping"
                    ),
                }
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

/// The `migrate` subcommand: apply pending migrations and exit. Runs in the dedicated
/// `ExecStartPre` step under the migration credential, never in the long-running serve
/// process. Reads only the database environment - the database name and the migration
/// password - and connects as the migration role over TCP loopback, keeping DDL authority
/// on a separate credential from the runtime's peer-over-socket login.
async fn run_migrate() -> anyhow::Result<()> {
    let db = std::env::var("DB_NAME")
        .map_err(|_| anyhow::anyhow!("DB_NAME must be set for the `migrate` subcommand"))?;
    let password = secrecy::SecretString::from(
        engine::backends::from_credstore_or_env("db_migration_password", "DB_MIGRATION_PASSWORD")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "migration password not found (credential db_migration_password / env DB_MIGRATION_PASSWORD)"
                )
            })?,
    );
    // Host/port default to loopback:5432 but can be overridden for a cluster listening
    // elsewhere; a set-but-unparseable port fails loudly rather than silently using 5432.
    let host = std::env::var("DB_MIGRATE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = match std::env::var("DB_MIGRATE_PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| anyhow::anyhow!("DB_MIGRATE_PORT must be a u16 port number, got {p:?}"))?,
        Err(_) => 5432,
    };
    // The connection itself lives in the persistence crate, which owns every sqlx detail.
    persistence::connect_and_migrate(
        &host,
        port,
        &db,
        secrecy::ExposeSecret::expose_secret(&password),
    )
    .await?;
    tracing::info!(database = %db, "migrations applied");
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
/// `Arc<PgStore>` - nothing else.
fn spawn_refresh_loop(
    store: Arc<PgStore>,
    solidarity_tech: Arc<SolidarityTechHttp>,
    interval: std::time::Duration,
    list_id: String,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // consume the immediate first tick (i.e. if the index is already fresh)
        loop {
            ticker.tick().await;
            match sweep_roster(solidarity_tech.as_ref(), &list_id).await {
                // An empty sweep is almost always an upstream glitch, not a real membership
                // of zero - keep the last good roster rather than wiping it.
                Ok(records) if records.is_empty() => {
                    tracing::warn!(
                        "solidarity tech sweep returned zero members; keeping last good roster"
                    );
                }
                Ok(records) => match store.replace_roster(records).await {
                    Ok(()) => tracing::info!("member roster refreshed"),
                    // Keep the last good roster on a write failure - do not clear it.
                    Err(e) => tracing::error!(
                        error = %e,
                        "member roster refresh failed to write; keeping last good roster"
                    ),
                },
                Err(e) => {
                    tracing::warn!(error = %e, "member index refresh failed; keeping last good index");
                }
            }
        }
    });
}
