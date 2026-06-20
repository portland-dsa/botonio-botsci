//! The moderator verify-and-assign use case: match a Discord member to their
//! Solidarity Tech record, repair the stored identity link, and assign the role their
//! standing earns.
//!
//! [`match_member`] is the pure decision - id first (the immutable key), then handle
//! as a repair fallback, with a guard that never re-links a record already bound to a
//! different account. [`verify`] is the orchestrator that executes a decision against
//! the backends, the cache, and the audit log.

use domain::Role;

use crate::audit::AuditLog;
use crate::backends::discord::DiscordClient;
use crate::backends::solidarity_tech::SolidarityTechClient;
use crate::store::{IdentityWrite, MemberRecord, MemberStore};
use crate::util::{DiscordHandle, DiscordUserId};

/// The identity repair a successful match implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealAction {
    /// Matched by id; the stored handle drifted and is updated to the current one.
    UpdateHandle(DiscordHandle),
    /// Matched by handle and the record had no id; backfill it.
    BackfillId(DiscordUserId),
    /// The stored identity already agrees; nothing to write back.
    None,
}

/// What [`match_member`] decided.
#[derive(Debug)]
pub enum MatchOutcome {
    /// A clean match, with the repair (if any) to apply.
    Matched {
        record: MemberRecord,
        heal: HealAction,
    },
    /// The handle resolves to a record already linked to a different account.
    Conflict,
    /// Solidarity Tech does not know this member by id or handle.
    Miss,
}

/// Decide the verification outcome from the two cache reads.
///
/// An id hit is authoritative and repairs only a drifted handle. On an id miss, a
/// handle hit either backfills a missing id or - if an id is already present, which
/// (since the id lookup missed) must be a *different* account's - is a conflict the
/// caller must not resolve automatically.
pub fn match_member(
    by_id: Option<MemberRecord>,
    by_handle: Option<MemberRecord>,
    target_id: DiscordUserId,
    target_handle: &DiscordHandle,
) -> MatchOutcome {
    if let Some(record) = by_id {
        let heal = if record.discord_handle.as_ref() == Some(target_handle) {
            HealAction::None
        } else {
            HealAction::UpdateHandle(target_handle.clone())
        };
        return MatchOutcome::Matched { record, heal };
    }
    if let Some(record) = by_handle {
        return match record.discord_user_id {
            None => MatchOutcome::Matched {
                record,
                heal: HealAction::BackfillId(target_id),
            },
            // An id is present and necessarily differs from `target_id` (an equal id
            // would have been found by the id lookup), so the handle now points at a
            // record bound to another account.
            Some(_) => MatchOutcome::Conflict,
        };
    }
    MatchOutcome::Miss
}

/// What a verification resolved to - the moderator-facing result.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Matched; the member was assigned this standing-derived role.
    Verified(Role),
    /// No record found; the member was assigned `Unverified`.
    Unverified,
    /// The handle is on record for a different account; nothing was changed.
    Conflict,
}

/// Why a verification could not complete. The two store/audit failures are stringified
/// (their concrete types are the store's and audit log's associated errors); the role
/// write keeps its own message. Each maps to a generic, PII-free reply at the bot.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("cache read failed: {0}")]
    Store(String),
    #[error("audit write failed: {0}")]
    Audit(String),
    #[error("discord role write failed: {0}")]
    Discord(String),
}

