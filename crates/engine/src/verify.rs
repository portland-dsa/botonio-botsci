//! The moderator verify-and-assign use case: match a Discord member to their
//! Solidarity Tech record, repair the stored identity link, and assign the role their
//! standing earns.
//!
//! [`match_member`] is the pure decision - id first (the immutable key), then handle
//! as a repair fallback, with a guard that never re-links a record already bound to a
//! different account. [`verify`] is the orchestrator that executes a decision against
//! the backends, the cache, and the audit log.

use crate::store::MemberRecord;
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
