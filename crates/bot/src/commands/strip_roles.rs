//! `/strip-roles` - staging-only: remove the managed roles from everyone in the server so
//! `/bulk-verify` can be retested from a clean slate. Registered only when
//! `BOT_STRIP_ROLES_COMMAND` is set, and the forget path it runs for hand-approved members
//! additionally needs the staging-only `DELETE` grant - two locks that keep this off
//! production entirely.
//!
//! A hand-approved member is forgotten outright (roles, override marker, cache link, and
//! stamp); everyone else only loses their managed status roles and the override marker,
//! with the database untouched. The marker is swept from every member defensively: staging
//! is exactly where a marker may have drifted from its stamp.

use std::time::Duration;

use engine::backends::util::DiscordUserId;
use engine::bulk;
use engine::store::{BulkScope, OverrideLog};
use engine::verify::{DataStore, Member, StripOutcome, Target};

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;

/// A conservative pause between members we write to, matching `/bulk-verify`'s apply pacing
/// so a whole-server strip stays well under Discord's role-write limits.
const STRIP_PACING: Duration = Duration::from_millis(500);

/// Strip the managed roles from every member in the server. Moderators only, staging only.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn strip_roles(ctx: Context<'_>) -> Result<(), Error> {
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
    // Whether the override marker is configured at all; when it is, the defensive
    // marker-strip fires a Discord write per member, so it counts toward pacing.
    let marker_configured = data.guild_config.load().manual_override_role.is_some();

    // Enumerating the roster and writing to every member takes well over Discord's 3s
    // interaction-response budget, so acknowledge first and edit in the summary at the end.
    ctx.defer_ephemeral().await?;

    let members = match bulk::enumerate(&discord, BulkScope::WholeGuild).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "strip-roles roster enumerate failed");
            ctx.send(plain(
                "Failed to fetch the member list. Please try again in a moment.",
            ))
            .await?;
            return Ok(());
        }
    };

    let invoker = DiscordUserId(ctx.author().id.get());
    let ds = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
    );

    let scanned = members.len();
    let mut forgotten = 0usize;
    let mut stripped = 0usize;
    let mut errors = 0usize;

    for m in &members {
        // The cache is the source of truth for "was hand-approved"; the marker role can
        // drift in staging, so it does not decide this.
        let overridden = match data.store.get_override(m.id).await {
            Ok(stamp) => stamp.is_some(),
            Err(e) => {
                tracing::warn!(user = %m.id, error = %e, "strip-roles: override lookup failed, skipping member");
                errors += 1;
                continue;
            }
        };

        match Member::new(
            &ds,
            Target {
                id: m.id,
                handle: m.handle.clone(),
            },
        )
        .strip(invoker, overridden, &m.held)
        .await
        {
            Ok(StripOutcome::Forgotten) => forgotten += 1,
            Ok(StripOutcome::Stripped) => stripped += 1,
            // One member's error never aborts the run.
            Err(e) => {
                tracing::warn!(user = %m.id, error = %e, "strip-roles: strip failed, continuing");
                errors += 1;
            }
        }

        // Pace only the members we actually wrote to: a forget always writes; a plain strip
        // writes when the member held a managed role or the marker is configured. Members
        // with nothing to clear fly past with no sleep.
        if overridden || !m.held.is_empty() || marker_configured {
            tokio::time::sleep(STRIP_PACING).await;
        }
    }

    let mut summary = format!(
        "Stripped managed roles from {scanned} member{} ({forgotten} fully reset, {stripped} status-only).",
        if scanned == 1 { "" } else { "s" },
    );
    if errors > 0 {
        summary.push_str(&format!(" {errors} failed - see the logs."));
    }
    ctx.send(plain(&summary)).await?;
    Ok(())
}
