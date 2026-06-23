//! `/verify @member` - a moderator matches a member in Solidarity Tech and assigns
//! their role. Moderators only; every invocation is audited. On an automatic miss, the
//! moderator can supply the member's email by hand.

use std::time::Duration;

use serenity::all::{
    ActionRowComponent, ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton,
    CreateEmbed, CreateInteractionResponse, EditInteractionResponse, Message, User,
};

use engine::backends::discord::DiscordHttp;
use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::verify::{DataStore, Member, Target, VerifyOutcome};

use domain::Role;

use crate::commands::reclick::{ReclickAction, reclick_action};
use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;
use crate::render::modal::{
    EMAIL_FIELD_ID, EMAIL_MODAL_ID, OVERRIDE_MODAL_ID, REASON_FIELD_ID, email_modal,
    override_modal, parse_email, parse_reason,
};
use crate::render::verify::{VerifyState, state_embed};

/// The "Look up by email" button id and the inactivity window the collectors wait.
const LOOKUP_BUTTON_ID: &str = "verify_lookup_email";
const IDLE: Duration = Duration::from_secs(180);

/// The "Override and approve" button id - shown only on the not-found state.
const OVERRIDE_BUTTON_ID: &str = "verify_override_approve";

/// Outcome returned by `verify_step` once the exchange with the moderator ends.
// `Verified`'s inner `Role` is not read by the current callers.
#[allow(dead_code)]
pub(crate) enum StepOutcome {
    /// The member was found in Solidarity Tech and the role was assigned.
    Verified(Role),
    /// The member was hand-approved via the Manual Override path.
    Overridden,
    /// The moderator exhausted retries and did not override (or pressed a wizard control).
    NotFoundExhausted,
    /// Solidarity Tech found a handle/account conflict; nothing was changed.
    Conflict,
    /// The moderator did not interact within the idle window.
    Expired,
    /// A backend error occurred.
    Errored,
}

/// Result of waiting on an open modal while its trigger button stays live. A modal
/// dismissed without submitting fires no event, so the wait also watches for further
/// presses on the same row: a re-click of the trigger reopens the modal in place (handled
/// inline, no variant), and any other button is stashed for the outer dispatch.
enum ModalWait {
    /// The moderator submitted the modal. Boxed because `ModalInteraction` is large and
    /// the other variants carry nothing (clippy::large_enum_variant).
    Submitted(Box<serenity::all::ModalInteraction>),
    /// The moderator pressed a different live button; it is stashed in `pending` for the
    /// outer loop to route (Skip/Stop, Override, or a fresh lookup).
    Redispatch,
    /// The idle window elapsed with no interaction.
    TimedOut,
}

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

    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };

    let invoker = DiscordUserId(ctx.author().id.get());
    let target_id = DiscordUserId(target.id.get());
    let target_handle = DiscordHandle(target.name.clone());
    let data = ctx.data();

    let Some(discord) = data.role_writer() else {
        tracing::warn!("verify attempted before the managed roles were configured");
        ctx.send(plain(
            "Roles are not configured yet - a server manager needs to run /setup first.",
        ))
        .await?;
        return Ok(());
    };

    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
    );
    let result = Member::new(
        &store,
        Target {
            id: target_id,
            handle: target_handle.clone(),
        },
    )
    .verify(invoker)
    .await;

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
            manual_verify_flow(ctx, &discord, &target, invoker, target_id, target_handle).await?;
        }
        Ok(VerifyOutcome::Malformed) => {
            malformed_flow(ctx, &discord, &target, invoker, target_id, target_handle).await?;
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
    discord: &DiscordHttp,
    target: &User,
    invoker: DiscordUserId,
    target_id: DiscordUserId,
    target_handle: DiscordHandle,
) -> Result<(), Error> {
    let (display, handle, avatar) = header(target);

    let reply = |embed: CreateEmbed, components: Vec<CreateActionRow>| {
        poise::CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .components(components)
    };

    let handle_msg = ctx
        .send(reply(
            state_embed(&display, &handle, &avatar, &VerifyState::Prompt),
            vec![buttons_for(&VerifyState::Prompt, &[])],
        ))
        .await?;
    let message = handle_msg.message().await?;

    let (outcome, _) = verify_step(
        ctx,
        &message,
        discord,
        target,
        invoker,
        target_id,
        target_handle,
        &[],
    )
    .await?;

    // The step itself edits the message for most terminal states via interaction
    // responses. The only state that requires editing the host reply handle directly
    // is a timeout, where no interaction is available.
    if matches!(outcome, StepOutcome::Expired) {
        handle_msg
            .edit(
                ctx,
                reply(
                    state_embed(&display, &handle, &avatar, &VerifyState::Expired),
                    vec![],
                ),
            )
            .await?;
    }

    Ok(())
}

