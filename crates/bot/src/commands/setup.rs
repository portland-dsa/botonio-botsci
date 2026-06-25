//! `/setup` - the Manage-Server-gated guild configuration panel. Bootstraps the
//! moderator role, so it gates on Discord's Manage Guild permission rather than the
//! bot's own (not-yet-configured) moderator role. The panel is a by-feature navigator:
//! a Landing view with the current-config embed and one button per feature, opening
//! one of four pages (Verification, Membership & access, Dues reminders, Moderation).
//! Each page holds the native role/channel selects, the message editors, and the
//! toggles for that feature; a selection writes the whole config row, swaps the live
//! handle, and audits the change.
//!
//! Colour language across the pages: blurple (Primary) is an action, an edit, or a
//! navigation call to action; grey (Secondary) is neutral, Back, or an off toggle;
//! green (Success) is an on toggle.

use std::time::Duration;

use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, ChannelType, ComponentInteraction,
    ComponentInteractionCollector, ComponentInteractionDataKind, CreateActionRow, CreateButton,
    CreateEmbed, CreateInputText, CreateInteractionResponse, CreateInteractionResponseFollowup,
    CreateInteractionResponseMessage, CreateModal, CreateSelectMenu, CreateSelectMenuKind,
    EditInteractionResponse, InputTextStyle, MessageId, Permissions, RoleId,
};
use serenity::futures::StreamExt as _;

use domain::{DiscordChannelId, DiscordMessageId, DiscordRoleId};
use engine::audit::AuditLog;
use engine::backends::util::DiscordUserId;
use engine::reminders::{MessageKind, Milestone};
use engine::store::{ConfigStore, GuildConfig, MessageRef, MessageTemplates};

use crate::data::{Context, Error};
use crate::render::reminders::{banner_edit, banner_message, default_body};
use crate::render::self_verify::{verify_prompt, verify_prompt_edit};
use crate::render::setup::{dues_page_embed, landing_embed};

// Landing navigation buttons (one per feature page).
const NAV_VERIFICATION_ID: &str = "setup_nav_verification";
const NAV_MEMBERSHIP_ID: &str = "setup_nav_membership";
const NAV_DUES_ID: &str = "setup_nav_dues";
const NAV_MODERATION_ID: &str = "setup_nav_moderation";
const BACK_ID: &str = "setup_back";

// Toggles.
const SCAN_TOGGLE_ID: &str = "setup_scan_toggle";
const REMINDERS_TOGGLE_ID: &str = "setup_reminders_toggle";

// Per-setting role select-menu custom ids.
const MOD_ROLE_ID: &str = "setup_role_moderator";
const MEMBER_ROLE_ID: &str = "setup_role_member";
const DUES_ROLE_ID: &str = "setup_role_dues_expired";
const DUES_EXPIRING_ROLE_ID: &str = "setup_role_dues_expiring";
const UNVERIFIED_ROLE_ID: &str = "setup_role_unverified";
const OVERRIDE_ROLE_ID: &str = "setup_role_manual_override";

// Per-setting channel select-menu custom ids.
const MOD_CHAN_ID: &str = "setup_chan_mod_approval";
const UNVERIFIED_CHAN_ID: &str = "setup_chan_unverified";
const VERIFY_LOG_CHAN_ID: &str = "setup_chan_verify_log";
const DUES_CHAN_ID: &str = "setup_chan_dues_expired";

// Action buttons.
const POST_PROMPT_ID: &str = "setup_post_prompt";
const POST_BANNER_ID: &str = "setup_post_banner";
const DUES_URL_BUTTON_ID: &str = "setup_dues_url_button";

// Message-edit buttons (each opens a prefilled body modal for one message kind).
const EDIT_MONTHLY_ID: &str = "setup_edit_monthly";
const EDIT_YEARLY_ID: &str = "setup_edit_yearly";
const EDIT_ONETIME_ID: &str = "setup_edit_onetime";
const EDIT_INCOME_ID: &str = "setup_edit_income";
const EDIT_BANNER_ID: &str = "setup_edit_banner";
const EDIT_PROMPT_ID: &str = "setup_edit_prompt";

// Message-edit modal.
const MSG_MODAL_ID: &str = "setup_msg_modal";
const MSG_BODY_FIELD_ID: &str = "setup_msg_body";
const MSG_MODAL_TIMEOUT: Duration = Duration::from_secs(300);

// Dues sign-up URL modal.
const DUES_URL_MODAL_ID: &str = "setup_dues_url_modal";
const DUES_URL_FIELD_ID: &str = "setup_dues_url_field";
const DUES_URL_MODAL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the panel may sit idle before its collector is freed. This is an *inactivity*
/// window, reset on every interaction (see the loop in [`setup`]) - not a total lifetime.
/// An active moderator never trips it; each button press mints a fresh interaction token,
/// so there is no shorter ceiling to respect, and this only reclaims the collector once the
/// panel is abandoned.
const NAV_IDLE_TIMEOUT: Duration = Duration::from_secs(900);

/// Which feature page a re-render should redraw, so a select or action leaves the
/// moderator on the page they acted from.
#[derive(Clone, Copy)]
enum Page {
    Landing,
    Verification,
    Membership,
    Dues,
    Moderation,
}

/// Whether the invoker actually holds Manage Guild. `default_member_permissions` only
/// hides the command in the client; this is the enforced gate (mirroring the moderator
/// commands, which treat the permission as a hint and check in code).
pub async fn invoker_can_configure(ctx: &Context<'_>) -> bool {
    match ctx.author_member().await {
        Some(member) => member
            .permissions
            .is_some_and(|p| p.contains(Permissions::MANAGE_GUILD)),
        None => false,
    }
}

