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
use crate::store::{IdentityWrite, MemberRecord, MemberStore, OverrideLog};
use crate::util::{DiscordHandle, DiscordUserId, Email};

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
///
/// `#[must_use]`: discarding this throws away the conflict guard and the miss decision,
/// silently running an assignment against an unresolved member, so a caller that drops it
/// is a compile error rather than a security hole.
#[derive(Debug)]
#[must_use]
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

/// What [`match_by_email`] decided from the records a membership email resolved to.
///
/// `#[must_use]` for the same reason as [`MatchOutcome`]: dropping it would discard the
/// conflict guard and silently run an assignment against an unresolved member.
#[derive(Debug)]
#[must_use]
pub enum EmailMatchOutcome {
    /// One record is safe to claim for this Discord account, with the repair to apply.
    Matched {
        record: MemberRecord,
        heal: HealAction,
    },
    /// The email belongs to a record bound to a different account, or to several
    /// records none of which is already this account's - resolve by hand.
    Conflict,
    /// No Solidarity Tech record carries this email.
    Miss,
}

/// The [`HealAction`] a successful match implies, from the stored record and the
/// member's current identity. A record that already carries a Discord id only ever
/// needs its handle refreshed; one with no id is backfilled. Shared by
/// [`match_member`] (id/handle path) and [`match_by_email`] (email path) so both agree.
fn heal_for(
    record: &MemberRecord,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> HealAction {
    match record.discord_user_id {
        Some(_) => {
            if record.discord_handle.as_ref() == Some(target_handle) {
                HealAction::None
            } else {
                HealAction::UpdateHandle(target_handle.clone())
            }
        }
        None => HealAction::BackfillId(target),
    }
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
        let heal = heal_for(&record, target_id, target_handle);
        return MatchOutcome::Matched { record, heal };
    }
    if let Some(record) = by_handle {
        return match record.discord_user_id {
            None => {
                let heal = heal_for(&record, target_id, target_handle);
                MatchOutcome::Matched { record, heal }
            }
            // An id is present and necessarily differs from `target_id` (an equal id
            // would have been found by the id lookup), so the handle now points at a
            // record bound to another account.
            Some(_) => MatchOutcome::Conflict,
        };
    }
    MatchOutcome::Miss
}

/// Decide which (if any) of the records a typed email resolved to may be claimed for
/// `target`.
///
/// A unique email yields zero or one row in practice. Zero is a [`Miss`]. One is a clean
/// [`Matched`] when its stored Discord id is empty or already `target`'s, and a
/// [`Conflict`] when it is some other account's. More than one is a [`Matched`] only when
/// exactly one already carries `target`'s id (an idempotent re-verify); otherwise it is a
/// [`Conflict`], because the code never guesses which of several records to bind.
///
/// This is the email key's form of Part 1's impersonation guard: never bind an account to
/// a record that already belongs to a different one.
///
/// [`Miss`]: EmailMatchOutcome::Miss
/// [`Matched`]: EmailMatchOutcome::Matched
/// [`Conflict`]: EmailMatchOutcome::Conflict
pub fn match_by_email(
    matches: Vec<MemberRecord>,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> EmailMatchOutcome {
    // A record already bound to this account wins outright, at any match count.
    if let Some(record) = matches
        .iter()
        .find(|r| r.discord_user_id == Some(target))
        .cloned()
    {
        let heal = heal_for(&record, target, target_handle);
        return EmailMatchOutcome::Matched { record, heal };
    }
    match matches.len() {
        0 => EmailMatchOutcome::Miss,
        1 => {
            let record = matches.into_iter().next().expect("len checked");
            match record.discord_user_id {
                None => {
                    let heal = heal_for(&record, target, target_handle);
                    EmailMatchOutcome::Matched { record, heal }
                }
                Some(_) => EmailMatchOutcome::Conflict,
            }
        }
        _ => EmailMatchOutcome::Conflict,
    }
}

/// What a verification resolved to - the moderator-facing result.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Matched; the member was assigned this standing-derived role.
    Verified(Role),
    /// No record found; the member was assigned `Unverified`.
    Unverified,
    /// A manual email lookup found no record; nothing was changed (the member already
    /// holds `Unverified` from the automatic miss that opened the manual flow).
    NotFound,
    /// The handle is on record for a different account; nothing was changed.
    Conflict,
    /// A moderator hand-approved the member past Solidarity Tech; they were granted
    /// `Member` and the additive Manual Override marker.
    Overridden,
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
    #[error("solidarity tech read failed: {0}")]
    SolidarityTech(String),
    #[error("override stamp write failed: {0}")]
    Override(String),
}

/// How a verification was initiated, written to the audit row's `method` field so a query
/// or operator can tell the automatic id/handle path ([`verify`]) from the manual email
/// path ([`verify_by_email`]) from the hand-approval path ([`override_approve`]). A closed
/// set, so a call site cannot record a typo'd value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyMethod {
    /// The automatic match on the member's Discord id then handle.
    Discord,
    /// The moderator-supplied membership-email lookup.
    Email,
    /// The moderator's hand approval past Solidarity Tech.
    Override,
}

