//! `/reminders` - Manage-Guild-gated commands for dues-reminder configuration and testing.
//! Subcommands:
//!   `template <kind>` - open a modal to view/edit the stored template body for a kind.
//!   `preview <kind>`  - render and preview the live reminder message for a kind.
//!   `clear-optout <member>` - clear a member's permanent dues-reminder opt-out.
//!
//! The staging-only `test-send` subcommand is defined here but registered conditionally
//! in `commands/mod.rs` when `BOT_REMINDER_TEST_SEND` is set.

use std::time::Duration;

use serenity::all::{ChannelId, CreateActionRow, User};

use domain::DiscordGuildId;
use engine::backends::solidarity_tech::MembershipType;
use engine::backends::util::DiscordUserId;
use engine::reminders::{Milestone, ReminderTemplateKind};
use engine::store::{MemberStore, ReminderStore, ReminderTemplates};

use crate::data::{Context, Data, Error};
use crate::render::reminders::{default_template, reminder_buttons, reminder_embed};

// ---------------------------------------------------------------------------
// ChoiceParameter types
// ---------------------------------------------------------------------------

/// Poise choice parameter for the five reminder template kinds.
#[derive(Debug, poise::ChoiceParameter)]
pub enum TemplateKindChoice {
    #[name = "Monthly"]
    Monthly,
    #[name = "Yearly"]
    Yearly,
    #[name = "One-time"]
    OneTime,
    #[name = "Income-based"]
    IncomeBased,
    #[name = "Expired"]
    Expired,
}

impl From<TemplateKindChoice> for ReminderTemplateKind {
    fn from(c: TemplateKindChoice) -> Self {
        match c {
            TemplateKindChoice::Monthly => ReminderTemplateKind::Monthly,
            TemplateKindChoice::Yearly => ReminderTemplateKind::Yearly,
            TemplateKindChoice::OneTime => ReminderTemplateKind::OneTime,
            TemplateKindChoice::IncomeBased => ReminderTemplateKind::IncomeBased,
            TemplateKindChoice::Expired => ReminderTemplateKind::Expired,
        }
    }
}

/// Poise choice parameter for the four reminder milestones.
#[derive(Debug, Clone, Copy, poise::ChoiceParameter)]
pub enum MilestoneChoice {
    #[name = "30 days out"]
    Days30,
    #[name = "14 days out"]
    Days14,
    #[name = "1 day out"]
    Day1,
    #[name = "Expired"]
    Expired,
}

impl From<MilestoneChoice> for Milestone {
    fn from(c: MilestoneChoice) -> Self {
        match c {
            MilestoneChoice::Days30 => Milestone::Days30,
            MilestoneChoice::Days14 => Milestone::Days14,
            MilestoneChoice::Day1 => Milestone::Day1,
            MilestoneChoice::Expired => Milestone::Expired,
        }
    }
}

// ---------------------------------------------------------------------------
// Modal
// ---------------------------------------------------------------------------

/// The template-edit modal. A single required paragraph field pre-filled with the
/// stored or default body so the moderator can see and edit it in one step.
#[derive(Debug, poise::Modal)]
#[name = "Edit reminder template"]
struct TemplateModal {
    #[name = "Template body"]
    #[placeholder = "Enter the reminder message body..."]
    #[paragraph]
    #[min_length = 1]
    #[max_length = 1000]
    body: String,
}

// ---------------------------------------------------------------------------
// Parent command
// ---------------------------------------------------------------------------

/// Manage dues-reminder templates, previews, and opt-outs. Server managers only.
#[poise::command(
    slash_command,
    subcommands("template", "preview", "clear_optout"),
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

