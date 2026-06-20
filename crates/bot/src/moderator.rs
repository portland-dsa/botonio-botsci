//! The moderator gate: whether the invoker holds the configured moderator role.
//!
//! The single source of truth for "is this a moderator", shared by the help-topic
//! filter and the lookup authorization check, so the two never drift.

use serenity::all::RoleId;

use crate::data::Context;

/// Whether the invoker holds the configured moderator role. A member the gateway
/// cannot resolve (e.g. an interaction outside a guild) is treated as a
/// non-moderator.
pub async fn invoker_is_moderator(ctx: &Context<'_>) -> bool {
    let role_id = RoleId::new(ctx.data().config.moderator_role_id);
    match ctx.author_member().await {
        Some(member) => member.roles.contains(&role_id),
        None => false,
    }
}