impl VerifyMethod {
    /// The stable string written to the audit `method` field.
    fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Email => "email",
            Self::Override => "override",
        }
    }
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
            record_outcome(
                audit,
                invoker,
                target,
                "verified",
                Some(role),
                VerifyMethod::Discord,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                role,
                VerifyMethod::Discord,
            )
            .await?;
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
            record_outcome(
                audit,
                invoker,
                target,
                "unverified",
                Some(Role::Unverified),
                VerifyMethod::Discord,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                Role::Unverified,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(VerifyOutcome::Unverified)
        }
        MatchOutcome::Conflict => {
            // No role is granted, but the attempt is still recorded.
            record_outcome(
                audit,
                invoker,
                target,
                "conflict",
                None,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(VerifyOutcome::Conflict)
        }
    }
}

/// Append one `member_verify` audit row. The detail is non-identifying: the outcome,
/// method, and (when a role is granted) its name - never the conflicting account or any PII.
async fn record_outcome<A: AuditLog>(
    audit: &A,
    actor: DiscordUserId,
    subject: DiscordUserId,
    outcome: &str,
    role: Option<Role>,
    method: VerifyMethod,
) -> Result<(), VerifyError> {
    let detail = match role {
        Some(r) => {
            serde_json::json!({ "outcome": outcome, "role": r.as_str(), "method": method.as_str() })
        }
        None => serde_json::json!({ "outcome": outcome, "method": method.as_str() }),
    };
    audit
        .record(actor, subject, "member_verify", detail)
        .await
        .map_err(|e| VerifyError::Audit(e.to_string()))
}

/// Set the member's status role to exactly `role`: add it if missing and strip every
/// *other* managed role they hold.
///
/// Reading the member's full set of managed roles (not just one) matters because
/// [`DiscordClient::set_role`] only removes the single role handed to it as `current`. A
/// member who has somehow accumulated two managed roles - a previous assignment whose
/// removal half failed, or a hand-applied role - would otherwise keep the stale extra. So
/// one held role drives `set_role` (stripped in the same call as the add) and any further
/// held roles are removed after. A member already holding exactly `role` is a no-op.
async fn assign_role<Dc: DiscordClient>(
    discord: &Dc,
    target: DiscordUserId,
    role: Role,
) -> Result<(), VerifyError> {
    let held = discord
        .member_roles(target)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?
        .held;
    // Every managed role to strip is everything held except the target itself.
    let stale: Vec<Role> = held.iter().copied().filter(|&r| r != role).collect();
    // Drive set_role's single removal with one stale role so it is stripped in the same
    // call as the add; with nothing stale, name the target only when it is already held,
    // which makes set_role a true no-op rather than a redundant re-add.
    let current = stale
        .first()
        .copied()
        .or_else(|| held.contains(&role).then_some(role));
    discord
        .set_role(target, current, role)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?;
    // Any held managed roles beyond the one set_role already removed.
    if stale.len() > 1 {
        discord
            .remove_roles(target, &stale[1..])
            .await
            .map_err(|e| VerifyError::Discord(e.to_string()))?;
    }
    Ok(())
}