// The reminders config keys on the configured home guild - matching the sweep, /grace, and the
// member buttons (on_component) - not the interaction's guild_id, so a write can never land under
// a guild the sweep never reads. Both helpers exist only because the two command flavors carry
// different context types.
fn guild(ctx: &poise::ApplicationContext<'_, Data, Error>) -> DiscordGuildId {
    ctx.data().config.guild()
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
// /reminders template <kind>
// ---------------------------------------------------------------------------

/// View and edit the stored template body for a reminder kind.
///
/// Opens a pre-filled modal with the current template (or the built-in default
/// when no custom body is stored). Submitting saves the new body.
#[poise::command(slash_command)]
pub async fn template(
    ctx: poise::ApplicationContext<'_, Data, Error>,
    #[description = "Which template to edit"] kind: TemplateKindChoice,
) -> Result<(), Error> {
    if !ensure_can_manage(&poise::Context::Application(ctx)).await? {
        return Ok(());
    }
    let data = ctx.data();
    let guild_id = guild(&ctx);
    let kind: ReminderTemplateKind = kind.into();

    // Fetch the stored body (fast Postgres-localhost call) before responding so we
    // can prefill the modal. Discord's 3-second interaction window is normally ample
    // for a single indexed row read.
    let stored = data.store.template(guild_id, kind).await?;
    let prefill_body = stored
        .as_deref()
        .unwrap_or_else(|| default_template(kind))
        .to_owned();

    let defaults = TemplateModal { body: prefill_body };

    let submit = poise::execute_modal(ctx, Some(defaults), Some(Duration::from_secs(300))).await?;

    let Some(modal) = submit else {
        // Dismissed / timed out - nothing to save.
        return Ok(());
    };

    let new_body = modal.body.trim().to_owned();
    if new_body.is_empty() {
        // The min_length=1 constraint makes this unreachable via the UI, but guard it.
        return Ok(());
    }

    data.store.set_template(guild_id, kind, new_body).await?;
    tracing::debug!(kind = kind.as_token(), "reminder template updated");

    // Send a confirmation. The modal already closed via the execute_modal acknowledgement;
    // this is a follow-up ephemeral message.
    ctx.send(ephemeral_text(&format!(
        "Template for **{}** saved.",
        kind.as_token()
    )))
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// /reminders preview <kind>
// ---------------------------------------------------------------------------

/// Preview the rendered reminder message for a template kind.
///
/// Sends an ephemeral preview with the stored or default body and placeholder data,
/// so moderators can check the layout before the sweep fires.
#[poise::command(slash_command)]
pub async fn preview(
    ctx: Context<'_>,
    #[description = "Which reminder kind to preview"] kind: TemplateKindChoice,
) -> Result<(), Error> {
    if !ensure_can_manage(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let guild_id = guild_ctx(&ctx);
    let kind: ReminderTemplateKind = kind.into();

    let body: String = match data.store.template(guild_id, kind).await? {
        Some(custom) => custom,
        None => default_template(kind).to_owned(),
    };

    // Use placeholder values: days_until = 14 (except Expired), milestone = Days14.
    // The previewing moderator will understand these are example values.
    let (days_until, milestone) = if kind == ReminderTemplateKind::Expired {
        (-1i64, Milestone::Expired)
    } else {
        (14i64, Milestone::Days14)
    };

    let cfg = data.guild_config.load();
    let member_id = ctx.author().id;

    let embed = reminder_embed(&body, days_until, data.config.accent_color);
    // The preview's action buttons are disabled so a moderator checking the layout cannot
    // fire their own snooze / opt-out / ask-an-admin handlers.
    let buttons = reminder_buttons(milestone, cfg.dues_signup_url.as_deref(), member_id, true);

    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "Preview for **{}** template (placeholder data):",
                kind.as_token()
            ))
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(buttons)])
            .ephemeral(true),
    )
    .await?;

    Ok(())
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
/// flow can be verified against a staging account. Requires `dues_reminder_channel`
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

    // Require a configured dues reminder channel.
    let parent_channel = match cfg.dues_reminder_channel {
        Some(ch) => ChannelId::new(ch.0),
        None => {
            ctx.send(ephemeral_text(
                "No dues-reminder channel is configured. \
                 Set one in /setup before using this command.",
            ))
            .await?;
            return Ok(());
        }
    };

    let member_id = DiscordUserId(member.id.get());
    let milestone: Milestone = milestone.into();

    // Derive days_until from the milestone's lead days; Expired uses -1.
    let days_until: i64 = milestone.lead_days().unwrap_or(-1);

    // Read membership_type from the cached record; fall back to Monthly when absent
    // so the test still fires and uses a real (if arbitrary) template.
    let membership_type: Option<MembershipType> = match data.store.by_discord_id(member_id).await {
        Ok(Some(record)) => record.membership_type,
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
        guild: guild_id,
        today: chrono::Utc::now().date_naive(),
        parent_channel,
        signup_url: cfg.dues_signup_url.as_deref(),
        catchup_gap: data.config.reminder_catchup_gap,
        pace: data.config.scan_pace,
        accent: data.config.accent_color,
    };

    match crate::reminders::send_reminder(
        &reminder_ctx,
        member_id,
        milestone,
        days_until,
        membership_type,
    )
    .await
    {
        Ok(()) => {
            ctx.send(ephemeral_text(&format!(
                "Sent {} reminder to <@{}>.",
                milestone.as_token(),
                member.id.get()
            )))
            .await?;
        }
        Err(e) => {
            tracing::error!(error = %e, id = member_id.0, "test-send: send_reminder failed");
            ctx.send(ephemeral_text(
                "Failed to send the test reminder - check the logs for details.",
            ))
            .await?;
        }
    }

    Ok(())
}