/// Configure the bot's roles and channels. Server managers only.
#[poise::command(slash_command, default_member_permissions = "MANAGE_GUILD")]
pub async fn setup(ctx: Context<'_>) -> Result<(), Error> {
    if !invoker_can_configure(&ctx).await {
        ctx.send(
            poise::CreateReply::default()
                .content("That command is for server managers only.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let data = ctx.data();
    let invoker = DiscordUserId(ctx.author().id.get());
    let accent = data.config.accent_color;

    let handle = ctx
        .send(
            poise::CreateReply::default()
                .embed(landing_embed(&data.guild_config.load(), accent))
                .components(landing_page())
                .ephemeral(true),
        )
        .await?;
    let msg = handle.message().await?;

    // One collector for every button and select on this ephemeral message, scoped to the
    // invoker. Each interaction navigates between pages, applies one setting, opens a modal,
    // or runs a posting/toggle action.
    //
    // No built-in `.timeout()`: serenity's collector timeout is a single sleep composed via
    // `take_until`, so it ends the stream a fixed time after it *opened*, regardless of
    // activity - which would kill the panel mid-configuration. Instead, give each wait its
    // own deadline below, so the window resets on every interaction.
    let mut stream = ComponentInteractionCollector::new(ctx.serenity_context())
        .message_id(msg.id)
        .author_id(ctx.author().id)
        .stream();

    loop {
        // Reset the idle window on each interaction. A modal's own wait happens inside the
        // arm (off this timer), and clicks that arrive during a modal are buffered, not lost.
        let interaction = match tokio::time::timeout(NAV_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(interaction)) => interaction,
            // The stream ended (shard gone), or the panel sat idle past the deadline.
            Ok(None) | Err(_) => break,
        };
        match interaction.data.custom_id.as_str() {
            // ---- navigation ------------------------------------------------
            NAV_VERIFICATION_ID => nav(&ctx, &interaction, accent, Page::Verification).await,
            NAV_MEMBERSHIP_ID => nav(&ctx, &interaction, accent, Page::Membership).await,
            NAV_DUES_ID => nav(&ctx, &interaction, accent, Page::Dues).await,
            NAV_MODERATION_ID => nav(&ctx, &interaction, accent, Page::Moderation).await,
            BACK_ID => nav(&ctx, &interaction, accent, Page::Landing).await,

            // ---- toggles ---------------------------------------------------
            SCAN_TOGGLE_ID => {
                toggle_scan(&ctx, &interaction, accent, invoker).await;
            }
            REMINDERS_TOGGLE_ID => {
                toggle_reminders(&ctx, &interaction, accent, invoker).await;
            }

            // ---- role selects (Membership page) ----------------------------
            MEMBER_ROLE_ID | DUES_ROLE_ID | DUES_EXPIRING_ROLE_ID | OVERRIDE_ROLE_ID => {
                // Acknowledge first so the persist + audit below can't blow Discord's
                // 3-second response deadline; the panel is then edited in place.
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                redraw(
                    &ctx,
                    &interaction,
                    accent,
                    Page::Membership,
                    note.as_deref(),
                )
                .await;
            }

            // ---- role select (Moderation page) -----------------------------
            MOD_ROLE_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                redraw(
                    &ctx,
                    &interaction,
                    accent,
                    Page::Moderation,
                    note.as_deref(),
                )
                .await;
            }

            // ---- role + channel selects (Verification page) ----------------
            UNVERIFIED_ROLE_ID | MOD_CHAN_ID | UNVERIFIED_CHAN_ID | VERIFY_LOG_CHAN_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                redraw(
                    &ctx,
                    &interaction,
                    accent,
                    Page::Verification,
                    note.as_deref(),
                )
                .await;
            }

            // ---- channel select (Dues reminders page) ----------------------
            DUES_CHAN_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                redraw(&ctx, &interaction, accent, Page::Dues, note.as_deref()).await;
            }

            // ---- posting actions -------------------------------------------
            POST_PROMPT_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = post_prompt(&ctx, accent).await;
                redraw(
                    &ctx,
                    &interaction,
                    accent,
                    Page::Verification,
                    note.as_deref(),
                )
                .await;
            }
            POST_BANNER_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = post_banner(&ctx, accent).await;
                redraw(&ctx, &interaction, accent, Page::Dues, note.as_deref()).await;
            }

            // ---- message-edit modals ---------------------------------------
            EDIT_MONTHLY_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::Monthly).await;
            }
            EDIT_YEARLY_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::Yearly).await;
            }
            EDIT_ONETIME_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::OneTime).await;
            }
            EDIT_INCOME_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::IncomeBased).await;
            }
            EDIT_BANNER_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::DuesBanner).await;
            }
            EDIT_PROMPT_ID => {
                edit_message(&ctx, &interaction, &msg, accent, MessageKind::Unverified).await;
            }

            // ---- dues sign-up URL modal ------------------------------------
            DUES_URL_BUTTON_ID => {
                set_dues_url(&ctx, &interaction, &msg, accent, invoker).await;
            }

            _ => {}
        }
    }
    Ok(())
}

// ====================================================================
// Page layouts
// ====================================================================

