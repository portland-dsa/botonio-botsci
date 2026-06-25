//! Dues-reminder sweep: on each scan pass, send each due member their milestone
//! message into a private thread off the configured dues-expired channel, and
//! reconcile the Dues Expiring marker role for all members with an xdate.
//!
//! The sweep and the per-member [`send_notice`] helper are separated so the
//! staging test-send command can drive a single member without running the full
//! planner. Both receive a [`ReminderCtx`] carrying the stable per-pass context.
//!
//! This module also owns the persistent button handler ([`on_component`]) for the
//! opt-out / ask-an-admin actions embedded in each reminder message.

use std::time::Duration;

use chrono::{Local, NaiveDate};
use domain::DiscordGuildId;
use engine::backends::discord::{DiscordClient, MarkerRole};
use engine::backends::solidarity_tech::MembershipType;
use engine::backends::util::DiscordUserId;
use engine::reminders::{ExpiryStatus, MessageKind, Milestone};
use engine::store::{MemberStore, MessageTemplates, OptOutSource, ReminderStore};
use persistence::PgStore;
use serenity::all::{
    AutoArchiveDuration, ChannelId, ChannelType, ComponentInteraction, Context,
    CreateAllowedMentions, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, CreateThread, EditThread, Http, RoleId, UserId,
};

use crate::data::{Data, Error};
use crate::render::reminders::{
    DUES_BANNER_HELP_ID, DUES_HELP_ID, DUES_OPTOUT_ID, default_body, reminder_message,
};

// ---- context ------------------------------------------------------------

/// Stable per-pass context threaded from the scan loop. The staging test-send
/// command constructs one directly to drive a single member.
pub struct ReminderCtx<'a> {
    pub http: &'a Http,
    pub store: &'a PgStore,
    pub discord: &'a engine::backends::discord::DiscordHttp,
    pub guild: DiscordGuildId,
    pub today: NaiveDate,
    pub parent_channel: ChannelId,
    pub signup_url: Option<&'a str>,
    pub pace: Duration,
    pub accent: u32,
}

// ---- per-member helper --------------------------------------------------

/// Create or reuse the member's lifecycle thread off `parent_channel`.
///
/// If a thread is already persisted in `reminder_state`, it is returned as-is.
/// Otherwise a private thread is created, the member is added to it, and the id is
/// persisted via `set_thread` before returning - so a later failure on the caller's
/// side reuses the same thread on the next attempt rather than orphaning it.
///
/// `status` drives the thread title: `Expiring` -> Renewal Notice, `Lapsed` -> Lapse
/// Notice. On a `Lapsed` request when a thread already exists, the thread is retitled.
#[allow(clippy::too_many_arguments)]
async fn ensure_lifecycle_thread(
    http: &Http,
    store: &PgStore,
    guild: DiscordGuildId,
    parent_channel: ChannelId,
    member: DiscordUserId,
    display_name: &str,
    status: ExpiryStatus,
    xdate: NaiveDate,
) -> Result<ChannelId, crate::error::BotError> {
    let state = store.reminder_state(guild, member).await?;
    let serenity_user = UserId::new(member.0);

    let thread_title = |milestone: Milestone| -> String {
        match milestone {
            Milestone::Renewal => format!("{display_name} - Dues Renewal Notice"),
            Milestone::Lapse => format!("{display_name} - Dues Lapse Notice"),
        }
    };

    let milestone = match status {
        ExpiryStatus::Expiring { .. } | ExpiryStatus::Current => Milestone::Renewal,
        ExpiryStatus::Lapsed => Milestone::Lapse,
    };

    let thread_id: i64 = match state.and_then(|s| s.thread_id) {
        Some(tid) => {
            // On a Lapse request, retitle the thread if it exists.
            if milestone == Milestone::Lapse {
                let thread_channel = ChannelId::new(tid as u64);
                if let Err(e) = thread_channel
                    .edit_thread(http, EditThread::new().name(thread_title(Milestone::Lapse)))
                    .await
                {
                    tracing::warn!(id = member.0, error = %e, "ensure_lifecycle_thread: could not retitle thread on lapse");
                }
            }
            tid
        }
        None => {
            let thread = parent_channel
                .create_thread(
                    http,
                    CreateThread::new(thread_title(milestone))
                        .kind(ChannelType::PrivateThread)
                        .auto_archive_duration(AutoArchiveDuration::OneWeek)
                        .invitable(false),
                )
                .await?;

            thread.id.add_thread_member(http, serenity_user).await?;

            // Discord snowflakes fit in 63 bits; a wrapping cast is lossless
            // and the reverse cast (tid as u64) recovers the exact value.
            let tid = thread.id.get() as i64;

            // Persist before the caller's message goes out, so a later failure
            // reuses this thread rather than orphaning it.
            store.set_thread(guild, member, xdate, tid).await?;
            tid
        }
    };

    Ok(ChannelId::new(thread_id as u64))
}