/// Assign the role, and if the Discord write fails, append a `verify_failed` follow-up to
/// the audit log before surfacing the error.
///
/// The `verified`/`unverified` row was written *before* the attempt (audit-before-write,
/// so no grant is ever unattributable). A failed write would otherwise leave the log
/// showing a success that never landed; this reconciling row records that it did not. The
/// follow-up is best-effort - the role write already failed, so there is no granted action
/// left to gate - so its own failure is logged, not surfaced, and the caller still gets the
/// original Discord error.
async fn assign_role_or_record_failure<Dc, A>(
    discord: &Dc,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    role: Role,
    method: VerifyMethod,
) -> Result<(), VerifyError>
where
    Dc: DiscordClient,
    A: AuditLog,
{
    let Err(e) = assign_role(discord, target, role).await else {
        return Ok(());
    };
    if let Err(audit_err) =
        record_outcome(audit, invoker, target, "verify_failed", Some(role), method).await
    {
        tracing::warn!(
            error = %audit_err,
            "verify: could not record the verify_failed follow-up after a failed role write"
        );
    }
    Err(e)
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

/// Verify `target` from a moderator-supplied membership `email`, the manual fallback when
/// the automatic id/handle match in [`verify`] misses.
///
/// Reads Solidarity Tech live by email (the cache holds no email index), projects the
/// hits, and decides with [`match_by_email`]. A match audits before writing, assigns the
/// standing role, and writes the discovered Discord identity back to Solidarity Tech and
/// the cache (best-effort, as in [`verify`]). A miss writes no role - the member already
/// holds `Unverified` from the automatic miss - and a conflict writes nothing. Every
/// outcome is audited with `method: "email"`, never the email itself.
#[allow(clippy::too_many_arguments)]
pub async fn verify_by_email<St, Dc, S, A>(
    solidarity_tech: &St,
    discord: &Dc,
    store: &S,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    target_handle: DiscordHandle,
    email: Email,
) -> Result<VerifyOutcome, VerifyError>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite,
    A: AuditLog,
{
    let members = solidarity_tech
        .find_by_email(&email)
        .await
        .map_err(|e| VerifyError::SolidarityTech(e.to_string()))?;
    let records: Vec<MemberRecord> = members.into_iter().map(MemberRecord::from).collect();

    match match_by_email(records, target, &target_handle) {
        EmailMatchOutcome::Matched { record, heal } => {
            let role = record.role();
            record_outcome(
                audit,
                invoker,
                target,
                "verified",
                Some(role),
                VerifyMethod::Email,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                role,
                VerifyMethod::Email,
            )
            .await?;
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
        EmailMatchOutcome::Conflict => {
            record_outcome(
                audit,
                invoker,
                target,
                "conflict",
                None,
                VerifyMethod::Email,
            )
            .await?;
            Ok(VerifyOutcome::Conflict)
        }
        EmailMatchOutcome::Miss => {
            record_outcome(
                audit,
                invoker,
                target,
                "not_found",
                None,
                VerifyMethod::Email,
            )
            .await?;
            Ok(VerifyOutcome::NotFound)
        }
    }
}

/// Hand-approve `target` past Solidarity Tech, on behalf of moderator `invoker` - the
/// escape hatch when no record can be matched. Grants `Member` and the additive Manual
/// Override marker, and stamps the approval permanently.
///
/// Fail-closed twice before any write: the audit row, then the override stamp, so the
/// "your approval has been logged" promise holds whenever a role is granted. The stamp is
/// insert-once, so a retry after a later role-write failure is idempotent. The marker is
/// added after `Member`; the status-role logic never strips it.
pub async fn override_approve<Dc, O, A>(
    discord: &Dc,
    override_log: &O,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
) -> Result<VerifyOutcome, VerifyError>
where
    Dc: DiscordClient,
    O: OverrideLog,
    A: AuditLog,
{
    record_outcome(
        audit,
        invoker,
        target,
        "override",
        Some(Role::Member),
        VerifyMethod::Override,
    )
    .await?;
    override_log
        .stamp_override(target, invoker)
        .await
        .map_err(|e| VerifyError::Override(e.to_string()))?;
    assign_role_or_record_failure(
        discord,
        audit,
        invoker,
        target,
        Role::Member,
        VerifyMethod::Override,
    )
    .await?;
    if let Err(e) = discord.assign_override_marker(target).await {
        // The marker is the secondary half of the grant; Member is already set and the
        // stamp already written, so a retry re-adds only the marker (idempotent).
        if let Err(audit_err) = record_outcome(
            audit,
            invoker,
            target,
            "override_marker_failed",
            None,
            VerifyMethod::Override,
        )
        .await
        {
            tracing::warn!(error = %audit_err, "override: could not record the marker-failure follow-up");
        }
        return Err(VerifyError::Discord(e.to_string()));
    }
    Ok(VerifyOutcome::Overridden)
}

#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::store::MemberRecord;
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use domain::MigsStatus;

    fn rec(st: &str, id: Option<u64>, handle: &str) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId(st.into()),
            discord_user_id: id.map(DiscordUserId),
            discord_handle: Some(DiscordHandle(handle.into())),
            email: Email("m@b.test".into()),
            full_name: None,
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

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

    #[test]
    fn email_zero_matches_is_a_miss() {
        let out = match_by_email(vec![], DiscordUserId(9), &DiscordHandle("rosy".into()));
        assert!(matches!(out, EmailMatchOutcome::Miss));
    }

    #[test]
    fn email_single_unlinked_match_backfills_the_id() {
        let out = match_by_email(
            vec![rec("st-1", None, "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        match out {
            EmailMatchOutcome::Matched { heal, .. } => {
                assert_eq!(heal, HealAction::BackfillId(DiscordUserId(9)));
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn email_single_match_already_ours_with_drifted_handle_updates_handle() {
        let out = match_by_email(
            vec![rec("st-1", Some(9), "old")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        match out {
            EmailMatchOutcome::Matched { heal, .. } => {
                assert_eq!(heal, HealAction::UpdateHandle(DiscordHandle("rosy".into())));
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn email_single_match_bound_to_other_account_is_a_conflict() {
        let out = match_by_email(
            vec![rec("st-1", Some(5), "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Conflict));
    }

    #[test]
    fn email_many_matches_none_ours_is_a_conflict() {
        let out = match_by_email(
            vec![rec("st-1", None, "rosy"), rec("st-2", Some(5), "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Conflict));
    }

    #[test]
    fn email_many_matches_one_already_ours_is_matched() {
        let out = match_by_email(
            vec![rec("st-1", Some(9), "rosy"), rec("st-2", None, "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Matched { .. }));
    }
}
