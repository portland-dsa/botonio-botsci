//! `/forget @member` - reset a member's bot state (managed roles, cache link, override
//! stamp) for testing. Registered only when `BOT_FORGET_COMMAND` is set, and the override
//! stamp delete additionally needs a `DELETE` grant present only in staging - two locks
//! that keep this off production entirely.

use serenity::all::User;

use engine::backends::util::DiscordUserId;
use engine::verify;

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;

/// Reset a member to an unverified, unlinked state. Moderators only.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn forget(
    ctx: Context<'_>,
    #[description = "The member to reset"] target: User,
) -> Result<(), Error> {
    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };
    if !invoker_is_moderator(&ctx).await {
        ctx.send(plain("That command is for moderators only."))
            .await?;
        return Ok(());
    }
    let data = ctx.data();
    let Some(discord) = data.role_writer() else {
        ctx.send(plain(
            "Roles are not configured yet - a server manager needs to run /setup first.",
        ))
        .await?;
        return Ok(());
    };
    let invoker = DiscordUserId(ctx.author().id.get());
    let target_id = DiscordUserId(target.id.get());
    match verify::forget_member(
        &discord,
        &*data.store,
        &*data.store,
        &*data.auditor,
        invoker,
        target_id,
    )
    .await
    {
        Ok(()) => {
            ctx.send(plain(&format!(
                "Reset {} to an unverified, unlinked state.",
                target.name
            )))
            .await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "forget failed");
            ctx.send(plain("Something went wrong resetting that member."))
                .await?;
        }
    }
    Ok(())
}
