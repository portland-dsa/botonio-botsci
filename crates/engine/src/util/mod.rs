//! Re-exported `domain` vocabulary used across the engine.
//!
//! The shared id newtypes and the [`DryRun`] flag, re-exported from `domain` so
//! the engine modules keep addressing them as `crate::util::...`.

pub use domain::DryRun;
pub use domain::dry_run;
pub use domain::ids;
pub use domain::ids::{
    DiscordChannelId, DiscordGuildId, DiscordHandle, DiscordUserId, Email, Phone, StUserId,
};
