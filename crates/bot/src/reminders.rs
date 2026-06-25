//! Dues-reminder sweep: on each scan pass, send each due member their milestone
//! message into a private thread off the configured dues-reminder channel.
//!
//! The sweep and the per-member [`send_reminder`] helper are separated so the
//! staging test-send command can drive a single member without running the full
//! planner. Both receive a [`ReminderCtx`] carrying the stable per-pass context.
//!
//! This module also owns the persistent button handler ([`on_component`]) for the
//! snooze / opt-out / ask-an-admin actions embedded in each reminder message.

use std::time::Duration;

use chrono::{NaiveDate, Utc};
use domain::DiscordGuildId;
use engine::backends::solidarity_tech::MembershipType;
use engine::backends::util::DiscordUserId;
use engine::reminders::{Milestone, ReminderTemplateKind};
use engine::store::{OptOutSource, ReminderStore, ReminderTemplates};
use persistence::PgStore;
use serenity::all::{
    AutoArchiveDuration, ChannelId, ChannelType, ComponentInteraction, Context,
    CreateAllowedMentions, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, CreateThread, Http, RoleId, UserId,
};

use crate::data::{Data, Error};
use crate::render::reminders::{
    DUES_HELP_ID, DUES_OPTOUT_ID, DUES_SNOOZE_ID, default_template, reminder_message,
};

// ---- context ------------------------------------------------------------

/// Stable per-pass context threaded from the scan loop. The staging test-send
/// command constructs one directly to drive a single member.
pub struct ReminderCtx<'a> {
    pub http: &'a Http,
    pub store: &'a PgStore,
    pub guild: DiscordGuildId,
    pub today: NaiveDate,
    pub parent_channel: ChannelId,
    pub signup_url: Option<&'a str>,
    pub catchup_gap: Duration,
    pub pace: Duration,
    pub accent: u32,
}

// ---- per-member helper --------------------------------------------------

/// Send one member their dues reminder. Creates or reuses their private thread,
/// resolves the correct template body, sends the message, and records the send.
///
/// Returns an error on Discord API or store failure; the sweep loop treats each
/// such error as a skip and continues.
pub async fn send_reminder(
    ctx: &ReminderCtx<'_>,
    member: DiscordUserId,
    milestone: Milestone,
    days_until: i64,
    membership_type: Option<MembershipType>,
) -> Result<(), crate::error::BotError> {
    // Resolve the template kind. Expired is fixed; pre-lapse nudges key on
    // membership type. Skip with a warning when the type is absent for a
    // pre-lapse milestone - the right template cannot be chosen without it.
    let kind = if milestone == Milestone::Expired {
        ReminderTemplateKind::Expired
    } else {
        match membership_type {
            Some(t) => ReminderTemplateKind::from_membership_type(t),
            None => {
                tracing::warn!(
                    id = member.0,
                    milestone = milestone.as_token(),
                    "skipping reminder: pre-lapse milestone but membership_type is absent"
                );
                return Ok(());
            }
        }
    };

    // Ensure the member has a thread: reuse the stored id, or create a new
    // private thread off the parent channel and add the member.
    let state = ctx.store.reminder_state(ctx.guild, member).await?;
    let serenity_user = UserId::new(member.0);

    // Recompute xdate from ctx.today + days_until so the invariant is
    // structurally enforced rather than left to the caller.
    let xdate = match ctx
        .today
        .checked_add_signed(chrono::Duration::days(days_until))
    {
        Some(d) => d,
        None => {
            tracing::warn!(
                id = member.0,
                "skipping reminder: xdate overflowed (days_until={})",
                days_until
            );
            return Ok(());
        }
    };

    let thread_id: i64 = match state.and_then(|s| s.thread_id) {
        Some(tid) => tid,
        None => {
            // Name is stable and PII-free; the member is added by id, not
            // named in the title.
            let thread = ctx
                .parent_channel
                .create_thread(
                    ctx.http,
                    CreateThread::new("Dues renewal")
                        .kind(ChannelType::PrivateThread)
                        .auto_archive_duration(AutoArchiveDuration::OneWeek)
                        .invitable(false),
                )
                .await?;

            thread.id.add_thread_member(ctx.http, serenity_user).await?;

            // Discord snowflakes fit in 63 bits; a wrapping cast is lossless
            // and the reverse cast (thread_id as u64) recovers the exact value.
            let tid = thread.id.get() as i64;

            // Persist the new thread id before the message goes out, so a later send or
            // record_sent failure reuses this thread on the next sweep instead of orphaning it
            // and creating a duplicate.
            ctx.store.set_thread(ctx.guild, member, xdate, tid).await?;
            tid
        }
    };

    let thread_channel = ChannelId::new(thread_id as u64);

    // Resolve body: stored moderator template, else the built-in default.
    let body: String = match ctx.store.template(ctx.guild, kind).await? {
        Some(custom) => custom,
        None => default_template(kind).to_owned(),
    };

    let msg = reminder_message(
        &body,
        days_until,
        milestone,
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
/// Reads `last_reminder_run`, plans which members are due a reminder today,
/// delivers each one paced, and records the run time. A per-member failure is
/// logged and skipped; the sweep continues and marks completion so a later pass
/// does not double-fire.
///
/// `info!` carries aggregate counts only; `debug!` carries the member id with
/// no other PII.
pub async fn run_reminder_sweep(ctx: ReminderCtx<'_>) {
    let now = Utc::now();

    // Timely: first-ever run (no stored timestamp) counts as timely. A gap
    // larger than the catchup threshold means the bot missed one or more
    // crossings, so the planner uses round-to-nearest logic instead.
    let timely = match store_last_run(ctx.store, ctx.guild).await {
        Some(last) => {
            let gap = now
                .signed_duration_since(last)
                .to_std()
                .unwrap_or(Duration::MAX);
            gap <= ctx.catchup_gap
        }
        None => true,
    };

    let plan = match engine::reminders::plan(ctx.store, ctx.guild, ctx.today, timely).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "reminder sweep: planning failed; skipping pass");
            return;
        }
    };

    let total = plan.due.len();
    tracing::info!(total, timely, "reminder sweep starting");

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

        match send_reminder(
            &ctx,
            due.id,
            due.milestone,
            due.days_until,
            due.membership_type,
        )
        .await
        {
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

    if let Err(e) = ctx.store.set_last_reminder_run(ctx.guild, now).await {
        tracing::error!(error = %e, "reminder sweep: failed to record last-run timestamp");
    }

    tracing::info!(sent, failed, total, "reminder sweep complete");
}

