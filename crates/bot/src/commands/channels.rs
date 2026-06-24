//! `/channels` - server-manager commands for inspecting and managing channel-permission
//! snapshots. Three subcommands: `check` (read-only desync report), `save` (snapshot
//! current overwrites), and `restore` (roll back to the latest snapshot). A fourth
//! subcommand (`setup`) is added in a later task.

use std::time::Duration;

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateInteractionResponse, EditInteractionResponse,
};

use domain::DiscordRoleId;
use engine::backends::discord::DiscordHttp;
use engine::backends::util::DiscordUserId;
use engine::channels::{ChannelsError, DesyncReport, RestoreOutcome, SetupConfig};
use engine::seam::NoProgress;
use engine::store::GuildConfig;

use crate::data::{Context, Error};

/// Button ids for the restore confirm flow.
const RESTORE_CONFIRM_ID: &str = "channels_restore_confirm";
const RESTORE_CANCEL_ID: &str = "channels_restore_cancel";

/// How long to wait for the moderator to click confirm on the restore prompt.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Parent command
// ---------------------------------------------------------------------------

/// Inspect and manage channel-permission snapshots. Server managers only.
#[poise::command(
    slash_command,
    subcommands("check", "save", "restore"),
    default_member_permissions = "MANAGE_GUILD",
    guild_only
)]
pub async fn channels(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a channel-capable `DiscordHttp` using the gateway's shared `Http` handle. Channel
/// reads and overwrite writes need only http + guild; the managed-role list is empty because
/// this path never touches membership roles.
fn channels_http(ctx: &Context<'_>) -> DiscordHttp {
    let data = ctx.data();
    let guild_id = ctx.guild_id().expect("channels is guild_only").get();
    DiscordHttp::from_http(
        data.http.clone(),
        serenity::all::GuildId::new(guild_id),
        vec![],
    )
}

/// Build a `SetupConfig` from the live `GuildConfig`. Returns a friendly error string when
/// any of the four required roles is unset, so the caller can surface it to the moderator.
/// Used by the `setup` subcommand (Task 11).
#[allow(dead_code)]
pub(crate) fn build_setup_config(
    cfg: &GuildConfig,
    guild_id: u64,
    bot_user: DiscordUserId,
) -> Result<SetupConfig, String> {
    let member_role = cfg
        .member_role
        .ok_or("The Member role is not configured - run /setup first.")?;
    let dues_expired_role = cfg
        .dues_expired_role
        .ok_or("The Dues-expired role is not configured - run /setup first.")?;
    let unverified_role = cfg
        .unverified_role
        .ok_or("The Unverified role is not configured - run /setup first.")?;
    let moderator_role = cfg
        .moderator_role
        .ok_or("The Moderator role is not configured - run /setup first.")?;

    let mut unverified_channels = std::collections::BTreeSet::new();
    if let Some(ch) = cfg.unverified_channel {
        unverified_channels.insert(ch);
    }

    let mut dues_expired_channels = std::collections::BTreeSet::new();
    if let Some(ch) = cfg.dues_expired_channel {
        dues_expired_channels.insert(ch);
    }

    // Exclude configured mod channels as defense in depth so the sweep never touches them.
    let mut exclude_channels = std::collections::BTreeSet::new();
    if let Some(ch) = cfg.mod_approval_channel {
        exclude_channels.insert(ch);
    }
    if let Some(ch) = cfg.verification_log_channel {
        exclude_channels.insert(ch);
    }

    Ok(SetupConfig {
        everyone: DiscordRoleId(guild_id),
        member_role,
        dues_expired_role,
        unverified_role,
        moderator_role,
        bot_user,
        unverified_channels,
        dues_expired_channels,
        exclude_channels,
    })
}

/// Map a `ChannelsError` to a one-sentence user-facing message. Never exposes internals.
fn channels_err_msg(e: &ChannelsError) -> &'static str {
    match e {
        ChannelsError::NoUnverifiedChannel => {
            "No verification channel is configured - run /setup first."
        }
        ChannelsError::ChannelSetsOverlap => {
            "The unverified and dues-expired channels overlap - fix /setup before running this."
        }
        ChannelsError::VerificationBreach(_) => {
            "The plan would hide the verification channel from new members - aborting."
        }
        ChannelsError::PlanChanged { .. } => {
            "The server changed between the preview and the apply - please try again."
        }
        ChannelsError::CircuitBreaker { .. } => {
            "Aborted after repeated write failures; the pre-run snapshot is intact."
        }
        ChannelsError::NoSnapshot => "No saved snapshot to restore from.",
        ChannelsError::Discord(_) => "Something went wrong talking to Discord.",
        ChannelsError::Snapshot(_) => "Something went wrong with the database.",
    }
}

