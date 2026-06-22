//! `/refresh-cache` - re-warm the member cache from Solidarity Tech on demand, the same
//! sweep the background loop runs on its own cadence. Moderators only; throttled by a
//! process-wide cooldown so it cannot hammer the API.
//!
//! Useful in every environment, not just for testing: `/verify` does not reach past the
//! cache when it misses (searching Solidarity Tech by Discord identity would mean
//! enumerating the whole backend, which has no such filter), so this is the explicit way
//! to pull a freshly-added member into the cache before verifying them.

use std::time::Instant;

use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;
use crate::refresh::{self, RefreshReport};

/// Re-warm the member cache from Solidarity Tech. Moderators only.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn refresh_cache(ctx: Context<'_>) -> Result<(), Error> {
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

    // Throttle before any work: the spend is recorded the moment the check passes, so a
    // slow or failing sweep still holds the window and concurrent calls cannot both sweep.
    if let Err(remaining) = data.refresh_cooldown.check(Instant::now()) {
        // Round up to whole seconds so the message never says "try again in 0s".
        let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
        ctx.send(plain(&format!(
            "The cache was refreshed recently. Try again in {secs}s."
        )))
        .await?;
        return Ok(());
    }

    // The sweep can take a few seconds; acknowledge first, then edit in the result.
    ctx.defer_ephemeral().await?;

    let report = refresh::refresh_once(
        &data.store,
        &data.solidarity_tech,
        &data.config.discord_list_id,
    )
    .await;

    let msg = match report {
        RefreshReport::Loaded(n) => format!(
            "Refreshed the member cache: {n} member{} loaded.",
            if n == 1 { "" } else { "s" }
        ),
        RefreshReport::Empty => {
            "Solidarity Tech returned no members; kept the last good roster.".to_owned()
        }
        RefreshReport::Failed => "The refresh failed; kept the last good roster.".to_owned(),
    };
    ctx.send(plain(&msg)).await?;
    Ok(())
}