/// The Landing view's buttons: one row per pair of feature pages, all blurple.
fn landing_page() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::Buttons(vec![
            CreateButton::new(NAV_VERIFICATION_ID)
                .label("Verification")
                .style(ButtonStyle::Primary),
            CreateButton::new(NAV_MEMBERSHIP_ID)
                .label("Membership & access")
                .style(ButtonStyle::Primary),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(NAV_DUES_ID)
                .label("Dues reminders")
                .style(ButtonStyle::Primary),
            CreateButton::new(NAV_MODERATION_ID)
                .label("Moderation")
                .style(ButtonStyle::Primary),
        ]),
    ]
}

/// Verification page: unverified role + the three verification channels, then a row of
/// blurple actions (edit/post the prompt) and grey Back. Five rows, at Discord's cap.
fn verification_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(UNVERIFIED_ROLE_ID, "Unverified role", cfg.unverified_role),
        channel_select(
            UNVERIFIED_CHAN_ID,
            "Unverified channel (the prompt is posted here)",
            cfg.unverified_channel,
        ),
        channel_select(
            MOD_CHAN_ID,
            "Mod-approval channel",
            cfg.mod_approval_channel,
        ),
        channel_select(
            VERIFY_LOG_CHAN_ID,
            "Verification-log channel",
            cfg.verification_log_channel,
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_PROMPT_ID)
                .label("Edit prompt message")
                .style(ButtonStyle::Primary),
            publish_button(
                POST_PROMPT_ID,
                cfg.unverified_prompt.is_some(),
                "Post prompt",
                "Update prompt",
            ),
            back_button(),
        ]),
    ]
}

/// Membership & access page: the four "who-is-what" role selects plus grey Back.
fn membership_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(MEMBER_ROLE_ID, "Member role", cfg.member_role),
        role_select(DUES_ROLE_ID, "Dues Expired role", cfg.dues_expired_role),
        role_select(
            DUES_EXPIRING_ROLE_ID,
            "Dues Expiring role",
            cfg.dues_expiring_role,
        ),
        role_select(
            OVERRIDE_ROLE_ID,
            "Manual Override role",
            cfg.manual_override_role,
        ),
        back_row(),
    ]
}

/// Dues reminders page: the thread-parent channel select; a row of four blurple per-type
/// message editors; a row of blurple "Edit channel message" + "Post channel message" + grey
/// "Set sign-up URL"; the Reminders toggle on its own row; grey Back. Five rows, at the cap.
/// The channel hosts the renewal + lapse threads, so it must be visible to both Member and
/// Dues Expired (see the help text).
fn dues_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        channel_select(
            DUES_CHAN_ID,
            "Dues-expired channel (visible to Member + Dues Expired)",
            cfg.dues_expired_channel,
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_MONTHLY_ID)
                .label("Monthly")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_YEARLY_ID)
                .label("Yearly")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_ONETIME_ID)
                .label("One-time")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_INCOME_ID)
                .label("Income-based")
                .style(ButtonStyle::Primary),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_BANNER_ID)
                .label("Edit channel message")
                .style(ButtonStyle::Primary),
            publish_button(
                POST_BANNER_ID,
                cfg.dues_banner.is_some(),
                "Post channel message",
                "Update message",
            ),
            CreateButton::new(DUES_URL_BUTTON_ID)
                .label("Set sign-up URL")
                .style(ButtonStyle::Secondary),
        ]),
        CreateActionRow::Buttons(vec![toggle_button(
            REMINDERS_TOGGLE_ID,
            "Reminders",
            cfg.reminders_enabled,
        )]),
        back_row(),
    ]
}

/// Moderation page: the moderator role select, then a row with the Automatic Membership
/// Checks toggle and grey Back.
fn moderation_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(MOD_ROLE_ID, "Moderator role", cfg.moderator_role),
        CreateActionRow::Buttons(vec![
            toggle_button(
                SCAN_TOGGLE_ID,
                "Automatic Membership Checks",
                cfg.scan_enabled,
            ),
            back_button(),
        ]),
    ]
}

/// The components for `page`, read against the current config.
fn page_components(page: Page, cfg: &GuildConfig) -> Vec<CreateActionRow> {
    match page {
        Page::Landing => landing_page(),
        Page::Verification => verification_page(cfg),
        Page::Membership => membership_page(cfg),
        Page::Dues => dues_page(cfg),
        Page::Moderation => moderation_page(cfg),
    }
}

/// The embed(s) for `page`: only the landing carries one (the readiness summary). The
/// feature pages show their values in their own selects, so they render with an empty
/// embed list - which also clears the landing's embed when navigating into a page.
fn page_embeds(page: Page, cfg: &GuildConfig, accent: u32) -> Vec<CreateEmbed> {
    match page {
        Page::Landing => vec![landing_embed(cfg, accent)],
        Page::Dues => vec![dues_page_embed(accent)],
        _ => Vec::new(),
    }
}

// ====================================================================
// Toggle handlers
// ====================================================================

/// Flip the Automatic Membership Checks (scheduled scan) toggle, persist, audit, and
/// redraw the Moderation page. Warns if the scan is enabled with no mod-approval channel
/// for the safety alert.
async fn toggle_scan(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    if !ack(ctx, interaction).await {
        return;
    }
    let data = ctx.data();
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let was_enabled = cfg.scan_enabled;
    cfg.scan_enabled = !was_enabled;
    let no_mod_channel = cfg.mod_approval_channel.is_none();
    let note = if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        let now_enabled = cfg.scan_enabled;
        data.guild_config.store(std::sync::Arc::new(cfg));
        // Attribute the toggle like every other config change (see `apply_selection`):
        // enabling a background job that can demote roles is a mod action to log.
        if let Err(e) = data
            .auditor
            .record(
                invoker,
                invoker,
                "config.set_scan",
                serde_json::json!({
                    "field": "automatic membership checks",
                    "old": was_enabled,
                    "new": now_enabled
                }),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to audit config change");
        }
        // Turning the checks on with no mod-approval channel leaves the mass-demote
        // tripwire alert nowhere to post; warn rather than enable it silently.
        if now_enabled && no_mod_channel {
            Some(
                "Automatic membership checks on. Note: no mod-approval channel is set, so the \
                 safety alert can't be posted if a check is paused - set one under Verification."
                    .to_owned(),
            )
        } else {
            None
        }
    };
    redraw(ctx, interaction, accent, Page::Moderation, note.as_deref()).await;
}

