//! Self-service verification: the standing-button + modal flow that lets a member
//! verify themselves against Solidarity Tech with no moderator approval. This file
//! holds the pure name double-check and the gateway interaction handler.

use std::time::Instant;

use serenity::all::{
    ComponentInteraction, Context, CreateInteractionResponse, CreateInteractionResponseMessage,
    EditInteractionResponse, Interaction, ModalInteraction,
};

use engine::backends::util::{DiscordHandle, DiscordUserId, Email};
use engine::verify::{DataStore, EmailGrant, Member as VerifyMember, Target};

use crate::data::{Data, Error};
use crate::render::modal::parse_email;
use crate::render::self_verify::{
    EMAIL_FIELD_ID, FIRST_FIELD_ID, LAST_FIELD_ID, PROMPT_BUTTON_ID, SUBMIT_MODAL_ID, log_embed,
    self_verify_modal,
};

/// Whether the name a member typed lines up with their Solidarity Tech record.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// First name and last initial both line up.
    Match,
    /// At least one does not - the log post is flagged, but the grant still stands.
    Mismatch,
    /// The record has no name to compare against; not a mismatch.
    Unchecked,
}

/// Compare a submitted first name + last initial against the record's full name.
///
/// Deliberately blunt - no fuzzy or nickname matching. The first whitespace token
/// of the record name must equal the submitted first name (case-insensitive), and
/// the first character of the record name's last token must equal the submitted
/// last initial (case-insensitive). A record with no usable name is
/// [`Unchecked`](NameCheck::Unchecked), never a mismatch.
pub fn name_check(record_name: Option<&str>, first: &str, last_initial: &str) -> NameCheck {
    let Some(name) = record_name else {
        return NameCheck::Unchecked;
    };
    let mut tokens = name.split_whitespace();
    let Some(rec_first) = tokens.next() else {
        return NameCheck::Unchecked;
    };
    let rec_last = name.split_whitespace().last().unwrap_or(rec_first);

    let first_ok = rec_first.eq_ignore_ascii_case(first.trim());
    let last_ok = match (last_initial.trim().chars().next(), rec_last.chars().next()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    };
    if first_ok && last_ok {
        NameCheck::Match
    } else {
        NameCheck::Mismatch
    }
}

/// Shown for every outcome that is not a clean grant - a miss, a conflict, a
/// malformed email, an unusable record. Deliberately gives no detail (no membership
/// enumeration); a real member can simply retry.
const UNIFORM_FAILURE: &str = "We couldn't find you. You can try to verify again at any time.";
/// The only distinct failure: verification could not run (backend down / unconfigured).
const BACKEND_ERROR: &str = "Something went wrong on my end - please try again in a moment.";
const RATE_LIMITED: &str = "You're going a little fast - please wait a moment and try again.";
const SUCCESS: &str = "You're verified! Welcome.";

/// Route a raw gateway interaction. Acts only on the self-verify button and modal;
/// every other interaction (poise commands, other collectors) is ignored so the
/// framework's own dispatch is untouched.
pub async fn on_interaction(
    ctx: &Context,
    interaction: &Interaction,
    data: &Data,
) -> Result<(), Error> {
    match interaction {
        Interaction::Component(c) if c.data.custom_id == PROMPT_BUTTON_ID => {
            open_modal(ctx, c).await
        }
        Interaction::Modal(m) if m.data.custom_id == SUBMIT_MODAL_ID => {
            on_submit(ctx, m, data).await
        }
        _ => Ok(()),
    }
}

/// Open the verification form in response to the standing button.
async fn open_modal(ctx: &Context, c: &ComponentInteraction) -> Result<(), Error> {
    c.create_response(ctx, CreateInteractionResponse::Modal(self_verify_modal()))
        .await?;
    Ok(())
}