async fn store_last_run(store: &PgStore, guild: DiscordGuildId) -> Option<chrono::DateTime<Utc>> {
    match store.last_reminder_run(guild).await {
        Ok(ts) => ts,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reminder sweep: could not read last_reminder_run; treating as first run"
            );
            None
        }
    }
}

// ---- button handler -----------------------------------------------------

/// Route a dues-reminder button press (snooze / opt-out / ask-an-admin).
///
/// Ignores interactions that are not from the home guild, not a `dues_*` id, or
/// whose suffix target does not match the presser - so a moderator who joined the
/// thread cannot toggle a member's reminder state.
pub async fn on_component(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
) -> Result<(), Error> {
    if !in_home_guild(c.guild_id, data) {
        return Ok(());
    }
    let id = c.data.custom_id.as_str();
    let Some((kind, suffix)) = id.split_once(':') else {
        return Ok(());
    };
    // Only act on dues_* prefixes.
    if !matches!(kind, DUES_SNOOZE_ID | DUES_OPTOUT_ID | DUES_HELP_ID) {
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
        DUES_SNOOZE_ID => handle_snooze(ctx, c, data, guild, target_id).await,
        DUES_OPTOUT_ID => handle_optout(ctx, c, data, guild, target_id).await,
        DUES_HELP_ID => handle_help(ctx, c, data, target_id).await,
        _ => Ok(()),
    }
}

/// Snooze reminders for this cycle. Keys the snooze on the recorded `cycle_xdate` (the cycle
/// the reminder was sent for); if no state is recorded, acknowledges gracefully without writing.
async fn handle_snooze(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    guild: DiscordGuildId,
    id: DiscordUserId,
) -> Result<(), Error> {
    // Snooze the cycle the member was actually reminded about - the recorded `cycle_xdate` -
    // not the live roster `expires`, which may have moved on if the roster refreshed since the
    // reminder was sent. No recorded state means no reminder was sent, so there is nothing to
    // snooze; that falls through to a graceful acknowledgement below.
    let xdate = match data.store.reminder_state(guild, id).await {
        Ok(state) => state.map(|s| s.cycle_xdate),
        Err(e) => {
            tracing::warn!(id = id.0, error = %e, "dues snooze: state read failed");
            return ack_ephemeral(
                ctx,
                c,
                "Something went wrong saving that - please try again in a moment.",
            )
            .await;
        }
    };
    match xdate {
        Some(cycle_xdate) => {
            if let Err(e) = data.store.set_snooze(guild, id, cycle_xdate).await {
                tracing::warn!(id = id.0, error = %e, "dues snooze: store write failed");
                return ack_ephemeral(
                    ctx,
                    c,
                    "Something went wrong saving that - please try again in a moment.",
                )
                .await;
            }
            tracing::debug!(id = id.0, "dues reminder snoozed for cycle");
        }
        None => {
            tracing::debug!(
                id = id.0,
                "dues snooze: no xdate on record; skipping store write"
            );
        }
    }
    ack_ephemeral(ctx, c, "You won't get more reminders this cycle.").await
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