/// Flip the dues-reminders toggle, persist, audit, and redraw the Dues reminders page.
/// On a flip to off, sweep the Dues Expiring marker off every currently-marked member so
/// none is stranded with it. Warns if enabled with no dues-expired channel.
async fn toggle_reminders(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    if !ack(ctx, interaction).await {
        return;
    }
    let data = ctx.data();
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let was_enabled = cfg.reminders_enabled;
    cfg.reminders_enabled = !was_enabled;
    let note = if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        let now_enabled = cfg.reminders_enabled;
        data.guild_config.store(std::sync::Arc::new(cfg));
        if let Err(e) = data
            .auditor
            .record(
                invoker,
                invoker,
                "config.set_reminders",
                serde_json::json!({
                    "field": "dues reminders",
                    "old": was_enabled,
                    "new": now_enabled
                }),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to audit config change");
        }
        if now_enabled {
            // Turning reminders on with no dues-expired channel set means threads have
            // nowhere to post; warn rather than enable it silently.
            if data.guild_config.load().dues_expired_channel.is_none() {
                Some(
                    "Dues reminders on. Note: no dues-expired channel is set - set one on this \
                     page so reminder threads have a home."
                        .to_owned(),
                )
            } else {
                None
            }
        } else {
            // Turning reminders off: clear the Dues Expiring marker from every member that
            // still holds it, so it does not linger once the sweep stops reconciling it.
            cleanup_markers_on_off(ctx).await;
            Some(
                "Dues reminders off. Cleared the Dues Expiring marker from any held members."
                    .to_owned(),
            )
        }
    };
    redraw(ctx, interaction, accent, Page::Dues, note.as_deref()).await;
}

/// Sweep the Dues Expiring marker off every currently-marked member after reminders are
/// disabled. Builds the role writer from the saved config; if the managed roles are not
/// fully configured there is nothing to write, so it is a no-op.
async fn cleanup_markers_on_off(ctx: &Context<'_>) {
    let data = ctx.data();
    let Some(discord) = data.role_writer() else {
        tracing::debug!("reminders off: managed roles not configured; no markers to clear");
        return;
    };
    crate::reminders::cleanup_expiring_markers(
        &data.store,
        &discord,
        data.config.guild(),
        data.config.scan_pace,
    )
    .await;
}

// ====================================================================
// Posting actions
// ====================================================================

/// Which standing message a posted reference belongs to. Selects the [`GuildConfig`] slot
/// [`save_message_ref`] writes.
#[derive(Clone, Copy)]
enum PostedMessage {
    Prompt,
    Banner,
}

/// Publish the verification prompt: edit the standing message in place if one was posted
/// before, otherwise post a fresh one to the configured unverified channel and remember it.
/// Any edit failure (most often the message was deleted) falls back to a fresh post. Returns
/// a short note for the moderator.
async fn post_prompt(ctx: &Context<'_>, accent: u32) -> Option<String> {
    let data = ctx.data();
    let body = resolve_body(ctx, MessageKind::Unverified).await;
    let (existing, configured_channel) = {
        let cfg = data.guild_config.load();
        (cfg.unverified_prompt, cfg.unverified_channel)
    };

    if let Some(r) = existing {
        match ChannelId::new(r.channel.0)
            .edit_message(
                ctx.serenity_context().http.clone(),
                MessageId::new(r.message.0),
                verify_prompt_edit(&body, accent),
            )
            .await
        {
            Ok(_) => return Some("Updated the verification prompt in place.".to_owned()),
            Err(e) => tracing::warn!(
                error = %e,
                "setup: couldn't edit the existing verification prompt; posting a fresh one"
            ),
        }
    }

    let Some(channel) = configured_channel else {
        return Some(
            "Set an unverified channel first (on this page), then post the prompt.".to_owned(),
        );
    };
    match ChannelId::new(channel.0)
        .send_message(
            ctx.serenity_context().http.clone(),
            verify_prompt(&body, accent),
        )
        .await
    {
        Ok(msg) => {
            save_message_ref(
                ctx,
                PostedMessage::Prompt,
                channel,
                DiscordMessageId(msg.id.get()),
            )
            .await;
            Some("Posted the verification prompt to the unverified channel.".to_owned())
        }
        Err(e) => {
            tracing::warn!(error = %e, "setup: failed to post the verification prompt");
            Some(
                "Couldn't post the prompt there - check my permissions on that channel.".to_owned(),
            )
        }
    }
}