/// Send one member their dues notice. Creates or reuses their private thread,
/// resolves the correct template body, sends the message, and records the send.
/// On a Lapse send, edits the thread's name to the lapse title.
///
/// Returns an error on Discord API or store failure; the sweep loop treats each
/// such error as a skip and continues.
pub async fn send_notice(
    ctx: &ReminderCtx<'_>,
    member: DiscordUserId,
    milestone: Milestone,
    membership_type: Option<MembershipType>,
    xdate: NaiveDate,
) -> Result<(), crate::error::BotError> {
    // Resolve the template kind from membership type. Lapse and Renewal both key on
    // membership type. Skip with a warning when the type is absent - the right template
    // cannot be chosen without it.
    let kind = match membership_type {
        Some(t) => MessageKind::from_membership_type(t),
        None => {
            tracing::warn!(
                id = member.0,
                milestone = milestone.as_token(),
                "skipping notice: membership_type is absent"
            );
            return Ok(());
        }
    };

    let serenity_user = UserId::new(member.0);

    // Resolve the member's server display name for the thread title.
    let display_name = match serenity::all::GuildId::new(ctx.guild.0)
        .member(ctx.http, serenity_user)
        .await
    {
        Ok(m) => m.display_name().to_owned(),
        Err(e) => {
            tracing::warn!(id = member.0, error = %e, "send_notice: could not fetch member for thread title; using id");
            member.0.to_string()
        }
    };

    let status = match milestone {
        Milestone::Renewal => ExpiryStatus::Expiring {
            time_left: xdate - ctx.today,
        },
        Milestone::Lapse => ExpiryStatus::Lapsed,
    };

    let thread_channel = ensure_lifecycle_thread(
        ctx.http,
        ctx.store,
        ctx.guild,
        ctx.parent_channel,
        member,
        &display_name,
        status,
        xdate,
    )
    .await?;

    let thread_id = thread_channel.get() as i64;

    // Resolve body: stored moderator template, else the built-in default.
    let body: String = match ctx.store.template(ctx.guild, kind).await? {
        Some(custom) => custom,
        None => default_body(kind).to_owned(),
    };

    let msg = reminder_message(
        &body,
        milestone,
        xdate,
        ctx.signup_url,
        serenity_user,
        ctx.accent,
    );
    thread_channel.send_message(ctx.http, msg).await?;

    ctx.store
        .record_sent(ctx.guild, member, xdate, milestone, thread_id)
        .await?;

    Ok(())
}

// ---- sweep --------------------------------------------------------------

/// Run the dues-reminder sweep for one scan pass.
///
/// Plans which members are due a notice today and delivers each one paced.
/// Also reconciles the Dues Expiring marker role for all members with an xdate.
/// A per-member failure is logged and skipped; the sweep continues.
///
/// `info!` carries aggregate counts only; `debug!` carries the member id with
/// no other PII.
pub async fn run_reminder_sweep(ctx: ReminderCtx<'_>) {
    let plan = match engine::reminders::plan(ctx.store, ctx.guild, ctx.today).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "reminder sweep: planning failed; skipping pass");
            return;
        }
    };

    let total = plan.due.len();
    tracing::info!(total, "reminder sweep starting");

    let (mut sent, mut failed) = (0usize, 0usize);

    for (i, due) in plan.due.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(ctx.pace).await;
        }

        tracing::debug!(
            id = due.id.0,
            milestone = due.milestone.as_token(),
            "reminder sweep: sending"
        );

        match send_notice(&ctx, due.id, due.milestone, due.membership_type, due.xdate).await {
            Ok(()) => sent += 1,
            Err(e) => {
                failed += 1;
                tracing::error!(
                    error = %e,
                    "reminder sweep: per-member send failed; continuing"
                );
            }
        }
    }

    tracing::info!(sent, failed, total, "reminder sweep notices done");

    // Reconcile the Dues Expiring marker for all members with an xdate.
    reconcile_expiring_markers(&ctx).await;
}