fn ephemeral_text(content: &str) -> poise::CreateReply {
    poise::CreateReply::default()
        .content(content.to_owned())
        .ephemeral(true)
}

// ---------------------------------------------------------------------------
// /channels check
// ---------------------------------------------------------------------------

/// Show which channels are out of sync with their category overwrites.
#[poise::command(slash_command)]
pub async fn check(ctx: Context<'_>) -> Result<(), Error> {
    let discord = channels_http(&ctx);
    let data = ctx.data();
    let channels = engine::channels::Channels::new(&discord, &*data.store);

    match channels.check().await {
        Ok(report) => {
            let content = format_desync_report(&report);
            ctx.send(ephemeral_text(&content)).await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "channels check failed");
            ctx.send(ephemeral_text(channels_err_msg(&e))).await?;
        }
    }
    Ok(())
}

fn format_desync_report(report: &DesyncReport) -> String {
    if report.out_of_sync.is_empty() {
        "All channels are in sync with their category overwrites.".to_owned()
    } else {
        let mut lines = vec![format!(
            "{} channel(s) out of sync with their category:",
            report.out_of_sync.len()
        )];
        for (child_id, name, parent_id) in &report.out_of_sync {
            lines.push(format!(
                "- #{name} ({}) -> category {}",
                child_id.0, parent_id.0
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// /channels save
// ---------------------------------------------------------------------------

/// Snapshot the current channel overwrites so they can be restored later.
#[poise::command(slash_command)]
pub async fn save(ctx: Context<'_>) -> Result<(), Error> {
    let discord = channels_http(&ctx);
    let data = ctx.data();
    let channels = engine::channels::Channels::new(&discord, &*data.store);

    match channels.save().await {
        Ok(snap) => {
            let count = snap.channels.len();
            let when = snap.saved_at.format("%Y-%m-%d %H:%M UTC");
            ctx.send(ephemeral_text(&format!(
                "Snapshot saved: {count} channel{} at {when}.",
                if count == 1 { "" } else { "s" },
            )))
            .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "channels save failed");
            ctx.send(ephemeral_text(channels_err_msg(&e))).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// /channels restore
// ---------------------------------------------------------------------------

/// Restore channel overwrites from the latest saved snapshot.
#[poise::command(slash_command)]
pub async fn restore(ctx: Context<'_>) -> Result<(), Error> {
    let sctx = ctx.serenity_context();

    // Send the confirm prompt as an ephemeral message with two buttons.
    let handle = ctx
        .send(
            poise::CreateReply::default()
                .content(
                    "This will overwrite every channel's permissions with the last saved snapshot.\n\
                     Are you sure?",
                )
                .ephemeral(true)
                .components(vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(RESTORE_CONFIRM_ID)
                        .label("Yes, restore")
                        .style(ButtonStyle::Danger),
                    CreateButton::new(RESTORE_CANCEL_ID)
                        .label("Cancel")
                        .style(ButtonStyle::Secondary),
                ])]),
        )
        .await?;

    let message = handle.message().await?;
    let press = match message
        .await_component_interaction(sctx)
        .author_id(ctx.author().id)
        .timeout(CONFIRM_TIMEOUT)
        .await
    {
        Some(p) => p,
        None => {
            // Timed out - clean up the buttons and exit.
            handle
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Restore cancelled (timed out).")
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    // Acknowledge immediately so Discord knows the interaction was handled within 3 seconds.
    press
        .create_response(sctx, CreateInteractionResponse::Acknowledge)
        .await?;

    if press.data.custom_id == RESTORE_CANCEL_ID {
        press
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .content("Restore cancelled.")
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

    // RESTORE_CONFIRM_ID: fetch the snapshot and apply it.
    let discord = channels_http(&ctx);
    let data = ctx.data();
    let channels = engine::channels::Channels::new(&discord, &*data.store);

    let snap = match channels.latest_snapshot().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "channels restore: latest_snapshot failed");
            press
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .content(channels_err_msg(&e))
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    let outcome: RestoreOutcome = match channels.restore(&snap, &NoProgress).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "channels restore failed");
            press
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .content(channels_err_msg(&e))
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    let mut summary = format!(
        "Restored {written} channel{}, skipped {skipped} (already matching)",
        if outcome.written == 1 { "" } else { "s" },
        written = outcome.written,
        skipped = outcome.skipped_no_op,
    );
    if outcome.failed > 0 {
        summary.push_str(&format!(", {} failed - see the logs.", outcome.failed));
    } else {
        summary.push('.');
    }
    press
        .edit_response(
            sctx,
            EditInteractionResponse::new()
                .content(summary)
                .components(vec![]),
        )
        .await?;
    Ok(())
}