/// Publish the dues-expiring banner: the dues counterpart to [`post_prompt`], editing the
/// standing banner in place when one exists and otherwise posting a fresh one to the
/// configured dues-expired channel. Returns a short note for the moderator.
async fn post_banner(ctx: &Context<'_>, accent: u32) -> Option<String> {
    let data = ctx.data();
    let body = resolve_body(ctx, MessageKind::DuesBanner).await;
    let (existing, configured_channel, signup_url) = {
        let cfg = data.guild_config.load();
        (
            cfg.dues_banner,
            cfg.dues_expired_channel,
            cfg.dues_signup_url.clone(),
        )
    };

    if let Some(r) = existing {
        match ChannelId::new(r.channel.0)
            .edit_message(
                ctx.serenity_context().http.clone(),
                MessageId::new(r.message.0),
                banner_edit(&body, signup_url.as_deref(), accent),
            )
            .await
        {
            Ok(_) => return Some("Updated the dues-expiring message in place.".to_owned()),
            Err(e) => tracing::warn!(
                error = %e,
                "setup: couldn't edit the existing dues channel message; posting a fresh one"
            ),
        }
    }

    let Some(channel) = configured_channel else {
        return Some(
            "Set a dues-expired channel first (on this page), then post the channel message."
                .to_owned(),
        );
    };
    match ChannelId::new(channel.0)
        .send_message(
            ctx.serenity_context().http.clone(),
            banner_message(&body, signup_url.as_deref(), accent),
        )
        .await
    {
        Ok(msg) => {
            save_message_ref(
                ctx,
                PostedMessage::Banner,
                channel,
                DiscordMessageId(msg.id.get()),
            )
            .await;
            Some("Posted the dues-expiring message to the dues-expired channel.".to_owned())
        }
        Err(e) => {
            tracing::warn!(error = %e, "setup: failed to post the dues channel message");
            Some(
                "Couldn't post the message there - check my permissions on that channel."
                    .to_owned(),
            )
        }
    }
}

/// Remember where a standing message was just posted, so the next publish edits it in place
/// and its button reads "Update". Persists the reference into the guild config and swaps the
/// live handle. A failed save is logged, not surfaced: the message is already posted, and a
/// missing reference only means the next publish posts a fresh copy.
async fn save_message_ref(
    ctx: &Context<'_>,
    which: PostedMessage,
    channel: DiscordChannelId,
    message: DiscordMessageId,
) {
    let data = ctx.data();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let slot = match which {
        PostedMessage::Prompt => &mut cfg.unverified_prompt,
        PostedMessage::Banner => &mut cfg.dues_banner,
    };
    *slot = Some(MessageRef { channel, message });
    if let Err(e) = data.store.save_config(data.config.guild(), &cfg).await {
        tracing::warn!(
            error = %e,
            "setup: posted the message but couldn't save its reference; the next publish posts fresh"
        );
        return;
    }
    data.guild_config.store(std::sync::Arc::new(cfg));
}

// ====================================================================
// Message-edit modal
// ====================================================================

/// Which page a message editor lives on, so a redraw after a save returns there.
fn editor_page(kind: MessageKind) -> Page {
    match kind {
        MessageKind::Unverified => Page::Verification,
        _ => Page::Dues,
    }
}

/// Open a prefilled body modal for `kind`, await the submission, store the new body, and
/// reply with a rendered preview before redrawing the editor's page. A dismissed modal
/// sends no event; the timeout prevents a hang.
async fn edit_message(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    msg: &serenity::all::Message,
    accent: u32,
    kind: MessageKind,
) {
    let current = resolve_body(ctx, kind).await;
    let modal = message_modal(kind, &current);
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Modal(modal),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to open the message-edit modal");
        return;
    }

    let submit = match msg
        .await_modal_interaction(ctx.serenity_context())
        .author_id(ctx.author().id)
        .custom_ids(vec![MSG_MODAL_ID.to_owned()])
        .timeout(MSG_MODAL_TIMEOUT)
        .await
    {
        Some(s) => s,
        None => return,
    };
    if let Err(e) = submit
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Acknowledge,
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to acknowledge the message-edit modal submit");
        return;
    }

    let body = modal_field(&submit, MSG_BODY_FIELD_ID);
    let trimmed = body.trim();
    let data = ctx.data();
    let guild = data.config.guild();

    let note = if trimmed.is_empty() {
        Some("Left the message unchanged - the body can't be empty.".to_owned())
    } else if let Err(e) = data
        .store
        .set_template(guild, kind, trimmed.to_owned())
        .await
    {
        tracing::error!(error = %e, "setup: failed to save message template");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        Some("Saved. Here's how it looks:".to_owned())
    };

    // Redraw the editor's page on the original ephemeral message, then post the preview as a
    // follow-up so the moderator sees the rendered result alongside the panel.
    redraw_via(&submit, ctx, accent, editor_page(kind), note.as_deref()).await;
    if trimmed.is_empty() {
        return;
    }
    let preview = CreateInteractionResponseFollowup::new()
        .ephemeral(true)
        .embed(preview_embed(kind, trimmed, accent));
    if let Err(e) = submit
        .create_followup(ctx.serenity_context(), preview)
        .await
    {
        tracing::warn!(error = %e, "setup: failed to post the message preview");
    }
}

/// The modal used to edit one message body. Title and field label name the message kind;
/// the field is prefilled with the current body so the moderator edits rather than retypes.
fn message_modal(kind: MessageKind, current: &str) -> CreateModal {
    let input = CreateInputText::new(InputTextStyle::Paragraph, "Message body", MSG_BODY_FIELD_ID)
        .value(current)
        .required(true)
        .min_length(1)
        .max_length(2000);
    CreateModal::new(MSG_MODAL_ID, format!("Edit {} message", kind_label(kind)))
        .components(vec![CreateActionRow::InputText(input)])
}

