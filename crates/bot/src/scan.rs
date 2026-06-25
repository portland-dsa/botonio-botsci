//! The scheduled membership scan: on a cadence, reconcile every guild member's role to
//! their Solidarity Tech standing and sweep orphans. Off unless the guild enabled it in
//! /setup. The plan + tripwire verdict are `engine::scan::plan`; this module owns the
//! loop, the paced apply via `Member::verify`, and the mass-demote alert.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use serenity::all::{ChannelId, Http};

use domain::DiscordGuildId;
use engine::backends::util::DiscordUserId;
use engine::bulk::enumerate;
use engine::scan::{ScanThreshold, ScanVerdict, plan};
use engine::store::{BulkScope, GuildConfig};
use engine::verify::{DataStore, Member, Target};
use persistence::{Auditor, PgStore};

use crate::guild_config::build_role_writer;

#[allow(clippy::too_many_arguments)]
pub fn spawn_scan_loop(
    http: Arc<Http>,
    store: Arc<PgStore>,
    auditor: Arc<Auditor>,
    solidarity_tech: Arc<engine::backends::solidarity_tech::SolidarityTechHttp>,
    guild_config: Arc<ArcSwap<GuildConfig>>,
    guild_id: u64,
    bot_user_id: DiscordUserId,
    interval: Duration,
    threshold: ScanThreshold,
    pace: Duration,
    catchup_gap: Duration,
    accent: u32,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // consume the immediate first tick; wait one interval before the first pass
        loop {
            ticker.tick().await;
            run_once(
                &http,
                &store,
                &auditor,
                &solidarity_tech,
                &guild_config,
                guild_id,
                bot_user_id,
                threshold,
                pace,
                catchup_gap,
                accent,
            )
            .await;
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    http: &Arc<Http>,
    store: &Arc<PgStore>,
    auditor: &Arc<Auditor>,
    solidarity_tech: &Arc<engine::backends::solidarity_tech::SolidarityTechHttp>,
    guild_config: &Arc<ArcSwap<GuildConfig>>,
    guild_id: u64,
    bot_user_id: DiscordUserId,
    threshold: ScanThreshold,
    pace: Duration,
    catchup_gap: Duration,
    accent: u32,
) {
    let cfg = guild_config.load();
    let guild = DiscordGuildId(guild_id);
    if !cfg.scan_enabled {
        // The scheduled scan is off, but the dues-reminder sweep toggles independently (see
        // GuildConfig). Run it on its own and skip the role-sync pass.
        run_reminder_sweep_if_enabled(&cfg, http, store, guild, pace, catchup_gap, accent).await;
        return;
    }
    let Some(discord) = build_role_writer(http.clone(), guild_id, &cfg) else {
        tracing::debug!("scheduled scan skipped: managed roles not configured");
        return;
    };
    let mod_channel = cfg.mod_approval_channel.map(|c| ChannelId::new(c.0));

    tracing::info!("scheduled scan starting");

    let members = match enumerate(&discord, BulkScope::WholeGuild).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "scheduled scan: roster enumeration failed; skipping pass");
            return;
        }
    };

    let scan_plan = match plan(
        store.as_ref(),
        &members,
        threshold,
        guild,
        chrono::Utc::now().date_naive(),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "scheduled scan: planning failed; skipping pass");
            return;
        }
    };

    if let ScanVerdict::Abort { demotions, scanned } = scan_plan.verdict {
        tracing::error!(
            demotions,
            scanned,
            percent = threshold.percent,
            floor = threshold.floor,
            "scheduled scan ABORTED by the mass-demote tripwire; no roles changed"
        );
        match mod_channel {
            Some(channel) => {
                let embed = crate::render::scan::scan_alert_embed(
                    demotions,
                    scanned,
                    threshold.percent,
                    threshold.floor,
                );
                if let Err(e) = channel
                    .send_message(
                        http.as_ref(),
                        serenity::all::CreateMessage::new().embed(embed),
                    )
                    .await
                {
                    tracing::error!(error = %e, "scheduled scan: failed to post the tripwire alert");
                }
            }
            // No mod-approval channel configured: the abort would otherwise be silent. Log
            // loudly so a paused mass-demote still leaves a visible trace; /setup also warns
            // when the scan is enabled without a channel set.
            None => {
                tracing::error!(
                    "scheduled scan: tripwire aborted but no mod-approval channel is configured; \
                     the mass-demote alert could not be delivered - set one in /setup"
                );
            }
        }
        return;
    }

    // Proceed: apply each change via the existing verify path, paced.
    let datastore = DataStore::new(
        solidarity_tech.as_ref(),
        &discord,
        store.as_ref(),
        auditor.as_ref(),
        guild,
    );
    let (mut promoted, mut demoted, mut failed) = (0usize, 0usize, 0usize);
    for (i, change) in scan_plan.changes.iter().enumerate() {
        // Pace between writes: not before the first, not after the last.
        if i > 0 {
            tokio::time::sleep(pace).await;
        }
        tracing::debug!(
            id = change.id.0,
            target = change.target.as_str(),
            demotion = change.demotion,
            "scan applying change"
        );
        let result = Member::new(
            &datastore,
            Target {
                id: change.id,
                handle: change.handle.clone(),
            },
        )
        .verify(bot_user_id)
        .await;
        match result {
            Ok(_) if change.demotion => demoted += 1,
            Ok(_) => promoted += 1,
            Err(e) => {
                failed += 1;
                tracing::error!(error = %e, "scheduled scan: a member apply failed; continuing");
            }
        }
    }

    tracing::info!(
        scanned = scan_plan.scanned,
        promoted,
        demoted,
        misses = scan_plan.misses,
        conflicts = scan_plan.conflicts,
        malformed = scan_plan.malformed,
        failed,
        "scheduled scan complete"
    );

    // Dues-reminder sweep: runs after the role-sync apply completes. The tripwire-abort and
    // scan-error paths return early above, so the sweep is naturally skipped on an aborted pass.
    run_reminder_sweep_if_enabled(&cfg, http, store, guild, pace, catchup_gap, accent).await;
}

/// Run the dues-reminder sweep when the guild has it enabled and a dues-reminder channel set.
/// Split out of `run_once` so it runs whether or not the scheduled scan is enabled - the two
/// toggle independently (see `GuildConfig`) - while still being skipped on a tripwire abort,
/// whose early return is reached before either call site.
async fn run_reminder_sweep_if_enabled(
    cfg: &GuildConfig,
    http: &Arc<Http>,
    store: &Arc<PgStore>,
    guild: DiscordGuildId,
    pace: Duration,
    catchup_gap: Duration,
    accent: u32,
) {
    if !cfg.reminders_enabled {
        return;
    }
    let Some(chan) = cfg.dues_reminder_channel else {
        tracing::debug!("reminders enabled but no dues-reminder channel configured; skipping");
        return;
    };
    crate::reminders::run_reminder_sweep(crate::reminders::ReminderCtx {
        http: http.as_ref(),
        store: store.as_ref(),
        guild,
        today: chrono::Utc::now().date_naive(),
        parent_channel: ChannelId::new(chan.0),
        signup_url: cfg.dues_signup_url.as_deref(),
        catchup_gap,
        pace,
        accent,
    })
    .await;
}