/// Verify `target` and assign their role, on behalf of moderator `invoker`.
///
/// Reads the cache by id then handle, decides via [`match_member`], records the
/// decided outcome to the audit log *before* any write (so no grant is unattributable;
/// an audit failure refuses the grant), assigns the role, and then repairs the stored
/// identity link in Solidarity Tech and the cache. The self-heal is best-effort: once
/// the role is set, a write-back failure is logged and the next run re-heals, rather
/// than denying the member a role they have earned.
pub async fn verify<St, Dc, S, A>(
    solidarity_tech: &St,
    discord: &Dc,
    store: &S,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    target_handle: DiscordHandle,
) -> Result<VerifyOutcome, VerifyError>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite,
    A: AuditLog,
{
    let by_id = store
        .by_discord_id(target)
        .await
        .map_err(|e| VerifyError::Store(e.to_string()))?;
    // The id hit wins; only read by handle when it missed.
    let by_handle = match &by_id {
        Some(_) => None,
        None => store
            .by_handle(&target_handle)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?,
    };

    match match_member(by_id, by_handle, target, &target_handle) {
        MatchOutcome::Matched { record, heal } => {
            let role = record.role();
            record_outcome(audit, invoker, target, "verified", Some(role)).await?;
            assign_role(discord, target, role).await?;
            self_heal(
                solidarity_tech,
                store,
                &record,
                target,
                &target_handle,
                &heal,
            )
            .await;
            Ok(VerifyOutcome::Verified(role))
        }
        MatchOutcome::Miss => {
            record_outcome(audit, invoker, target, "unverified", Some(Role::Unverified)).await?;
            assign_role(discord, target, Role::Unverified).await?;
            Ok(VerifyOutcome::Unverified)
        }
        MatchOutcome::Conflict => {
            // No role is granted, but the attempt is still recorded.
            record_outcome(audit, invoker, target, "conflict", None).await?;
            Ok(VerifyOutcome::Conflict)
        }
    }
}

/// Append one `member_verify` audit row. The detail is non-identifying: the outcome and
/// (when a role is granted) its name - never the conflicting account or any PII.
async fn record_outcome<A: AuditLog>(
    audit: &A,
    actor: DiscordUserId,
    subject: DiscordUserId,
    outcome: &str,
    role: Option<Role>,
) -> Result<(), VerifyError> {
    let detail = match role {
        Some(r) => serde_json::json!({ "outcome": outcome, "role": r.as_str() }),
        None => serde_json::json!({ "outcome": outcome }),
    };
    audit
        .record(actor, subject, "member_verify", detail)
        .await
        .map_err(|e| VerifyError::Audit(e.to_string()))
}

/// Set the member's status role, reading their current one first so the write no-ops
/// when it already matches.
async fn assign_role<Dc: DiscordClient>(
    discord: &Dc,
    target: DiscordUserId,
    role: Role,
) -> Result<(), VerifyError> {
    let current = discord
        .member_roles(target)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?
        .held
        .into_iter()
        .next();
    discord
        .set_role(target, current, role)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))
}

/// Write the discovered identity back to Solidarity Tech and then the cache. Best-effort:
/// the role is already granted, so a failure here is logged, not surfaced.
async fn self_heal<St, S>(
    solidarity_tech: &St,
    store: &S,
    record: &MemberRecord,
    target: DiscordUserId,
    handle: &DiscordHandle,
    heal: &HealAction,
) where
    St: SolidarityTechClient,
    S: IdentityWrite,
{
    let st_id = record.st_user_id.as_str();
    let st_result = match heal {
        HealAction::UpdateHandle(h) => solidarity_tech.set_discord_handle(st_id, h).await,
        HealAction::BackfillId(id) => {
            solidarity_tech
                .set_discord_identity(st_id, handle, *id)
                .await
        }
        HealAction::None => return,
    };
    if let Err(e) = st_result {
        tracing::warn!(error = %e, "verify: solidarity tech self-heal failed; role granted, will re-heal");
        return;
    }
    if let Err(e) = store
        .link_identity(&record.st_user_id, target, handle)
        .await
    {
        tracing::warn!(error = %e, "verify: cache write-through failed; role granted, will re-heal");
    }
}

#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::store::MemberRecord;
    use crate::util::{DiscordHandle, Email, StUserId};

    /// The security guard: a handle hit whose record already carries a *different* id
    /// (the id lookup missed, so it cannot be this member's) must never backfill - that
    /// would re-link another account's record. It is a conflict.
    #[test]
    fn handle_match_with_present_other_id_is_a_conflict() {
        let record = MemberRecord {
            st_user_id: StUserId("st-1".into()),
            discord_user_id: Some(DiscordUserId(5)),
            discord_handle: Some(DiscordHandle("rosy".into())),
            email: Email("m@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        };
        let out = match_member(
            None,
            Some(record),
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, MatchOutcome::Conflict));
    }
}