/// The preview embed for a just-saved body, titled the same way the live message will be.
/// A dues type previews as the renewal notice (with a placeholder expiry 30 days out so the
/// title reads concretely); the banner as "Dues Expiring!"; the prompt as "Verify your
/// membership". Only the editable body changes, so the preview shows the titled card alone -
/// the live buttons (Renew / Get help / opt-out) are not interactive in a preview.
fn preview_embed(kind: MessageKind, body: &str, accent: u32) -> serenity::all::CreateEmbed {
    let title = match kind {
        MessageKind::Unverified => "Verify your membership".to_owned(),
        MessageKind::DuesBanner => "Dues Expiring!".to_owned(),
        _ => {
            let placeholder_xdate = chrono::Local::now().date_naive() + chrono::TimeDelta::days(30);
            crate::render::reminders::notice_title(Milestone::Renewal, placeholder_xdate)
        }
    };
    serenity::all::CreateEmbed::new()
        .title(title)
        .description(body)
        .color(accent)
}

/// The human label for a message kind, used in the editor modal's title.
fn kind_label(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Monthly => "monthly reminder",
        MessageKind::Yearly => "yearly reminder",
        MessageKind::OneTime => "one-time reminder",
        MessageKind::IncomeBased => "income-based reminder",
        MessageKind::Unverified => "verification prompt",
        MessageKind::DuesBanner => "dues channel",
    }
}

/// The stored body for `kind`, or the built-in default when no override is set.
async fn resolve_body(ctx: &Context<'_>, kind: MessageKind) -> String {
    let data = ctx.data();
    let guild = data.config.guild();
    match data.store.template(guild, kind).await {
        Ok(Some(body)) => body,
        Ok(None) => default_body(kind).to_owned(),
        Err(e) => {
            tracing::warn!(error = %e, "setup: failed to read message template; using default");
            default_body(kind).to_owned()
        }
    }
}

// ====================================================================
// Dues sign-up URL modal
// ====================================================================

/// Open the dues sign-up URL modal, await submission, validate the scheme, persist, audit,
/// and redraw the Dues reminders page.
async fn set_dues_url(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    msg: &serenity::all::Message,
    accent: u32,
    invoker: DiscordUserId,
) {
    let data = ctx.data();
    let modal = dues_url_modal(data.guild_config.load().dues_signup_url.as_deref());
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Modal(modal),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to open dues URL modal");
        return;
    }
    let submit = match msg
        .await_modal_interaction(ctx.serenity_context())
        .author_id(ctx.author().id)
        .custom_ids(vec![DUES_URL_MODAL_ID.to_owned()])
        .timeout(DUES_URL_MODAL_TIMEOUT)
        .await
    {
        Some(s) => s,
        None => return,
    };
    if let Err(e) = submit
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Acknowledge,
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to acknowledge dues URL modal submit");
        return;
    }
    let raw_url = modal_field(&submit, DUES_URL_FIELD_ID);
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let old_url = cfg.dues_signup_url.clone();
    let trimmed = raw_url.trim();
    let note = if !trimmed.is_empty() && !crate::render::reminders::is_http_signup_url(trimmed) {
        // A link button with a non-http(s) URL is rejected by Discord, which would fail every
        // reminder send; refuse the value and keep the current one.
        Some(
            "That doesn't look like a web link - the dues sign-up URL must start with http:// or \
             https://. Left it unchanged."
                .to_owned(),
        )
    } else {
        let new_url = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        };
        cfg.dues_signup_url = new_url.clone();
        if let Err(e) = data.store.save_config(guild, &cfg).await {
            tracing::error!(error = %e, "setup: failed to save guild config");
            Some("Something went wrong saving that - please try again.".to_owned())
        } else {
            data.guild_config.store(std::sync::Arc::new(cfg));
            if let Err(e) = data
                .auditor
                .record(
                    invoker,
                    invoker,
                    "config.set_dues_url",
                    serde_json::json!({
                        "field": "dues sign-up URL",
                        "old": old_url,
                        "new": new_url
                    }),
                )
                .await
            {
                tracing::warn!(error = %e, "setup: failed to audit config change");
            }
            if old_url == new_url {
                Some("That's already the dues sign-up URL - nothing changed.".to_owned())
            } else {
                Some("Updated the dues sign-up URL.".to_owned())
            }
        }
    };
    redraw_via(&submit, ctx, accent, Page::Dues, note.as_deref()).await;
}

/// The modal used by "Set sign-up URL". Pre-fills the current value so the moderator can see
/// and edit it rather than re-typing from scratch.
fn dues_url_modal(current: Option<&str>) -> CreateModal {
    let input = CreateInputText::new(InputTextStyle::Short, "Dues sign-up URL", DUES_URL_FIELD_ID)
        .placeholder("https://example.org/dues")
        .required(false)
        .max_length(2000);
    let input = match current {
        Some(url) => input.value(url),
        None => input,
    };
    CreateModal::new(DUES_URL_MODAL_ID, "Set dues sign-up URL")
        .components(vec![CreateActionRow::InputText(input)])
}

// ====================================================================
// Shared components
// ====================================================================

fn back_button() -> CreateButton {
    CreateButton::new(BACK_ID)
        .label("Back")
        .style(ButtonStyle::Secondary)
}

fn back_row() -> CreateActionRow {
    CreateActionRow::Buttons(vec![back_button()])
}

/// A single-button toggle: grey (Secondary) when off, green (Success) when on, the label
/// suffixed with its current state. Pressing it flips the underlying setting.
fn toggle_button(id: &str, label: &str, enabled: bool) -> CreateButton {
    let (style, state) = if enabled {
        (ButtonStyle::Success, "ON")
    } else {
        (ButtonStyle::Secondary, "OFF")
    };
    CreateButton::new(id)
        .label(format!("{label}: {state}"))
        .style(style)
}

