//! The home-guild guard: the bot serves only its single configured guild and
//! removes itself from any other server it is in.
//!
//! Split so the behavior is exercisable offline (mocks only - there is no live
//! target): [`disposition`] is the pure decision, [`GuildLeaver`] is the one
//! Discord action behind a mockable seam, and [`on_guild_create`] is the handler
//! the gateway drives.

use serenity::all::GuildId;

/// Whether the bot belongs in a guild or should remove itself from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The configured home guild - stay and serve.
    Serve,
    /// Any other guild - the bot does not belong here.
    Leave,
}

/// The bot serves only its single configured home guild; every other guild is left.
pub fn disposition(incoming: GuildId, home: GuildId) -> Disposition {
    if incoming == home {
        Disposition::Serve
    } else {
        Disposition::Leave
    }
}

/// The one Discord action the guard needs: leave a guild. Behind a trait so the
/// behavior can be driven against a recording double with no live gateway.
#[async_trait::async_trait]
pub trait GuildLeaver {
    async fn leave(&self, guild: GuildId) -> Result<(), serenity::Error>;
}

/// Production implementation over serenity's HTTP client.
#[async_trait::async_trait]
impl GuildLeaver for serenity::http::Http {
    async fn leave(&self, guild: GuildId) -> Result<(), serenity::Error> {
        // serenity's inherent `leave_guild`; the trait method is named `leave` so
        // this call does not recurse into the trait.
        self.leave_guild(guild).await
    }
}

/// Handle a `GuildCreate`: serve the configured home guild, leave every other.
///
/// `is_new` (just-joined vs. already present at connect) does not change the
/// decision - the bot leaves either way; it only flavors the log. A failed leave
/// is logged, not propagated: the per-interaction command and ping guards still
/// refuse anything in the meantime.
pub async fn on_guild_create<L: GuildLeaver>(
    leaver: &L,
    incoming: GuildId,
    home: GuildId,
    is_new: Option<bool>,
) {
    match disposition(incoming, home) {
        Disposition::Serve => {
            tracing::debug!(
                guild_id = incoming.get(),
                "serving the configured home guild"
            );
        }
        Disposition::Leave => {
            tracing::warn!(
                guild_id = incoming.get(),
                newly_added = ?is_new,
                "leaving an unauthorized guild (not the configured home guild)"
            );
            if let Err(e) = leaver.leave(incoming).await {
                tracing::warn!(
                    guild_id = incoming.get(),
                    error = %e,
                    "failed to leave an unauthorized guild; per-interaction guards still reject it"
                );
            }
        }
    }
}