/// One ephemeral message seeded with the malformed state and an override-only button.
/// Reuses `verify_step` so the Override path is identical to the manual flow; the email
/// lookup is intentionally absent (the member is already located in Solidarity Tech).
async fn malformed_flow(
    ctx: Context<'_>,
    discord: &DiscordHttp,
    target: &User,
    invoker: DiscordUserId,
    target_id: DiscordUserId,
    target_handle: DiscordHandle,
) -> Result<(), Error> {
    let (display, handle, avatar) = header(target);
    let reply = |embed: CreateEmbed, components: Vec<CreateActionRow>| {
        poise::CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .components(components)
    };
    let handle_msg = ctx
        .send(reply(
            state_embed(&display, &handle, &avatar, &VerifyState::Malformed),
            vec![override_only_buttons(&[])],
        ))
        .await?;
    let message = handle_msg.message().await?;
    let (outcome, _) = verify_step(
        ctx,
        &message,
        discord,
        target,
        invoker,
        target_id,
        target_handle,
        &[],
    )
    .await?;
    if matches!(outcome, StepOutcome::Expired) {
        handle_msg
            .edit(
                ctx,
                reply(
                    state_embed(&display, &handle, &avatar, &VerifyState::Expired),
                    vec![],
                ),
            )
            .await?;
    }
    Ok(())
}