/// Reconcile the Dues Expiring marker role for all members with a lapse date.
///
/// A member who is `Expiring`, not yet marked, and in good standing -> grant the marker.
/// A member who is currently marked but now `Current` or `Lapsed` -> remove it.
/// Per-member errors are logged and skipped.
async fn reconcile_expiring_markers(ctx: &ReminderCtx<'_>) {
    let records = match ctx.store.all_records().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "reminder sweep: could not read roster for marker reconciliation");
            return;
        }
    };

    let (mut granted, mut removed, mut failed) = (0usize, 0usize, 0usize);

    for record in &records {
        let Some(id) = record.discord_user_id else {
            continue;
        };
        let Some(xdate) = record.expires else {
            continue;
        };

        let status = ExpiryStatus::from(xdate - ctx.today);

        let state = match ctx.store.reminder_state(ctx.guild, id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(id = id.0, error = %e, "marker reconcile: could not read state; skipping");
                failed += 1;
                continue;
            }
        };

        // Treat a stale cycle as unmarked.
        let expiring_marked = match &state {
            Some(s) if s.cycle_xdate == xdate => s.expiring_marked,
            _ => false,
        };

        match status {
            ExpiryStatus::Expiring { time_left } if !expiring_marked => {
                tracing::debug!(
                    id = id.0,
                    days = time_left.num_days(),
                    "marker reconcile: granting DuesExpiring"
                );
                if let Err(e) = ctx
                    .discord
                    .assign_marker_role(id, MarkerRole::DuesExpiring)
                    .await
                {
                    tracing::warn!(id = id.0, error = %e, "marker reconcile: assign failed; skipping");
                    failed += 1;
                    continue;
                }
                if let Err(e) = ctx
                    .store
                    .set_expiring_marked(ctx.guild, id, xdate, true)
                    .await
                {
                    tracing::warn!(id = id.0, error = %e, "marker reconcile: set_expiring_marked failed");
                    failed += 1;
                }
                granted += 1;
            }
            ExpiryStatus::Current | ExpiryStatus::Lapsed if expiring_marked => {
                tracing::debug!(id = id.0, "marker reconcile: removing DuesExpiring");
                if let Err(e) = ctx
                    .discord
                    .remove_marker_role(id, MarkerRole::DuesExpiring)
                    .await
                {
                    tracing::warn!(id = id.0, error = %e, "marker reconcile: remove failed; skipping");
                    failed += 1;
                    continue;
                }
                if let Err(e) = ctx
                    .store
                    .set_expiring_marked(ctx.guild, id, xdate, false)
                    .await
                {
                    tracing::warn!(id = id.0, error = %e, "marker reconcile: set_expiring_marked failed");
                    failed += 1;
                }
                removed += 1;
            }
            _ => {}
        }
    }

    tracing::info!(granted, removed, failed, "marker reconcile complete");
}

/// Remove the Dues Expiring marker from every currently-marked member and clear the flag.
/// Called when reminders are toggled off so the marker does not linger on members.
/// Per-member errors are logged and skipped.
pub async fn cleanup_expiring_markers(
    store: &PgStore,
    discord: &engine::backends::discord::DiscordHttp,
    guild: DiscordGuildId,
    pace: Duration,
) {
    let members = match store.marked_members(guild).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "cleanup_expiring_markers: could not read marked members");
            return;
        }
    };

    let total = members.len();
    let (mut removed, mut failed) = (0usize, 0usize);

    for (i, id) in members.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(pace).await;
        }
        tracing::debug!(id = id.0, "cleanup: removing DuesExpiring marker");
        if let Err(e) = discord
            .remove_marker_role(*id, MarkerRole::DuesExpiring)
            .await
        {
            tracing::warn!(id = id.0, error = %e, "cleanup: remove_marker_role failed; skipping");
            failed += 1;
            continue;
        }
        // The cycle key only addresses the member's row; the flag is what we are clearing.
        // Read the stored cycle_xdate so set_expiring_marked targets that current row.
        let xdate = match store.reminder_state(guild, *id).await {
            Ok(Some(s)) => s.cycle_xdate,
            _ => {
                // Cannot determine cycle_xdate; skip the flag clear but count as success.
                removed += 1;
                continue;
            }
        };
        if let Err(e) = store.set_expiring_marked(guild, *id, xdate, false).await {
            tracing::warn!(id = id.0, error = %e, "cleanup: set_expiring_marked failed");
            failed += 1;
        } else {
            removed += 1;
        }
    }

    tracing::info!(total, removed, failed, "cleanup_expiring_markers complete");
}

// ---- button handler -----------------------------------------------------

