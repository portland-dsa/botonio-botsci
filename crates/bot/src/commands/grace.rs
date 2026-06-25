//! `/grace` - moderator command to grant or clear a dues-grace window for a member.
//!
//! `set <member> <days> [reason]`: grant grace for `days` days, promote the member to
//! `Member` on the spot. `clear <member>`: lift the grace early and revert to their
//! Solidarity-Tech-derived role. Both paths are audited (no PII in the audit row).

use chrono::Utc;
use serenity::all::User;

use domain::DiscordGuildId;
use engine::audit::AuditLog;
use engine::backends::util::DiscordUserId;
use engine::store::GraceStore;
use engine::verify::{DataStore, Member, Target};

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;

/// Grant or clear a dues-grace window for a member. Moderators only.
///
/// Hidden from non-moderators via `default_member_permissions`; the in-code
/// moderator check is the real gate.
#[poise::command(
    slash_command,
    default_member_permissions = "ADMINISTRATOR",
    guild_only,
    subcommands("set", "clear")
)]
pub async fn grace(_ctx: Context<'_>) -> Result<(), Error> {
    // poise dispatches to the subcommand; this root handler is never invoked directly.
    Ok(())
}

/// Grant a grace window of `days` days, starting today. Promotes the member to Member.
#[poise::command(slash_command)]
async fn set(
    ctx: Context<'_>,
    #[description = "The member to grant grace to"] target: User,
    #[description = "Number of days of grace (must be > 0)"] days: u32,
    #[description = "Optional reason (not recorded in the audit log)"] reason: Option<String>,
) -> Result<(), Error> {
    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted a grace set");
        ctx.send(
            poise::CreateReply::default()
                .content("That command is for moderators only.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    if days == 0 {
        ctx.send(
            poise::CreateReply::default()
                .content("Days must be greater than 0.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let invoker = DiscordUserId(ctx.author().id.get());
    let target_id = DiscordUserId(target.id.get());
    let data = ctx.data();
    let guild = data.config.guild();

    let today = Utc::now().date_naive();
    // N days of grace runs today through today + (N - 1) inclusive, matching the inclusive
    // `grace_until >= today` active check. days >= 1 is guaranteed above, so days - 1 >= 0.
    let until = today + chrono::Days::new(u64::from(days - 1));

    // Write the grace stamp.
    data.store
        .set_grace(guild, target_id, until, invoker, reason)
        .await
        .map_err(Error::Db)?;

    // Audit: no PII, no reason text - only the number of days is recorded.
    if let Err(e) = data
        .auditor
        .record(
            invoker,
            target_id,
            "grace_set",
            serde_json::json!({ "days": days }),
        )
        .await
    {
        tracing::warn!(error = %e, "failed to write grace_set audit row");
    }

    // Promote the member to Member now that grace is active.
    let on_success = format!(
        "Grace set: {} is held at Member until {} (inclusive).",
        target.name,
        until.format("%b %-d, %Y")
    );
    reverify_after_grace(&ctx, &target, invoker, guild, on_success).await
}

/// Lift a member's active grace early and revert to their Solidarity-Tech-derived role.
#[poise::command(slash_command)]
async fn clear(
    ctx: Context<'_>,
    #[description = "The member whose grace to clear"] target: User,
) -> Result<(), Error> {
    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted a grace clear");
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
    let data = ctx.data();
    let guild = data.config.guild();

    data.store
        .clear_grace(guild, target_id)
        .await
        .map_err(Error::Db)?;

    if let Err(e) = data
        .auditor
        .record(invoker, target_id, "grace_clear", serde_json::json!({}))
        .await
    {
        tracing::warn!(error = %e, "failed to write grace_clear audit row");
    }

    // Revert the member to their Solidarity-Tech-derived role now that grace is lifted.
    let on_success = format!(
        "Grace cleared for {}; role reverted to their current standing.",
        target.name
    );
    reverify_after_grace(&ctx, &target, invoker, guild, on_success).await
}

/// Shared tail of `/grace set` and `/grace clear`: re-run the verify path so the member's role
/// reflects their new standing, and report the result. The stamp (or its removal) is already
/// persisted by the caller, so a role-write failure is reported as a retryable follow-up rather
/// than a rollback. `on_success` is the caller's confirmation wording.
async fn reverify_after_grace(
    ctx: &Context<'_>,
    target: &User,
    invoker: DiscordUserId,
    guild: DiscordGuildId,
    on_success: String,
) -> Result<(), Error> {
    let data = ctx.data();
    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };
    let Some(discord) = data.role_writer() else {
        tracing::warn!("grace change attempted before managed roles were configured");
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
        guild,
    );
    let outcome = Member::new(
        &store,
        Target {
            id: DiscordUserId(target.id.get()),
            handle: engine::backends::util::DiscordHandle(target.name.clone()),
        },
    )
    .verify(invoker)
    .await;

    let msg = match outcome {
        Ok(_) => on_success,
        Err(e) => {
            tracing::error!(error = %e, "verify after grace change failed");
            format!(
                "The grace change was saved, but updating the role failed: {e}. \
                 Retry /verify to sync their role."
            )
        }
    };
    ctx.send(plain(&msg)).await?;
    Ok(())
}