/// Drive the email-lookup / override exchange for a single member on an already-sent
/// host message. Returns the outcome and, if the moderator pressed one of
/// `extra_buttons` (wizard controls like Skip or Stop), the interaction for that press
/// so the caller can own those semantics.
///
/// For `/verify`, pass `&[]` for `extra_buttons` and ignore the second field; the
/// host message is created by `manual_verify_flow` and the result is discarded.
///
/// On `StepOutcome::Expired`, the caller is responsible for editing the host message
/// to show the expired state (the step has no access to the reply handle required for
/// ephemeral message edits when no interaction is present).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_step(
    ctx: Context<'_>,
    message: &Message,
    discord: &DiscordHttp,
    target: &User,
    invoker: DiscordUserId,
    target_id: DiscordUserId,
    target_handle: DiscordHandle,
    extra_buttons: &[CreateButton],
) -> Result<(StepOutcome, Option<ComponentInteraction>), Error> {
    let (display, handle, avatar) = header(target);

    let modal_id = format!("{EMAIL_MODAL_ID}:{}", message.id.get());
    let sctx = ctx.serenity_context();

    // A button press waiting to be dispatched. Normally we await a fresh press at the top
    // of the loop, but a modal dismissed without submitting leaves its trigger button live;
    // a follow-up press is captured by the modal wait below and stashed here so this same
    // dispatch handles it (reopen the lookup, Skip/Stop, or Override).
    let mut pending: Option<ComponentInteraction> = None;
    loop {
        // Take a stashed press, or wait for the moderator to press a button.
        let button = match pending.take() {
            Some(button) => button,
            None => {
                let Some(button) = message
                    .await_component_interaction(sctx)
                    .author_id(ctx.author().id)
                    .timeout(IDLE)
                    .await
                else {
                    // No interaction arrived before the idle window closed. The caller
                    // edits the host message to show the expired state because ephemeral
                    // edits require the original reply handle, which the step does not hold.
                    return Ok((StepOutcome::Expired, None));
                };
                button
            }
        };

        // If the moderator pressed one of the wizard's extra controls (Skip, Stop,
        // etc.), return that interaction immediately so the caller can handle it. These
        // are any button that is not one the step itself owns.
        let is_step_button = button.data.custom_id == LOOKUP_BUTTON_ID
            || button.data.custom_id == OVERRIDE_BUTTON_ID;
        if !extra_buttons.is_empty() && !is_step_button {
            return Ok((StepOutcome::NotFoundExhausted, Some(button)));
        }

        // The override button is the deliberate last resort: hand-approve a member
        // Solidarity Tech does not know, granting Member plus the Manual Override marker.
        if button.data.custom_id == OVERRIDE_BUTTON_ID {
            let data = ctx.data();
            // Hand-approval grants the Manual Override marker, so it needs that role
            // configured; refuse before collecting a reason rather than approve without the
            // marker it promises.
            if data.guild_config.load().manual_override_role.is_none() {
                tracing::warn!("override pressed but no Manual Override role is configured");
                button
                    .create_response(sctx, CreateInteractionResponse::Acknowledge)
                    .await?;
                button
                    .edit_response(
                        sctx,
                        EditInteractionResponse::new()
                            .embed(state_embed(&display, &handle, &avatar, &VerifyState::Error))
                            .components(vec![]),
                    )
                    .await?;
                return Ok((StepOutcome::Errored, None));
            }

            // Collect an optional reason in a modal before approving.
            let override_modal_id = format!("{OVERRIDE_MODAL_ID}:{}", message.id.get());
            button
                .create_response(
                    sctx,
                    CreateInteractionResponse::Modal(override_modal(&override_modal_id, &display)),
                )
                .await?;
            // A dismissed modal sends no event, so also watch for a further button press.
            // A re-click of the override button reopens the modal in place; any other button
            // is stashed for the outer dispatch (so Skip/Stop and the email lookup still work
            // after the reason modal is dismissed).
            let wait = loop {
                tokio::select! {
                    modal = async {
                        message
                            .await_modal_interaction(sctx)
                            .author_id(ctx.author().id)
                            .custom_ids(vec![override_modal_id.clone()])
                            .timeout(IDLE)
                            .await
                    } => break modal.map_or(ModalWait::TimedOut, |m| ModalWait::Submitted(Box::new(m))),
                    reclick = async {
                        message
                            .await_component_interaction(sctx)
                            .author_id(ctx.author().id)
                            .timeout(IDLE)
                            .await
                    } => match reclick {
                        Some(rc) => match reclick_action(&rc.data.custom_id, OVERRIDE_BUTTON_ID) {
                            ReclickAction::Reopen => {
                                rc.create_response(
                                    sctx,
                                    CreateInteractionResponse::Modal(override_modal(
                                        &override_modal_id,
                                        &display,
                                    )),
                                )
                                .await?;
                                continue;
                            }
                            ReclickAction::Redispatch => {
                                pending = Some(rc);
                                break ModalWait::Redispatch;
                            }
                        },
                        None => break ModalWait::TimedOut,
                    },
                }
            };
            let submit = match wait {
                ModalWait::Submitted(submit) => *submit,
                ModalWait::Redispatch => continue,
                ModalWait::TimedOut => return Ok((StepOutcome::Expired, None)),
            };
            // Acknowledge so the audit/stamp/role writes below cannot blow Discord's
            // 3-second interaction deadline.
            submit
                .create_response(sctx, CreateInteractionResponse::Acknowledge)
                .await?;
            let reason = parse_reason(&read_input(&submit, REASON_FIELD_ID));
            let store = DataStore::new(
                &*data.solidarity_tech,
                discord,
                &*data.store,
                &*data.auditor,
            );
            let (next, outcome) = match Member::new(
                &store,
                Target {
                    id: target_id,
                    handle: target_handle.clone(),
                },
            )
            .override_approve(invoker, reason)
            .await
            {
                Ok(()) => (VerifyState::Overridden, StepOutcome::Overridden),
                Err(e) => {
                    tracing::error!(error = %e, "override approve failed");
                    (VerifyState::Error, StepOutcome::Errored)
                }
            };
            submit
                .edit_response(
                    sctx,
                    EditInteractionResponse::new()
                        .embed(state_embed(&display, &handle, &avatar, &next))
                        .components(vec![]),
                )
                .await?;
            return Ok((outcome, None));
        }

        // Open the modal in response to the button.
        button
            .create_response(
                sctx,
                CreateInteractionResponse::Modal(email_modal(&modal_id, &display)),
            )
            .await?;

        // Await the submission. Dismissing a modal sends no event, so also watch for a
        // further press on the still-visible row. A re-click of the lookup button reopens
        // the modal in place; any other button (Skip/Stop/Override) is stashed so the outer
        // dispatch handles it - otherwise the row would be dead until timeout, and a
        // Skip/Stop press would wrongly reopen the email modal.
        let wait = loop {
            tokio::select! {
                modal = async {
                    message
                        .await_modal_interaction(sctx)
                        .author_id(ctx.author().id)
                        .custom_ids(vec![modal_id.clone()])
                        .timeout(IDLE)
                        .await
                } => break modal.map_or(ModalWait::TimedOut, |m| ModalWait::Submitted(Box::new(m))),
                reclick = async {
                    message
                        .await_component_interaction(sctx)
                        .author_id(ctx.author().id)
                        .timeout(IDLE)
                        .await
                } => match reclick {
                    Some(rc) => match reclick_action(&rc.data.custom_id, LOOKUP_BUTTON_ID) {
                        ReclickAction::Reopen => {
                            rc.create_response(
                                sctx,
                                CreateInteractionResponse::Modal(email_modal(&modal_id, &display)),
                            )
                            .await?;
                            continue;
                        }
                        ReclickAction::Redispatch => {
                            pending = Some(rc);
                            break ModalWait::Redispatch;
                        }
                    },
                    None => break ModalWait::TimedOut,
                },
            }
        };
        let submit = match wait {
            ModalWait::Submitted(submit) => *submit,
            ModalWait::Redispatch => continue,
            ModalWait::TimedOut => return Ok((StepOutcome::Expired, None)),
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
                let store = DataStore::new(
                    &*data.solidarity_tech,
                    discord,
                    &*data.store,
                    &*data.auditor,
                );
                match Member::new(
                    &store,
                    Target {
                        id: target_id,
                        handle: target_handle.clone(),
                    },
                )
                .verify_by_email(invoker, email)
                .await
                {
                    Ok(VerifyOutcome::Verified(role)) => VerifyState::Verified(role),
                    Ok(VerifyOutcome::NotFound) => VerifyState::NotFound,
                    Ok(VerifyOutcome::Conflict) => VerifyState::Conflict,
                    Ok(VerifyOutcome::Unverified) => VerifyState::NotFound,
                    Ok(VerifyOutcome::Malformed) => VerifyState::Malformed,
                    Err(e) => {
                        tracing::error!(error = %e, "manual verify by email failed");
                        VerifyState::Error
                    }
                }
            }
        };

        // What stays on the message decides whether this step is done. A recoverable miss
        // or invalid email keeps the lookup button (and, once missed, the override button);
        // a malformed record keeps an override-only row - no lookup, since the member is
        // already located in Solidarity Tech - so the moderator can still hand-approve them.
        // Every other state is terminal and clears the buttons.
        let components = match next {
            VerifyState::NotFound | VerifyState::InvalidEmail => {
                vec![buttons_for(&next, extra_buttons)]
            }
            VerifyState::Malformed => vec![override_only_buttons(extra_buttons)],
            _ => vec![],
        };
        let keep_live = !components.is_empty();
        submit
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .embed(state_embed(&display, &handle, &avatar, &next))
                    .components(components),
            )
            .await?;

        if !keep_live {
            let outcome = match next {
                VerifyState::Verified(role) => StepOutcome::Verified(role),
                VerifyState::Conflict => StepOutcome::Conflict,
                _ => StepOutcome::Errored,
            };
            return Ok((outcome, None));
        }
        // Loop: await the next button press on the same message.
    }
}