/// Route a dues-reminder button press (opt-out / ask-an-admin / banner get-help).
///
/// Ignores interactions that are not from the home guild or not a `dues_*` id.
/// For `DUES_HELP_ID`/`DUES_OPTOUT_ID` (which carry a `kind:member_id` suffix)
/// only the target member may act. `DUES_BANNER_HELP_ID` carries no suffix - the
/// presser is the actor.
pub async fn on_component(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
) -> Result<(), Error> {
    if !in_home_guild(c.guild_id, data) {
        return Ok(());
    }
    let id = c.data.custom_id.as_str();

    // Banner help: no member-id suffix; the presser is the actor.
    if id == DUES_BANNER_HELP_ID {
        let presser = DiscordUserId(c.user.id.get());
        return handle_banner_help(ctx, c, data, presser).await;
    }

    let Some((kind, suffix)) = id.split_once(':') else {
        return Ok(());
    };
    // Only act on dues_* prefixes.
    if !matches!(kind, DUES_OPTOUT_ID | DUES_HELP_ID) {
        return Ok(());
    }
    // Parse the target member id from the suffix.
    let Some(target_id) = suffix.parse::<u64>().ok().map(DiscordUserId) else {
        return Ok(());
    };
    // Defence in depth: only the target member may act on their own buttons.
    if c.user.id.get() != target_id.0 {
        return ack_ephemeral(ctx, c, "This button is for the member it was sent to.").await;
    }

    let guild = data.config.guild();

    match kind {
        DUES_OPTOUT_ID => handle_optout(ctx, c, data, guild, target_id).await,
        DUES_HELP_ID => handle_help(ctx, c, data, target_id).await,
        _ => Ok(()),
    }
}

/// Handle a press of the "Get help" button on the dues-expired channel banner.
///
/// The presser is the actor. If they hold a dues-related role (Member / DuesExpired /
/// DuesExpiring), their lifecycle thread is created (or reused) and the configured
/// moderator role is pinged in it. A presser who holds only the moderator role and
/// no dues role gets a private "this is for members" reply - no thread is created.
async fn handle_banner_help(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    presser: DiscordUserId,
) -> Result<(), Error> {
    let cfg = data.guild_config.load();
    let guild = data.config.guild();

    // Build a set of the roles that mark a dues-standing member (any of the three suffices).
    let is_dues_member = {
        let presser_roles = c.member.as_ref().map(|m| m.roles.as_slice()).unwrap_or(&[]);
        let dues_role_ids: Vec<serenity::all::RoleId> = [
            cfg.member_role,
            cfg.dues_expired_role,
            cfg.dues_expiring_role,
        ]
        .into_iter()
        .flatten()
        .map(|r| serenity::all::RoleId::new(r.0))
        .collect();
        dues_role_ids.iter().any(|r| presser_roles.contains(r))
    };

    if !is_dues_member {
        tracing::debug!(
            id = presser.0,
            "banner help: presser has no dues role; ignoring"
        );
        return ack_ephemeral(ctx, c, "This button is for members with dues coming due.").await;
    }

    // Resolve the parent channel synchronously (config is already in memory) before
    // the ack so we can still send the user an error response if it is missing.
    let Some(parent_channel_id) = cfg.dues_expired_channel else {
        tracing::warn!(
            id = presser.0,
            "banner help: dues_expired_channel not configured"
        );
        return ack_ephemeral(
            ctx,
            c,
            "Something went wrong - a moderator hasn't finished setting up the bot. Please ask them directly.",
        )
        .await;
    };
    let parent_channel = ChannelId::new(parent_channel_id.0);

    // Ack first (ephemeral) - all remaining work (store, HTTP) can exceed 3 s. Mirrors
    // handle_help's ack-first model; post-ack errors are logged and not re-surfaced.
    ack_ephemeral(ctx, c, "I've let the admins know - head to your thread.").await?;

    // Look up the presser's xdate from the cache to derive the thread title.
    let record = match data.store.by_discord_id(presser).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(id = presser.0, error = %e, "banner help: cache lookup failed");
            None
        }
    };

    let today = Local::now().date_naive();
    let (xdate, status) = match record.as_ref().and_then(|r| r.expires) {
        Some(d) => (d, ExpiryStatus::from(d - today)),
        None => {
            // No xdate in the cache - treat as lapsed for title purposes and use today
            // as a placeholder cycle_xdate so set_thread can persist if needed.
            tracing::debug!(
                id = presser.0,
                "banner help: no xdate in cache; using today as placeholder"
            );
            (today, ExpiryStatus::Lapsed)
        }
    };

    // Resolve the display name for the thread title.
    let display_name = match serenity::all::GuildId::new(guild.0)
        .member(&ctx.http, c.user.id)
        .await
    {
        Ok(m) => m.display_name().to_owned(),
        Err(e) => {
            tracing::warn!(id = presser.0, error = %e, "banner help: could not fetch member for thread title; using id");
            presser.0.to_string()
        }
    };

    // Find or create the lifecycle thread. Post-ack: log and return on failure.
    let thread_channel = match ensure_lifecycle_thread(
        &ctx.http,
        &data.store,
        guild,
        parent_channel,
        presser,
        &display_name,
        status,
        xdate,
    )
    .await
    {
        Ok(ch) => ch,
        Err(e) => {
            tracing::warn!(id = presser.0, error = %e, "banner help: could not ensure lifecycle thread");
            return Ok(());
        }
    };

    tracing::debug!(
        id = presser.0,
        "banner help: posting moderator ping in thread"
    );

    // Post the moderator-role ping in the thread (same body as handle_help).
    let (content, allowed) = match cfg.moderator_role {
        Some(role) => {
            let content = format!(
                "<@&{}> - <@{}> asked for help renewing their membership.",
                role.0, presser.0
            );
            let allowed = CreateAllowedMentions::new().roles(vec![RoleId::new(role.0)]);
            (content, Some(allowed))
        }
        None => {
            tracing::warn!(
                id = presser.0,
                "banner help: no moderator_role configured; posting without role mention"
            );
            let content = format!("<@{}> asked for help renewing their membership.", presser.0);
            (content, None)
        }
    };

    let mut msg = CreateMessage::new().content(content);
    if let Some(mentions) = allowed {
        msg = msg.allowed_mentions(mentions);
    }

    if let Err(e) = thread_channel.send_message(&ctx.http, msg).await {
        tracing::warn!(id = presser.0, error = %e, "banner help: failed to post the help message in the thread");
    }

    Ok(())
}

