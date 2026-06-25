//! The modal-driven flows: editing one message body, and setting the dues sign-up URL.
//!
//! Each opens a prefilled modal in response to the button, awaits the submission on the
//! original ephemeral message, persists, and redraws through the modal token via the hub's
//! [`redraw_via`]. Shared vocabulary (the modal-id/timeout constants, [`Page`],
//! [`redraw_via`], [`resolve_body`]) comes from the hub via `use super::*`.

use serenity::all::{
    ActionRowComponent, ComponentInteraction, CreateActionRow, CreateEmbed, CreateInputText,
    CreateInteractionResponse, CreateInteractionResponseFollowup, CreateModal, InputTextStyle,
    Message, ModalInteraction,
};

use engine::audit::AuditLog;
use engine::backends::util::DiscordUserId;
use engine::reminders::{MessageKind, Milestone};
use engine::store::{ConfigStore, GuildConfig, MessageTemplates};

use crate::render::reminders::{is_http_signup_url, notice_title};

use super::*;

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
pub(super) async fn edit_message(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    msg: &Message,
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
fn preview_embed(kind: MessageKind, body: &str, accent: u32) -> CreateEmbed {
    let title = match kind {
        MessageKind::Unverified => "Verify your membership".to_owned(),
        MessageKind::DuesBanner => "Dues Expiring!".to_owned(),
        _ => {
            let placeholder_xdate = chrono::Local::now().date_naive() + chrono::TimeDelta::days(30);
            notice_title(Milestone::Renewal, placeholder_xdate)
        }
    };
    CreateEmbed::new()
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

// ====================================================================
// Dues sign-up URL modal
// ====================================================================

/// Open the dues sign-up URL modal, await submission, validate the scheme, persist, audit,
/// and redraw the Dues reminders page.
pub(super) async fn set_dues_url(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    msg: &Message,
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
    let note = if !trimmed.is_empty() && !is_http_signup_url(trimmed) {
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

/// Read one input-text field's value out of a submitted modal interaction, or "" if absent.
fn modal_field(submit: &ModalInteraction, field_id: &str) -> String {
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
