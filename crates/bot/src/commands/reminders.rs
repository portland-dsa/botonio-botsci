//! `/reminders` - Manage-Guild-gated commands for dues-reminder configuration and testing.
//! Subcommands:
//!   `clear-optout <member>` - clear a member's permanent dues-reminder opt-out.
//!
//! The staging-only `test-send` subcommand is defined here but registered conditionally
//! in `commands/mod.rs` when `BOT_REMINDER_TEST_SEND` is set.

use serenity::all::User;

use domain::DiscordGuildId;
use engine::backends::solidarity_tech::MembershipType;
use engine::backends::util::DiscordUserId;
use engine::reminders::Milestone;
use engine::store::{MemberStore, ReminderStore};

use crate::data::{Context, Error};

// ---------------------------------------------------------------------------
// ChoiceParameter types
// ---------------------------------------------------------------------------

/// Poise choice parameter for the two reminder milestones.
#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum MilestoneChoice {
    #[name = "Renewal"]
    Renewal,
    #[name = "Lapse"]
    Lapse,
}

impl From<MilestoneChoice> for Milestone {
    fn from(c: MilestoneChoice) -> Self {
        match c {
            MilestoneChoice::Renewal => Milestone::Renewal,
            MilestoneChoice::Lapse => Milestone::Lapse,
        }
    }
}

// ---------------------------------------------------------------------------
// Parent command
// ---------------------------------------------------------------------------

/// Manage dues-reminder opt-outs. Server managers only.
#[poise::command(
    slash_command,
    subcommands("clear_optout"),
    default_member_permissions = "MANAGE_GUILD",
    guild_only
)]
pub async fn reminders(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ephemeral_text(content: &str) -> poise::CreateReply {
    poise::CreateReply::default()
        .content(content.to_owned())
        .ephemeral(true)
}

fn guild_ctx(ctx: &Context<'_>) -> DiscordGuildId {
    ctx.data().config.guild()
}

/// Manage-Guild gate for the reminders subcommands. Returns `Ok(true)` when the invoker may
/// proceed; otherwise sends the refusal and returns `Ok(false)`. `default_member_permissions`
/// is only a client-side hint an admin can override, so - mirroring /setup and /grace - this
/// in-code check is the real gate.
async fn ensure_can_manage(ctx: &Context<'_>) -> Result<bool, Error> {
    if crate::commands::setup::invoker_can_configure(ctx).await {
        return Ok(true);
    }
    ctx.send(ephemeral_text("That command is for server managers only."))
        .await?;
    Ok(false)
}

// ---------------------------------------------------------------------------
// /reminders clear-optout <member>
// ---------------------------------------------------------------------------

/// Clear a member's permanent dues-reminder opt-out.
///
/// After clearing, the member will receive dues reminders again on the next sweep.
#[poise::command(slash_command, rename = "clear-optout")]
pub async fn clear_optout(
    ctx: Context<'_>,
    #[description = "The member whose opt-out to clear"] member: User,
) -> Result<(), Error> {
    if !ensure_can_manage(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let guild_id = guild_ctx(&ctx);
    let member_id = DiscordUserId(member.id.get());

    data.store.clear_opt_out(guild_id, member_id).await?;
    tracing::debug!(
        id = member_id.0,
        "dues reminder opt-out cleared by moderator"
    );

    ctx.send(ephemeral_text(&format!(
        "Cleared dues-reminder opt-out for <@{}>. They will receive reminders again.",
        member.id.get()
    )))
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// /reminders test-send <member> <milestone>  (staging-only)
// ---------------------------------------------------------------------------

/// Send a test dues reminder to one member for a given milestone.
///
/// Fires the real thread-create + message + button path so the full reminder
/// flow can be verified against a staging account. Requires `dues_expired_channel`
/// to be configured in /setup.
///
/// Registered only when `BOT_REMINDER_TEST_SEND` is set.
#[poise::command(slash_command, default_member_permissions = "MANAGE_GUILD", guild_only)]
pub async fn test_send(
    ctx: Context<'_>,
    #[description = "The member to send the test reminder to"] member: User,
    #[description = "Which milestone to send"] milestone: MilestoneChoice,
) -> Result<(), Error> {
    if !ensure_can_manage(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let guild_id = guild_ctx(&ctx);
    let cfg = data.guild_config.load();

    // Require a configured dues-expired channel.
    let parent_channel = match cfg.dues_expired_channel {
        Some(ch) => serenity::all::ChannelId::new(ch.0),
        None => {
            ctx.send(ephemeral_text(
                "No dues-expired channel is configured. \
                 Set one in /setup before using this command.",
            ))
            .await?;
            return Ok(());
        }
    };

    let Some(discord) =
        crate::guild_config::build_role_writer(data.http.clone(), data.config.guild_id, &cfg)
    else {
        ctx.send(ephemeral_text(
            "Managed roles are not fully configured. Set them in /setup first.",
        ))
        .await?;
        return Ok(());
    };

    let member_id = DiscordUserId(member.id.get());
    let milestone: Milestone = milestone.into();
    let today = chrono::Utc::now().date_naive();

    // xdate: 14 days out for Renewal (inside the window), yesterday for Lapse.
    let xdate = match milestone {
        Milestone::Renewal => today + chrono::Duration::days(14),
        Milestone::Lapse => today - chrono::Duration::days(1),
    };

    // Read membership_type from the cached record; fall back to Monthly when absent
    // so the test still fires and uses a real (if arbitrary) template.
    let membership_type: Option<MembershipType> = match data.store.by_discord_id(member_id).await {
        Ok(Some(record)) => record.membership_type.or(Some(MembershipType::Monthly)),
        Ok(None) => {
            tracing::debug!(
                id = member_id.0,
                "test-send: no cached record; using Monthly as fallback"
            );
            Some(MembershipType::Monthly)
        }
        Err(e) => {
            tracing::warn!(error = %e, "test-send: store lookup failed; using Monthly as fallback");
            Some(MembershipType::Monthly)
        }
    };

    ctx.defer_ephemeral().await?;

    let reminder_ctx = crate::reminders::ReminderCtx {
        http: data.http.as_ref(),
        store: data.store.as_ref(),
        discord: &discord,
        guild: guild_id,
        today,
        parent_channel,
        signup_url: cfg.dues_signup_url.as_deref(),
        pace: data.config.scan_pace,
        accent: data.config.accent_color,
    };

    match crate::reminders::send_notice(&reminder_ctx, member_id, milestone, membership_type, xdate)
        .await
    {
        Ok(()) => {
            ctx.send(ephemeral_text(&format!(
                "Sent {} notice to <@{}>.",
                milestone.as_token(),
                member.id.get()
            )))
            .await?;
        }
        Err(e) => {
            tracing::error!(error = %e, id = member_id.0, "test-send: send_notice failed");
            ctx.send(ephemeral_text(
                "Failed to send the test reminder - check the logs for details.",
            ))
            .await?;
        }
    }

    Ok(())
}