/// Permanently opt the member out of dues reminders.
async fn handle_optout(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    guild: DiscordGuildId,
    id: DiscordUserId,
) -> Result<(), Error> {
    if let Err(e) = data.store.opt_out(guild, id, OptOutSource::Member).await {
        tracing::warn!(id = id.0, error = %e, "dues opt-out: store write failed");
        return ack_ephemeral(
            ctx,
            c,
            "Something went wrong saving that - please try again in a moment.",
        )
        .await;
    }
    tracing::debug!(id = id.0, "dues reminders opted out by member");
    ack_ephemeral(
        ctx,
        c,
        "You won't get dues reminders anymore. A moderator can turn them back on if you ask.",
    )
    .await
}

/// Post a message in the thread mentioning the configured moderator role, then
/// acknowledge the press ephemerally. If no moderator role is configured, falls
/// back to a plain message without the role mention.
async fn handle_help(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    id: DiscordUserId,
) -> Result<(), Error> {
    // Acknowledge first (ephemeral) so Discord doesn't time out the interaction.
    ack_ephemeral(
        ctx,
        c,
        "Got it - I've let the admins know. Someone will follow up with you here.",
    )
    .await?;

    let cfg = data.guild_config.load();
    let thread_id = c.channel_id;

    let (content, allowed) = match cfg.moderator_role {
        Some(role) => {
            let content = format!(
                "<@&{}> - <@{}> asked for help renewing their membership.",
                role.0, id.0
            );
            let allowed = CreateAllowedMentions::new().roles(vec![RoleId::new(role.0)]);
            (content, Some(allowed))
        }
        None => {
            tracing::warn!(
                id = id.0,
                "dues help: no moderator_role configured; posting without role mention"
            );
            let content = format!("<@{}> asked for help renewing their membership.", id.0);
            (content, None)
        }
    };

    let mut msg = CreateMessage::new().content(content);
    if let Some(mentions) = allowed {
        msg = msg.allowed_mentions(mentions);
    }

    if let Err(e) = thread_id.send_message(&ctx.http, msg).await {
        tracing::warn!(error = %e, "dues help: failed to post the help message in the thread");
    }

    Ok(())
}

/// Reply ephemerally and immediately (no deferred ack needed - no long work here).
async fn ack_ephemeral(ctx: &Context, c: &ComponentInteraction, text: &str) -> Result<(), Error> {
    c.create_response(
        ctx,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .content(text),
        ),
    )
    .await?;
    Ok(())
}

/// Whether an interaction came from the one guild this bot serves.
fn in_home_guild(guild_id: Option<serenity::all::GuildId>, data: &Data) -> bool {
    guild_id.is_some_and(|g| g.get() == data.config.guild_id)
}
