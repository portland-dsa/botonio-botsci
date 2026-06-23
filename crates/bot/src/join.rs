//! Auto-verification on join: when a member joins the served guild, run the same
//! id-first/handle-fallback match the moderator `/verify` performs and assign the role
//! their Solidarity Tech standing earns. Silent - the joining member sees no message.
//!
//! Receive-only over the gateway join event; every role write still goes through the
//! shared `DiscordHttp`/engine path, never a second write surface. The decision and the
//! writes are `engine::verify::Member::verify`, reused unchanged - this module only
//! decides *whether* to call it (skip bots and unconfigured guilds) and logs the
//! non-identifying outcome.

use serenity::all::{Context, Member};

use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::verify::{DataStore, Member as VerifyMember, Target, VerifyOutcome};

use crate::data::{Data, Error};

/// Whether a join event should be auto-verified. A pure decision, so the guard logic is
/// unit-tested without a gateway: bot accounts are never swept (the bulk-verify
/// self-sweep lesson), and a guild whose managed roles are unset has nothing to assign.
#[derive(Debug, PartialEq, Eq)]
enum JoinAction {
    Skip,
    Verify,
}

fn join_action(is_bot: bool, roles_configured: bool) -> JoinAction {
    if is_bot || !roles_configured {
        JoinAction::Skip
    } else {
        JoinAction::Verify
    }
}

/// Handle a join: match the joining member and assign the role their standing earns.
pub async fn on_guild_member_add(
    ctx: &Context,
    new_member: &Member,
    data: &Data,
) -> Result<(), Error> {
    // Suppress the unused-binding warning until a future slice (the self-service modal /
    // mod escalation) needs the gateway context; the decision and writes need only `data`.
    let _ = ctx;

    // Only the guild we serve - defence in depth beneath `guild_guard`, which already
    // makes the bot leave any guild other than this one.
    if new_member.guild_id.get() != data.config.guild_id {
        return Ok(());
    }

    // `role_writer()` is `Some` only when all three managed roles are configured, and is
    // also the client the assignment needs - so it doubles as the "configured" check.
    let writer = data.role_writer();
    if join_action(new_member.user.bot, writer.is_some()) == JoinAction::Skip {
        tracing::debug!(
            bot = new_member.user.bot,
            configured = writer.is_some(),
            "join verify skipped"
        );
        return Ok(());
    }
    // `Verify` is only returned when the writer is present, so this cannot panic.
    let discord = writer.expect("join_action returned Verify, so role_writer was Some");

    let target = Target {
        id: DiscordUserId(new_member.user.id.get()),
        handle: DiscordHandle(new_member.user.name.clone()),
    };
    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
    );
    // The bot acted on its own behalf; its id is the audit actor, which is what tells an
    // autonomous join-verify apart from a moderator `/verify` in the audit log.
    let actor = DiscordUserId(data.bot_user_id.get());

    // Member identifiers stay out of `info` logs; only the non-PII outcome is recorded.
    match VerifyMember::new(&store, target).verify(actor).await {
        Ok(VerifyOutcome::Verified(role)) => {
            tracing::info!(role = role.as_str(), "auto-verified a joining member");
        }
        Ok(VerifyOutcome::Unverified | VerifyOutcome::NotFound) => {
            tracing::info!("joining member not in Solidarity Tech; assigned Unverified");
        }
        Ok(VerifyOutcome::Malformed) => {
            tracing::warn!("joining member's record has no usable standing; no role assigned");
        }
        Ok(VerifyOutcome::Conflict) => {
            tracing::warn!(
                "joining member's handle is on record for a different account; no role assigned"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "auto-verify on join failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_accounts_are_skipped() {
        assert_eq!(join_action(true, true), JoinAction::Skip);
    }

    #[test]
    fn unconfigured_guild_is_skipped() {
        assert_eq!(join_action(false, false), JoinAction::Skip);
    }

    #[test]
    fn an_ordinary_joiner_with_roles_configured_is_verified() {
        assert_eq!(join_action(false, true), JoinAction::Verify);
    }

    #[test]
    fn a_bot_is_skipped_even_when_the_guild_is_unconfigured() {
        assert_eq!(join_action(true, false), JoinAction::Skip);
    }
}
