//! `/verify @member` - a moderator matches a member in Solidarity Tech and assigns
//! their role. Moderators only; every invocation is audited. On an automatic miss, the
//! moderator can supply the member's email by hand.

use std::time::Duration;

use serenity::all::{
    ActionRowComponent, ButtonStyle, CreateActionRow, CreateButton, CreateEmbed,
    CreateInteractionResponse, EditInteractionResponse, User,
};

use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::verify::{self, VerifyOutcome};

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;
use crate::render::modal::{EMAIL_FIELD_ID, EMAIL_MODAL_ID, email_modal, parse_email};
use crate::render::verify::{VerifyState, state_embed};

/// The "Look up by email" button id and the inactivity window the collectors wait.
const LOOKUP_BUTTON_ID: &str = "verify_lookup_email";
const IDLE: Duration = Duration::from_secs(180);

/// Verify a member and assign their standing role. Moderators only.
///
/// Hidden from non-moderators via `default_member_permissions`; the in-code
/// moderator check is the real gate.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn verify(
    ctx: Context<'_>,
    #[description = "The member to verify"] target: User,
) -> Result<(), Error> {
    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted a member verify");
        ctx.send(
            poise::CreateReply::default()
                .content("That command is for moderators only.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let invoker = DiscordUserId(ctx.author().id.get());
    let target_id = DiscordUserId(target.id.get());
    let target_handle = DiscordHandle(target.name.clone());
    let data = ctx.data();
    let result = verify::verify(
        &*data.solidarity_tech,
        &*data.discord,
        &*data.store,
        &*data.auditor,
        invoker,
        target_id,
        target_handle.clone(),
    )
    .await;

    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };

    match result {
        Ok(VerifyOutcome::Verified(role)) => {
            ctx.send(plain(&format!(
                "Verified {} as {}.",
                target.name,
                role.as_str()
            )))
            .await?;
        }
        Ok(VerifyOutcome::Conflict) => {
            tracing::warn!("verify hit a handle/account conflict");
            ctx.send(plain(
                "That handle is on record for a different account. \
                 Nothing was changed - please check the records by hand.",
            ))
            .await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "member verify failed");
            ctx.send(plain(
                "Something went wrong on my end - please try again in a moment.",
            ))
            .await?;
        }
        // The automatic match missed: offer the manual email path.
        Ok(VerifyOutcome::Unverified) | Ok(VerifyOutcome::NotFound) => {
            manual_verify_flow(ctx, &target, invoker, target_id, target_handle).await?;
        }
    }
    Ok(())
}

/// Display name + avatar for the target, for the host and outcome embeds.
fn header(target: &User) -> (String, String, String) {
    let display = target
        .global_name
        .clone()
        .unwrap_or_else(|| target.name.clone());
    (display, target.name.clone(), target.face())
}

/// One ephemeral message hosting the email-lookup button and every outcome edit.
async fn manual_verify_flow(
    ctx: Context<'_>,
    target: &User,
    invoker: DiscordUserId,
    target_id: DiscordUserId,
    target_handle: DiscordHandle,
) -> Result<(), Error> {
    let (display, handle, avatar) = header(target);
    let button_row = CreateActionRow::Buttons(vec![
        CreateButton::new(LOOKUP_BUTTON_ID)
            .label("Look up by email")
            .style(ButtonStyle::Primary),
    ]);

    let reply = |embed: CreateEmbed, with_button: bool| {
        let mut r = poise::CreateReply::default().embed(embed).ephemeral(true);
        r = r.components(if with_button {
            vec![button_row.clone()]
        } else {
            vec![]
        });
        r
    };

    let handle_msg = ctx
        .send(reply(
            state_embed(&display, &handle, &avatar, &VerifyState::Prompt),
            true,
        ))
        .await?;
    let message = handle_msg.message().await?;
    let modal_id = format!("{EMAIL_MODAL_ID}:{}", message.id.get());
    let sctx = ctx.serenity_context();

    loop {
        // Wait for the moderator to press the button.
        let Some(button) = message
            .await_component_interaction(sctx)
            .author_id(ctx.author().id)
            .timeout(IDLE)
            .await
        else {
            handle_msg
                .edit(
                    ctx,
                    reply(
                        state_embed(&display, &handle, &avatar, &VerifyState::Expired),
                        false,
                    ),
                )
                .await?;
            return Ok(());
        };

        // Open the modal in response to the button.
        button
            .create_response(
                sctx,
                CreateInteractionResponse::Modal(email_modal(&modal_id, &display)),
            )
            .await?;

        // Await the submission. Dismissing a modal sends no event, so also watch for a
        // re-click of the still-visible button and reopen the modal - otherwise the button
        // would be dead until this wait timed out.
        let submit = loop {
            tokio::select! {
                modal = async {
                    message
                        .await_modal_interaction(sctx)
                        .author_id(ctx.author().id)
                        .custom_ids(vec![modal_id.clone()])
                        .timeout(IDLE)
                        .await
                } => break modal,
                reclick = async {
                    message
                        .await_component_interaction(sctx)
                        .author_id(ctx.author().id)
                        .timeout(IDLE)
                        .await
                } => match reclick {
                    Some(rc) => {
                        rc.create_response(
                            sctx,
                            CreateInteractionResponse::Modal(email_modal(&modal_id, &display)),
                        )
                        .await?;
                        continue;
                    }
                    None => break None,
                },
            }
        };
        let Some(submit) = submit else {
            handle_msg
                .edit(
                    ctx,
                    reply(
                        state_embed(&display, &handle, &avatar, &VerifyState::Expired),
                        false,
                    ),
                )
                .await?;
            return Ok(());
        };

        // Acknowledge the submission immediately (a deferred message update) so the live
        // network work below cannot blow Discord's 3-second interaction deadline; the
        // outcome is written afterward by editing the same message.
        submit
            .create_response(sctx, CreateInteractionResponse::Acknowledge)
            .await?;

        let raw = read_input(&submit, EMAIL_FIELD_ID);
        let next = match parse_email(&raw) {
            None => VerifyState::InvalidEmail,
            Some(email) => {
                let data = ctx.data();
                match verify::verify_by_email(
                    &*data.solidarity_tech,
                    &*data.discord,
                    &*data.store,
                    &*data.auditor,
                    invoker,
                    target_id,
                    target_handle.clone(),
                    email,
                )
                .await
                {
                    Ok(VerifyOutcome::Verified(role)) => VerifyState::Verified(role),
                    Ok(VerifyOutcome::NotFound) => VerifyState::NotFound,
                    Ok(VerifyOutcome::Conflict) => VerifyState::Conflict,
                    Ok(VerifyOutcome::Unverified) => VerifyState::NotFound,
                    Err(e) => {
                        tracing::error!(error = %e, "manual verify by email failed");
                        VerifyState::Error
                    }
                }
            }
        };

        // Retry stays open on a recoverable state; everything else is terminal.
        let keep_button = matches!(next, VerifyState::NotFound | VerifyState::InvalidEmail);
        submit
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .embed(state_embed(&display, &handle, &avatar, &next))
                    .components(if keep_button {
                        vec![button_row.clone()]
                    } else {
                        vec![]
                    }),
            )
            .await?;

        if !keep_button {
            return Ok(());
        }
        // Loop: await the next button press on the same message.
    }
}

/// Read the value the moderator typed into the named modal field.
fn read_input(submit: &serenity::all::ModalInteraction, field_id: &str) -> String {
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
