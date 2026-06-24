//! `/channels` - server-manager commands for inspecting and managing channel-permission
//! snapshots and applying the membership-role terraform. Four subcommands: `check`
//! (read-only desync report), `save` (snapshot current overwrites), `restore` (roll back
//! to the latest snapshot), and `setup` (preview and apply the full terraform).

use std::time::Duration;

use serenity::all::{
    ActionRowComponent, ButtonStyle, CreateActionRow, CreateAttachment, CreateButton,
    CreateInputText, CreateInteractionResponse, CreateModal, EditInteractionResponse,
    InputTextStyle,
};

use domain::DiscordRoleId;
use engine::backends::discord::DiscordHttp;
use engine::backends::util::DiscordUserId;
use engine::channels::{
    ApplyOutcome, ChannelsError, DesyncReport, RestoreOutcome, SetupConfig, detail_markdown,
    summary_lines, unverified_visibility,
};
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
    subcommands("check", "save", "restore", "setup"),
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
    // Defer first: reading the guild's channels + roles can exceed Discord's 3-second
    // initial-response deadline on a large guild, which would expire the interaction and
    // fail the reply. Deferring buys the followup window.
    ctx.defer_ephemeral().await?;
    let discord = channels_http(&ctx);
    let data = ctx.data();
    let channels = engine::channels::Channels::new(&discord, &*data.store);

    match channels.check().await {
        Ok(report) => {
            let content = format_desync_report(&report);
            // Discord caps message content at 2000 chars; a guild with many out-of-sync
            // channels overflows it, so fall back to a short summary plus the full list as a
            // file attachment (the same approach as /channels setup's plan).
            if content.len() <= 1900 {
                ctx.send(ephemeral_text(&content)).await?;
            } else {
                let summary = format!(
                    "{} channel(s) out of sync with their category - full list attached.",
                    report.out_of_sync.len()
                );
                let attachment = CreateAttachment::bytes(content.into_bytes(), "channel-desync.md");
                ctx.send(
                    poise::CreateReply::default()
                        .content(summary)
                        .attachment(attachment)
                        .ephemeral(true),
                )
                .await?;
            }
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
        for e in &report.out_of_sync {
            lines.push(format!(
                "- #{} ({}) -> category {} ({})",
                e.child_name, e.child_id.0, e.parent_name, e.parent_id.0
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
    // Defer first: snapshotting reads the whole guild, which can exceed Discord's
    // 3-second initial-response deadline on a large guild (see `check`).
    ctx.defer_ephemeral().await?;
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

// ---------------------------------------------------------------------------
// /channels setup
// ---------------------------------------------------------------------------

/// Button and modal ids for the setup confirm flow.
const SETUP_CONFIRM_ID: &str = "channels_setup_confirm";
const SETUP_CANCEL_ID: &str = "channels_setup_cancel";
const SETUP_COUNT_MODAL_ID: &str = "channels_setup_count_modal";
const SETUP_COUNT_FIELD_ID: &str = "channels_setup_count";

/// How long to wait for the moderator to click Confirm on the setup preview.
const SETUP_CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for the moderator to submit the count modal.
const SETUP_MODAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Preview and apply the channel-permission terraform. Requires typing the write count to confirm.
#[poise::command(slash_command)]
pub async fn setup(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;

    let data = ctx.data();
    let guild_id = ctx.guild_id().expect("channels is guild_only").get();
    let bot_user = DiscordUserId(ctx.framework().bot_id.get());
    let guild_cfg = data.guild_config.load();

    // Build the SetupConfig from the live GuildConfig.
    let cfg = match build_setup_config(&guild_cfg, guild_id, bot_user) {
        Ok(c) => c,
        Err(msg) => {
            ctx.send(ephemeral_text(&msg)).await?;
            return Ok(());
        }
    };

    let discord = channels_http(&ctx);
    let channels = engine::channels::Channels::new(&discord, &*data.store);

    // Plan: read-only, validates config before any writes.
    let plan = match channels.plan(&cfg).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "channels setup: plan failed");
            ctx.send(ephemeral_text(channels_err_msg(&e))).await?;
            return Ok(());
        }
    };

    // Nothing to do - already in the desired state.
    if plan.counts.writes == 0 {
        ctx.send(ephemeral_text(
            "Already in the desired state - nothing to change.",
        ))
        .await?;
        return Ok(());
    }

    // Build the summary embed from summary_lines + unverified_visibility headline.
    let visible = unverified_visibility(&plan, &cfg);
    let visibility_line = if visible.is_empty() {
        "Unverified role will have no channel access after apply.".to_owned()
    } else {
        format!(
            "Unverified can still see: {}",
            visible
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let mut embed = serenity::all::CreateEmbed::new()
        .title(format!(
            "Channel terraform preview - {} write(s)",
            plan.counts.writes
        ))
        .description(visibility_line)
        .color(serenity::all::Color::GOLD);
    for (label, value) in summary_lines(&plan) {
        embed = embed.field(label, value, true);
    }

    // Build the markdown attachment.
    let md_bytes = detail_markdown(&plan, &cfg).into_bytes();
    let attachment = CreateAttachment::bytes(md_bytes, "channel-plan.md");

    let expected_writes = plan.counts.writes;

    // Send the preview with the Confirm / Cancel button row.
    let handle = ctx
        .send(
            poise::CreateReply::default()
                .embed(embed)
                .attachment(attachment)
                .ephemeral(true)
                .components(vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(SETUP_CONFIRM_ID)
                        .label("Confirm")
                        .style(ButtonStyle::Primary),
                    CreateButton::new(SETUP_CANCEL_ID)
                        .label("Cancel")
                        .style(ButtonStyle::Secondary),
                ])]),
        )
        .await?;

    let sctx = ctx.serenity_context();
    let message = handle.message().await?;

    // Await the button press.
    let press = match message
        .await_component_interaction(sctx)
        .author_id(ctx.author().id)
        .timeout(SETUP_CONFIRM_TIMEOUT)
        .await
    {
        Some(p) => p,
        None => {
            handle
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Setup cancelled (timed out).")
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    if press.data.custom_id == SETUP_CANCEL_ID {
        // Acknowledge and clear the buttons.
        press
            .create_response(sctx, CreateInteractionResponse::Acknowledge)
            .await?;
        press
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .content("Setup cancelled.")
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

    // SETUP_CONFIRM_ID: open a modal asking the moderator to type the write count.
    let count_modal = CreateModal::new(SETUP_COUNT_MODAL_ID, "Confirm apply").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                format!("Type the write count ({expected_writes}) to confirm"),
                SETUP_COUNT_FIELD_ID,
            )
            .placeholder(expected_writes.to_string())
            .required(true)
            .min_length(1)
            .max_length(6),
        ),
    ]);

    press
        .create_response(sctx, CreateInteractionResponse::Modal(count_modal))
        .await?;

    // Await the modal submission. A dismissed modal sends no event so we also have a timeout.
    let submit = match message
        .await_modal_interaction(sctx)
        .author_id(ctx.author().id)
        .custom_ids(vec![SETUP_COUNT_MODAL_ID.to_owned()])
        .timeout(SETUP_MODAL_TIMEOUT)
        .await
    {
        Some(s) => s,
        None => {
            handle
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Setup cancelled (timed out waiting for count).")
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    // Acknowledge immediately so the writes below cannot exceed Discord's 3-second deadline.
    submit
        .create_response(sctx, CreateInteractionResponse::Acknowledge)
        .await?;

    // Read and parse the typed count.
    let raw_count = submit
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            ActionRowComponent::InputText(input) if input.custom_id == SETUP_COUNT_FIELD_ID => {
                input.value.clone()
            }
            _ => None,
        })
        .unwrap_or_default();

    let typed: usize = match raw_count.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            submit
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .content(format!(
                            "Count mismatch - the plan writes {expected_writes}; \
                             \"{raw_count}\" is not a valid number. Re-run /channels setup."
                        ))
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
    };

    if typed != expected_writes {
        submit
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .content(format!(
                        "Count mismatch - the plan writes {expected_writes}; you typed {typed}. \
                         Re-run /channels setup."
                    ))
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

    // Show immediate progress: the apply re-reads the guild, takes an auto-snapshot, and
    // writes each channel sequentially, which can run for several seconds on a large guild.
    // Without this the moderator sees the unchanged preview after submitting and assumes
    // nothing happened (and if the apply ever stalls, it stays visibly on this message).
    submit
        .edit_response(
            sctx,
            EditInteractionResponse::new()
                .content(format!(
                    "Applying the channel terraform to {expected_writes} channel(s) - this can take a moment...",
                ))
                .components(vec![]),
        )
        .await?;

    // Apply. The confirmed preview `plan` is passed back so apply can verify the
    // freshly-resolved write-set still matches it, not merely its count.
    let outcome: ApplyOutcome = match channels.apply(&cfg, &plan, &NoProgress).await {
        Ok(o) => o,
        Err(ChannelsError::PlanChanged { .. }) => {
            submit
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .content("The server changed since the preview - re-run /channels setup.")
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
        Err(ChannelsError::CircuitBreaker { .. }) => {
            submit
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .content(
                            "Aborted after repeated write failures. \
                             The pre-run snapshot is intact - use /channels restore.",
                        )
                        .components(vec![]),
                )
                .await?;
            return Ok(());
        }
        Err(e) => {
            tracing::warn!(error = %e, "channels setup: apply failed");
            submit
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

    submit
        .edit_response(
            sctx,
            EditInteractionResponse::new()
                .content(format!(
                    "Applied: wrote {written}, failed {failed}.",
                    written = outcome.written,
                    failed = outcome.failed,
                ))
                .components(vec![]),
        )
        .await?;

    Ok(())
}