/// An override-only button row: the deliberate sole remedy for a malformed record (no
/// email lookup, since the member is already located). Used by `/verify` and the bulk
/// wizard's malformed step.
pub(crate) fn override_only_buttons(extra: &[CreateButton]) -> CreateActionRow {
    let mut buttons = vec![
        CreateButton::new(OVERRIDE_BUTTON_ID)
            .label("Override and approve")
            .style(ButtonStyle::Danger),
    ];
    buttons.extend(extra.iter().cloned());
    CreateActionRow::Buttons(buttons)
}

/// The button row for a given state: the lookup/retry button always, plus the red
/// override button once a lookup has missed (the deliberate last resort), and any
/// extra buttons the caller provides (e.g., Skip/Stop for the bulk wizard).
fn buttons_for(state: &VerifyState, extra_buttons: &[CreateButton]) -> CreateActionRow {
    let mut buttons = vec![
        CreateButton::new(LOOKUP_BUTTON_ID)
            .label("Look up by email")
            .style(ButtonStyle::Primary),
    ];
    if matches!(state, VerifyState::NotFound) {
        buttons.push(
            CreateButton::new(OVERRIDE_BUTTON_ID)
                .label("Override and approve")
                .style(ButtonStyle::Danger),
        );
    }
    buttons.extend(extra_buttons.iter().cloned());
    CreateActionRow::Buttons(buttons)
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
