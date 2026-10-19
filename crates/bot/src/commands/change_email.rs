//! `/change-email @member` and the "Change Email" right-click entry - a moderator moves a
//! member's Discord link onto the Solidarity Tech record under a new email, for a member who
//! signed up again and so has a second record. Moderators only; every outcome is audited.

use std::time::Duration;

use serenity::all::{
    CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    ModalInteractionCollector, User,
};

use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::verify::{DataStore, Member, Target};

use crate::commands::verify::{header, read_input};
use crate::data::{Context, Error};
use crate::moderator::{MODERATORS_ONLY, invoker_is_moderator};
use crate::render::change_email::{ChangeEmailState, change_email_embed};
use crate::render::modal::{EMAIL_FIELD_ID, change_email_modal, parse_email};

/// The email modal's custom id prefix; the interaction id is appended so concurrent changes
/// never collect each other's submissions.
const CHANGE_EMAIL_MODAL_ID: &str = "change_email_modal";

/// How long the email modal may stay open before its collector gives up.
const MODAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Move a member onto their record under a new email. Moderators only.
///
/// Hidden from members without Manage Roles via `default_member_permissions`; the in-code
/// moderator check is the real gate.
#[poise::command(
    slash_command,
    rename = "change-email",
    default_member_permissions = "MANAGE_ROLES",
    guild_only
)]
pub async fn change_email(
    ctx: Context<'_>,
    #[description = "The member whose email changed"] target: User,
) -> Result<(), Error> {
    run(ctx, &target).await
}

/// Right-click form of [`change_email`].
#[poise::command(
    context_menu_command = "Change Email",
    default_member_permissions = "MANAGE_ROLES",
    guild_only
)]
pub async fn change_email_menu(ctx: Context<'_>, target: User) -> Result<(), Error> {
    run(ctx, &target).await
}

/// The shared flow: gate, open the email modal on the command interaction, then relink from
/// the submitted email and report the outcome through the modal's own interaction.
async fn run(ctx: Context<'_>, target: &User) -> Result<(), Error> {
    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };

    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted an email change");
        ctx.send(plain(MODERATORS_ONLY)).await?;
        return Ok(());
    }
    let Some(discord) = ctx.data().role_writer() else {
        tracing::warn!("email change attempted before the managed roles were configured");
        ctx.send(plain(
            "Roles are not configured yet - a server manager needs to run /setup first.",
        ))
        .await?;
        return Ok(());
    };
    // Both entries are application commands; only their interaction can answer with a modal.
    let poise::Context::Application(actx) = ctx else {
        return Ok(());
    };

    let (display, handle, avatar) = header(target);
    let sctx = ctx.serenity_context();
    let modal_id = format!("{CHANGE_EMAIL_MODAL_ID}:{}", actx.interaction.id.get());
    actx.interaction
        .create_response(
            sctx,
            CreateInteractionResponse::Modal(change_email_modal(&modal_id, &display)),
        )
        .await?;

    let Some(submit) = ModalInteractionCollector::new(sctx)
        .author_id(ctx.author().id)
        .custom_ids(vec![modal_id])
        .timeout(MODAL_TIMEOUT)
        .await
    else {
        // Dismissed, or left open past the window: nothing was submitted, so nothing changed.
        return Ok(());
    };
    // Defer so the live Solidarity Tech reads and writes below cannot blow Discord's
    // 3-second interaction deadline; the outcome is edited in afterward.
    submit
        .create_response(
            sctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;

    let state = match parse_email(&read_input(&submit, EMAIL_FIELD_ID)) {
        None => ChangeEmailState::InvalidEmail,
        Some(email) => {
            let data = ctx.data();
            let store = DataStore::new(
                &*data.solidarity_tech,
                &discord,
                &*data.store,
                &*data.auditor,
                data.config.guild(),
            );
            let member = Member::new(
                &store,
                Target {
                    id: DiscordUserId(target.id.get()),
                    handle: DiscordHandle(target.name.clone()),
                },
            );
            match member
                .relink_by_email(DiscordUserId(ctx.author().id.get()), email)
                .await
            {
                Ok(outcome) => ChangeEmailState::Done(outcome),
                Err(e) => {
                    tracing::error!(error = %e, "email change failed");
                    ChangeEmailState::Error
                }
            }
        }
    };
    submit
        .edit_response(
            sctx,
            EditInteractionResponse::new()
                .embed(change_email_embed(&display, &handle, &avatar, &state)),
        )
        .await?;
    Ok(())
}
