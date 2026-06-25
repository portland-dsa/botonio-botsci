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
//!
//! This hub owns the command entry, the interaction loop, the custom-id constants that
//! bind the rendered buttons to the dispatch, the [`Page`] selector, and the panel-render
//! helpers ([`nav`]/[`redraw`]/[`redraw_via`]/[`ack`]). The leaf modules hold the work each
//! interaction triggers: [`pages`] builds the per-page components, [`handlers`] applies a
//! setting or runs a toggle/posting action, and [`modals`] drives the modal flows. The
//! leaves read this hub's shared vocabulary through `use super::*`.

mod handlers;
mod modals;
mod pages;

use std::time::Duration;

use serenity::all::{
    ComponentInteraction, ComponentInteractionCollector, CreateInteractionResponse,
    CreateInteractionResponseMessage, EditInteractionResponse, ModalInteraction, Permissions,
};
use serenity::futures::StreamExt as _;

use engine::backends::util::DiscordUserId;
use engine::reminders::MessageKind;
use engine::store::MessageTemplates;

use crate::data::{Context, Error};
use crate::render::reminders::default_body;
use crate::render::setup::landing_embed;

use handlers::{apply_selection, post_banner, post_prompt, toggle_reminders, toggle_scan};
use modals::{edit_message, set_dues_url};
use pages::{landing_page, page_components, page_embeds};

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

/// The stored body for `kind`, or the built-in default when no override is set. Shared by the
/// posting flow ([`handlers`]) and the message editor ([`modals`]).
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
    submit: &ModalInteraction,
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