/// The blurple publish button for a standing message. Reads `post_label` until the message
/// has been posted once (a stored [`MessageRef`] in the config), then `update_label` - a
/// press re-publishes by editing that message in place rather than posting a duplicate.
fn publish_button(id: &str, posted: bool, post_label: &str, update_label: &str) -> CreateButton {
    CreateButton::new(id)
        .label(if posted { update_label } else { post_label })
        .style(ButtonStyle::Primary)
}

fn role_select(id: &str, placeholder: &str, current: Option<DiscordRoleId>) -> CreateActionRow {
    let kind = CreateSelectMenuKind::Role {
        default_roles: current.map(|r| vec![RoleId::new(r.0)]),
    };
    CreateActionRow::SelectMenu(CreateSelectMenu::new(id, kind).placeholder(placeholder))
}

fn channel_select(
    id: &str,
    placeholder: &str,
    current: Option<DiscordChannelId>,
) -> CreateActionRow {
    let kind = CreateSelectMenuKind::Channel {
        channel_types: Some(vec![ChannelType::Text]),
        default_channels: current.map(|c| vec![ChannelId::new(c.0)]),
    };
    CreateActionRow::SelectMenu(CreateSelectMenu::new(id, kind).placeholder(placeholder))
}

/// Read one input-text field's value out of a submitted modal interaction, or "" if absent.
fn modal_field(submit: &serenity::all::ModalInteraction, field_id: &str) -> String {
    submit
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            ActionRowComponent::InputText(input) if input.custom_id == field_id => {
                input.value.clone()
            }
            _ => None,
        })
        .unwrap_or_default()
}

// ====================================================================
// Panel rendering
// ====================================================================

/// Navigate to `page`: respond by replacing the message with the page's components and the
/// current-config embed (no note). Used for the nav and Back buttons, which need no
/// deferred ack since they do no slow work.
async fn nav(ctx: &Context<'_>, interaction: &ComponentInteraction, accent: u32, page: Page) {
    let cfg = ctx.data().guild_config.load();
    let message = CreateInteractionResponseMessage::new()
        .embeds(page_embeds(page, &cfg, accent))
        .components(page_components(page, &cfg));
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::UpdateMessage(message),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to navigate the panel; continuing");
    }
}

/// Re-render `page` after a deferred [`ack`], editing the original message in place. The
/// apply path's counterpart to [`nav`], which it cannot use because the interaction is
/// already acknowledged. Logs and continues on a failed edit.
async fn redraw(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    page: Page,
    note: Option<&str>,
) {
    let cfg = ctx.data().guild_config.load();
    let mut edit = EditInteractionResponse::new()
        .embeds(page_embeds(page, &cfg, accent))
        .components(page_components(page, &cfg));
    if let Some(note) = note {
        edit = edit.content(note);
    }
    if let Err(e) = interaction
        .edit_response(ctx.serenity_context(), edit)
        .await
    {
        tracing::warn!(error = %e, "setup: failed to update the panel; continuing");
    }
}

/// Re-render `page` through a modal-submit interaction's token (the modal was acknowledged,
/// so it owns the edit of the original ephemeral message). Mirrors [`redraw`] for the
/// modal-driven flows.
async fn redraw_via(
    submit: &serenity::all::ModalInteraction,
    ctx: &Context<'_>,
    accent: u32,
    page: Page,
    note: Option<&str>,
) {
    let cfg = ctx.data().guild_config.load();
    let mut edit = EditInteractionResponse::new()
        .embeds(page_embeds(page, &cfg, accent))
        .components(page_components(page, &cfg));
    if let Some(note) = note {
        edit = edit.content(note);
    }
    if let Err(e) = submit.edit_response(ctx.serenity_context(), edit).await {
        tracing::warn!(error = %e, "setup: failed to update the panel after a modal; continuing");
    }
}

/// Acknowledge a component interaction with a deferred message update, so the database
/// write and audit that follow cannot blow Discord's 3-second response deadline (the panel
/// is then edited in place by [`redraw`]). Returns whether the acknowledgement landed; on
/// failure the caller skips this interaction rather than acting without feedback.
async fn ack(ctx: &Context<'_>, interaction: &ComponentInteraction) -> bool {
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Acknowledge,
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to acknowledge the interaction; skipping");
        return false;
    }
    true
}

// ====================================================================
// Selection apply
// ====================================================================

/// Apply one role/channel selection: set the field, validate, persist, swap the live
/// handle, audit. Returns a short note for the moderator (a confirmation, or a rejection
/// reason), or `None` if the interaction was not a setting select we handle.
async fn apply_selection(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    invoker: DiscordUserId,
) -> Option<String> {
    let data = ctx.data();
    let id = interaction.data.custom_id.as_str();
    let mut cfg: GuildConfig = GuildConfig::clone(&data.guild_config.load());

    let (label, action, old, new): (&str, &str, Option<u64>, u64) = match &interaction.data.kind {
        ComponentInteractionDataKind::RoleSelect { values } => {
            let rid = values.first()?.get();
            let (label, old) = set_role_field(&mut cfg, id, DiscordRoleId(rid))?;
            (label, "config.set_role", old, rid)
        }
        ComponentInteractionDataKind::ChannelSelect { values } => {
            let cid = values.first()?.get();
            let (label, old) = set_channel_field(&mut cfg, id, DiscordChannelId(cid))?;
            (label, "config.set_channel", old, cid)
        }
        _ => return None,
    };

    // Every configured role must stay distinct (see `roles_are_distinct`).
    if !roles_are_distinct(&cfg) {
        return Some(
            "That role is already assigned to another setting - pick a distinct role for each."
                .to_owned(),
        );
    }

    let guild = data.config.guild();
    if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        return Some("Something went wrong saving that - please try again.".to_owned());
    }
    data.guild_config.store(std::sync::Arc::new(cfg));
    if let Err(e) = data
        .auditor
        .record(
            invoker,
            invoker,
            action,
            serde_json::json!({ "field": label, "old": old, "new": new }),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to audit config change");
    }
    Some(format!("Updated the {label}."))
}

