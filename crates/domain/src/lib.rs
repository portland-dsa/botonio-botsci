//! Pure, source-agnostic vocabulary shared by every other crate: the identifier
//! newtypes ([`ids`]), the managed [`Role`] tiers, the raw [`MigsStatus`]
//! membership-status value shared across sources, and the computed
//! [`MembershipStatus`] with its mapping to a role.
//!
//! This crate is the bottom of the dependency graph
//! (`domain <- backends <- engine <- bot`): it has no network, no `serenity`, and
//! no secrets, so everything above can link it without pulling in IO. Anything
//! that needs a backend's wire types or a client does not belong here.

#![forbid(unsafe_code)]

pub mod ids;
pub mod membership;
pub mod migs_status;
pub mod role;

pub use ids::{
    DiscordChannelId, DiscordGuildId, DiscordHandle, DiscordUserId, Email, Phone, StUserId,
};
pub use membership::MembershipStatus;
pub use migs_status::{MigsStatus, MigsStatusError, RetiredMigsStatus};
pub use role::Role;