/// Read the value the member typed into the named modal field.
fn field_value(submit: &ModalInteraction, field_id: &str) -> String {
    submit
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            serenity::all::ActionRowComponent::InputText(input) if input.custom_id == field_id => {
                input.value.clone()
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Run the self-service verification for a submitted form.
async fn on_submit(ctx: &Context, submit: &ModalInteraction, data: &Data) -> Result<(), Error> {
    // Defer ephemerally first: the live Solidarity Tech read + grant below can
    // exceed Discord's 3-second interaction deadline. The result edits this reply.
    submit
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;

    let reply = |text: &str| {
        let text = text.to_owned();
        async move {
            let _ = submit
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await;
        }
    };

    let member_id = DiscordUserId(submit.user.id.get());
    if !data.self_verify_limiter.check(member_id, Instant::now()) {
        reply(RATE_LIMITED).await;
        return Ok(());
    }

    // Roles must be configured (this is also the role-write client we need).
    let Some(discord) = data.role_writer() else {
        tracing::warn!("self-verify attempted before the managed roles were configured");
        reply(BACKEND_ERROR).await;
        return Ok(());
    };

    let Some(email) = parse_email(&field_value(submit, EMAIL_FIELD_ID)) else {
        // A malformed email gives the same uniform response - no detail.
        reply(UNIFORM_FAILURE).await;
        return Ok(());
    };
    let first = field_value(submit, FIRST_FIELD_ID);
    let last_initial = field_value(submit, LAST_FIELD_ID);

    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
    );
    let target = Target {
        id: member_id,
        handle: DiscordHandle(submit.user.name.clone()),
    };
    // Self-service: the member is both actor and subject - that is what marks the
    // grant as self-service in the audit log (no extra field needed).
    let outcome = VerifyMember::new(&store, target)
        .verify_by_email_with_record(member_id, email.clone())
        .await;

    match outcome {
        Ok(EmailGrant::Verified { role, record }) => {
            // `name_check` and `NameCheck` are defined above in this same module.
            let warn = matches!(
                name_check(record.full_name.as_deref(), &first, &last_initial),
                NameCheck::Mismatch
            );
            post_log(
                ctx,
                data,
                submit,
                record.full_name.as_deref(),
                role,
                &email,
                warn,
            )
            .await;
            reply(SUCCESS).await;
        }
        Ok(EmailGrant::Malformed) | Ok(EmailGrant::Conflict) | Ok(EmailGrant::NotFound) => {
            // Not a clean grant (no match, a conflict, or a record with no usable
            // standing). No detail to the member; the engine already audited the attempt.
            reply(UNIFORM_FAILURE).await;
        }
        Err(e) => {
            tracing::error!(error = %e, "self-verify failed");
            reply(BACKEND_ERROR).await;
        }
    }
    Ok(())
}

/// Post the success record to the configured log channel. Missing channel -> warn
/// and skip; the grant already stands.
async fn post_log(
    ctx: &Context,
    data: &Data,
    submit: &ModalInteraction,
    record_name: Option<&str>,
    role: domain::Role,
    email: &Email,
    name_warning: bool,
) {
    let Some(channel) = data.guild_config.load().verification_log_channel else {
        tracing::warn!(
            "self-verify succeeded but no verification_log_channel is set; skipping log"
        );
        return;
    };
    let embed = log_embed(
        submit.user.id,
        &submit.user.name,
        record_name,
        role,
        &email.0,
        name_warning,
        data.config.accent_color,
    );
    if let Err(e) = serenity::all::ChannelId::new(channel.0)
        .send_message(&ctx.http, serenity::all::CreateMessage::new().embed(embed))
        .await
    {
        tracing::warn!(error = %e, "self-verify: failed to post the verification log");
    }
}

#[cfg(test)]
mod name_check_tests {
    use super::*;

    #[test]
    fn matches_first_and_last_initial_case_insensitively() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "rosy", "R"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("rosy rascal"), " ROSY ", "r"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "R."),
            NameCheck::Match
        );
    }

    #[test]
    fn first_name_or_initial_off_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Shadow", "R"),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "Z"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn an_absent_record_name_is_unchecked() {
        assert_eq!(name_check(None, "Rosy", "R"), NameCheck::Unchecked);
        assert_eq!(name_check(Some("   "), "Rosy", "R"), NameCheck::Unchecked);
    }

    #[test]
    fn a_single_token_record_name_compares_first_against_last() {
        // "Cher" -> first and last token are the same; first "Cher", initial "C".
        assert_eq!(name_check(Some("Cher"), "Cher", "C"), NameCheck::Match);
    }

    #[test]
    fn multi_token_name_uses_the_last_token_for_the_initial() {
        // "Amy Rose Hedgehog" -> first "Amy", last token "Hedgehog"; middle token "Rose" is ignored.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "H"),
            NameCheck::Match
        );
        // "R" matches the middle token "Rose", not the last - must be a mismatch.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "R"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn empty_or_blank_last_initial_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", ""),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "   "),
            NameCheck::Mismatch
        );
    }
}