/// Set the role field named by `id`, returning its display label and the previous id.
fn set_role_field(
    cfg: &mut GuildConfig,
    id: &str,
    rid: DiscordRoleId,
) -> Option<(&'static str, Option<u64>)> {
    let (label, slot): (&str, &mut Option<DiscordRoleId>) = match id {
        MOD_ROLE_ID => ("moderator role", &mut cfg.moderator_role),
        MEMBER_ROLE_ID => ("member role", &mut cfg.member_role),
        DUES_ROLE_ID => ("dues-expired role", &mut cfg.dues_expired_role),
        DUES_EXPIRING_ROLE_ID => ("dues-expiring role", &mut cfg.dues_expiring_role),
        UNVERIFIED_ROLE_ID => ("unverified role", &mut cfg.unverified_role),
        OVERRIDE_ROLE_ID => ("manual override role", &mut cfg.manual_override_role),
        _ => return None,
    };
    let old = slot.map(|r| r.0);
    *slot = Some(rid);
    Some((label, old))
}

/// Set the channel field named by `id`, returning its display label and the previous id.
fn set_channel_field(
    cfg: &mut GuildConfig,
    id: &str,
    cid: DiscordChannelId,
) -> Option<(&'static str, Option<u64>)> {
    let (label, slot): (&str, &mut Option<DiscordChannelId>) = match id {
        MOD_CHAN_ID => ("mod-approval channel", &mut cfg.mod_approval_channel),
        UNVERIFIED_CHAN_ID => ("unverified channel", &mut cfg.unverified_channel),
        DUES_CHAN_ID => ("dues-expired channel", &mut cfg.dues_expired_channel),
        VERIFY_LOG_CHAN_ID => (
            "verification-log channel",
            &mut cfg.verification_log_channel,
        ),
        _ => return None,
    };
    let old = slot.map(|c| c.0);
    *slot = Some(cid);
    Some((label, old))
}

/// Every configured role must be distinct - the moderator role included. Assigning one
/// Discord role to two settings is always a configuration mistake, and letting a managed
/// status role double as the moderator role would hand moderator access to anyone the bot
/// verifies as a member.
fn roles_are_distinct(cfg: &GuildConfig) -> bool {
    let mut seen = std::collections::HashSet::new();
    [
        cfg.moderator_role,
        cfg.member_role,
        cfg.dues_expired_role,
        cfg.dues_expiring_role,
        cfg.unverified_role,
        cfg.manual_override_role,
    ]
    .into_iter()
    .flatten()
    .all(|r| seen.insert(r.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_roles() {
        let mut cfg = GuildConfig {
            member_role: Some(DiscordRoleId(5)),
            dues_expired_role: Some(DiscordRoleId(5)),
            ..Default::default()
        };
        assert!(!roles_are_distinct(&cfg));
        cfg.dues_expired_role = Some(DiscordRoleId(6));
        cfg.unverified_role = Some(DiscordRoleId(7));
        assert!(roles_are_distinct(&cfg));
        // A managed status role must not double as the moderator role.
        cfg.moderator_role = Some(DiscordRoleId(5));
        assert!(!roles_are_distinct(&cfg));
    }

    #[test]
    fn dues_expiring_role_must_be_distinct() {
        let cfg = GuildConfig {
            member_role: Some(DiscordRoleId(5)),
            dues_expired_role: Some(DiscordRoleId(6)),
            unverified_role: Some(DiscordRoleId(7)),
            dues_expiring_role: Some(DiscordRoleId(5)), // clashes with Member
            ..Default::default()
        };
        assert!(!roles_are_distinct(&cfg));
    }

    #[test]
    fn set_role_field_reports_old_and_updates() {
        let mut cfg = GuildConfig::default();
        let (label, old) = set_role_field(&mut cfg, MEMBER_ROLE_ID, DiscordRoleId(9)).unwrap();
        assert_eq!(label, "member role");
        assert_eq!(old, None);
        assert_eq!(cfg.member_role, Some(DiscordRoleId(9)));
        let (_, old2) = set_role_field(&mut cfg, MEMBER_ROLE_ID, DiscordRoleId(10)).unwrap();
        assert_eq!(old2, Some(9));
    }

    #[test]
    fn set_role_field_handles_the_dues_expiring_role() {
        let mut cfg = GuildConfig::default();
        let (label, old) =
            set_role_field(&mut cfg, DUES_EXPIRING_ROLE_ID, DiscordRoleId(12)).unwrap();
        assert_eq!(label, "dues-expiring role");
        assert_eq!(old, None);
        assert_eq!(cfg.dues_expiring_role, Some(DiscordRoleId(12)));
    }

    #[test]
    fn set_channel_field_dues_expired() {
        let mut cfg = GuildConfig::default();
        let (label, old) = set_channel_field(&mut cfg, DUES_CHAN_ID, DiscordChannelId(20)).unwrap();
        assert_eq!(label, "dues-expired channel");
        assert_eq!(old, None);
        assert_eq!(cfg.dues_expired_channel, Some(DiscordChannelId(20)));
    }
}
