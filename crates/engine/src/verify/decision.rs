//! Pure decision layer: role-assignment rules and match/locate logic.

use crate::store::{MemberRecord, MemberStore};
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

/// What [`decide`] decided.
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
/// [`decide`] (id/handle path) and [`match_by_email`] (email path) so both agree.
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

/// Where a cache lookup found a member's record - the input to [`decide`], replacing the
/// `(Option, Option)` pair the verbs used to thread into the old read-and-match block.
///
/// [`ById`](Located::ById) is the authoritative hit on the immutable key; [`ByHandle`](Located::ByHandle)
/// is the repair-or-conflict branch reached only after an id miss; [`Unknown`](Located::Unknown)
/// is a miss by both. `#[must_use]`: dropping it discards the decision a verify must act on.
#[derive(Debug)]
#[must_use]
pub enum Located {
    /// Found by Discord id - authoritative; at most the stored handle drifted.
    ById(MemberRecord),
    /// Found by handle after the id missed - a backfill, or a conflict if the record
    /// already carries a different id.
    ByHandle(MemberRecord),
    /// No record by id or handle.
    Unknown,
}

/// Decide the verification outcome from where the record was found.
///
/// An [`ById`](Located::ById) hit is authoritative and repairs only a drifted handle. An
/// [`ByHandle`](Located::ByHandle) hit either backfills a record that has no id yet or - if an
/// id is already present, which (since the id lookup missed) must be a *different* account's -
/// is a [`Conflict`](MatchOutcome::Conflict) the caller must not resolve automatically. The
/// conflict guard is the security boundary against handle recycling.
pub fn decide(
    found: Located,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> MatchOutcome {
    match found {
        Located::ById(record) => {
            let heal = heal_for(&record, target, target_handle);
            MatchOutcome::Matched { record, heal }
        }
        Located::ByHandle(record) => match record.discord_user_id {
            None => {
                let heal = heal_for(&record, target, target_handle);
                MatchOutcome::Matched { record, heal }
            }
            // An id is present and necessarily differs from `target` (an equal id would have
            // been found by the id lookup), so the handle points at another account's record.
            Some(_) => MatchOutcome::Conflict,
        },
        Located::Unknown => MatchOutcome::Miss,
    }
}

/// Read a member by id, then by handle on a miss, into a [`Located`]. The single definition of
/// the id-first / handle-fallback read the verify path and the bulk preview share; the id hit
/// wins, so the handle is read only when the id lookup misses.
pub async fn locate<S: MemberStore>(
    store: &S,
    id: DiscordUserId,
    handle: &DiscordHandle,
) -> Result<Located, S::Error> {
    if let Some(record) = store.by_discord_id(id).await? {
        return Ok(Located::ById(record));
    }
    Ok(match store.by_handle(handle).await? {
        Some(record) => Located::ByHandle(record),
        None => Located::Unknown,
    })
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
/// This is the email key's form of the id/handle path's impersonation guard: never bind an account to
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
        let out = decide(
            Located::ByHandle(record),
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, MatchOutcome::Conflict));
    }

    #[test]
    fn id_hit_is_authoritative_and_matches() {
        let out = decide(
            Located::ById(rec("st-1", Some(9), "rosy")),
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, MatchOutcome::Matched { .. }));
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
