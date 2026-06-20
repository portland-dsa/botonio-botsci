//! Re-exported `domain` vocabulary used across the engine.
//!
//! The shared id newtypes, re-exported from `domain` so the engine modules keep
//! addressing them as `crate::util::...`.

pub use domain::ids;
pub use domain::ids::{
    DiscordChannelId, DiscordGuildId, DiscordHandle, DiscordUserId, Email, Phone, StUserId,
};
