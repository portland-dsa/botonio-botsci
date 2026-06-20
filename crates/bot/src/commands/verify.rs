//! `/verify @member` - a moderator matches a member in Solidarity Tech and assigns
//! their role. Moderators only; every invocation is audited.

use serenity::all::User;

use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::verify::{self, VerifyOutcome};

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;

/// Verify a member and assign their standing role. Moderators only.
///
/// Hidden from non-moderators via `default_member_permissions`; the in-code
/// moderator check is the real gate.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn verify(
    ctx: Context<'_>,
    #[description = "The member to verify"] target: User,
) -> Result<(), Error> {
    let reply = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };

    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted a member verify");
        ctx.send(reply("That command is for moderators only."))
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
        target_handle,
    )
    .await;

    match result {
        Ok(VerifyOutcome::Verified(role)) => {
            ctx.send(reply(&format!(
                "Verified {} as {}.",
                target.name,
                role.as_str()
            )))
            .await?;
        }
        Ok(VerifyOutcome::Unverified) => {
            ctx.send(reply(&format!(
                "{} isn't in our records, so I assigned them Unverified.",
                target.name
            )))
            .await?;
        }
        Ok(VerifyOutcome::Conflict) => {
            tracing::warn!("verify hit a handle/account conflict");
            ctx.send(reply(
                "That handle is on record for a different account. \
                 Nothing was changed - please check the records by hand.",
            ))
            .await?;
        }
        Err(e) => {
            tracing::error!(error = %e, "member verify failed");
            ctx.send(reply(
                "Something went wrong on my end - please try again in a moment.",
            ))
            .await?;
        }
    }
    Ok(())
}
