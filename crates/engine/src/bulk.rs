//! The network-free half of bulk verify: enumerate the guild roster, filter by
//! scope, preview-partition it against the cache, and the staleness/liveness
//! decisions the resumable session needs. The writing apply loop and the wizard
//! collector live in the bot (a live interaction has no offline surface); these are
//! the pieces that can be unit-tested without a gateway.

use chrono::{DateTime, Duration, Utc};

use domain::Role;

use crate::backends::discord::{DiscordClient, DiscordError, DiscordRosterMember};
use crate::paging::drain_pages;
use crate::seam::NoProgress;
use crate::store::{BulkScope, MemberStore};
use crate::verify::{MatchOutcome, match_member};

/// Returns the staleness window (7 days). An in-progress session older than this
/// is treated as abandoned, evaluated lazily at command entry (there is no background
/// sweeper).
fn stale_after() -> Duration {
    Duration::days(7)
}

/// Whether a roster member is in the chosen sweep population.
pub fn in_scope(member: &DiscordRosterMember, scope: BulkScope) -> bool {
    match scope {
        BulkScope::WholeGuild => true,
        BulkScope::UnmanagedOnly => member.held.is_empty(),
    }
}

/// Drain the whole guild roster (via [`DiscordClient::members_page`]) and keep the
/// members in `scope`. Owns the page loop through `drain_pages` with a no-op progress
/// sink, exactly like `store::sweep_roster` does for Solidarity Tech. Bot accounts are
/// dropped before the scope filter - they are never members, so neither sweep population
/// should ever try to verify one (including the bot itself).
pub async fn enumerate<Dc: DiscordClient>(
    discord: &Dc,
    scope: BulkScope,
) -> Result<Vec<DiscordRosterMember>, DiscordError> {
    let all = drain_pages(&NoProgress, "discord roster", |cursor| async move {
        discord.members_page(cursor.as_deref()).await
    })
    .await?;
    Ok(all
        .into_iter()
        .filter(|m| !m.bot && in_scope(m, scope))
        .collect())
}

/// The read-only preview tally: matched counts broken down by role, plus the miss and
/// conflict counts. Computed from the captured population and the cache, no writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewTally {
    pub scanned: usize,
    /// Counts in `Role::ALL` order: (Member, n), (DuesExpired, n), (Unverified, n).
    pub matched: Vec<(Role, usize)>,
    /// Members the cache does not know - the wizard queue estimate.
    pub misses: usize,
    /// Handle hits bound to another account - left for a manual `/verify`.
    pub conflicts: usize,
}

/// Partition `members` by running the pure `match_member` decision over cache reads
/// (id first, then handle), tallying matched-by-role, misses, and conflicts. A fresh
/// decision at apply time is authoritative; this is the moderator's count-check.
pub async fn preview<S: MemberStore>(
    store: &S,
    members: &[DiscordRosterMember],
) -> Result<PreviewTally, S::Error> {
    let mut counts = [0usize; Role::ALL.len()];
    let mut misses = 0usize;
    let mut conflicts = 0usize;
    for m in members {
        let by_id = store.by_discord_id(m.id).await?;
        let by_handle = match &by_id {
            Some(_) => None,
            None => store.by_handle(&m.handle).await?,
        };
        match match_member(by_id, by_handle, m.id, &m.handle) {
            MatchOutcome::Matched { record, .. } => {
                let role = record.role();
                let idx = Role::ALL
                    .iter()
                    .position(|&r| r == role)
                    .expect("role in ALL");
                counts[idx] += 1;
            }
            MatchOutcome::Miss => misses += 1,
            MatchOutcome::Conflict => conflicts += 1,
        }
    }
    let matched = Role::ALL.into_iter().zip(counts).collect();
    Ok(PreviewTally {
        scanned: members.len(),
        matched,
        misses,
        conflicts,
    })
}

/// Whether an in-progress session has gone stale and should be treated as abandoned.
pub fn is_session_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - updated_at > stale_after()
}

/// Whether a queued miss is still the wizard's to resolve when it is reached: the
/// member is still present AND has not since been sorted into a verified status role
/// (Member or Dues Expired) by another path. A member holding only `Unverified`
/// (or nothing) is still pending; one who left the guild or got verified is skipped.
pub fn miss_still_pending(present: bool, held: &[Role]) -> bool {
    present
        && !held
            .iter()
            .any(|r| matches!(r, Role::Member | Role::DuesExpired))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{DiscordHandle, DiscordUserId};
    use chrono::TimeZone;

    fn roster(id: u64, handle: &str, held: Vec<Role>) -> DiscordRosterMember {
        DiscordRosterMember {
            id: DiscordUserId(id),
            handle: DiscordHandle(handle.into()),
            held,
            bot: false,
        }
    }

    #[test]
    fn unmanaged_scope_keeps_only_roleless_members() {
        assert!(in_scope(&roster(1, "a", vec![]), BulkScope::UnmanagedOnly));
        assert!(!in_scope(
            &roster(2, "b", vec![Role::Member]),
            BulkScope::UnmanagedOnly
        ));
        // Whole-guild keeps everyone.
        assert!(in_scope(
            &roster(2, "b", vec![Role::Member]),
            BulkScope::WholeGuild
        ));
    }

    #[test]
    fn staleness_window_is_seven_days() {
        let now = Utc.with_ymd_and_hms(2026, 6, 21, 0, 0, 0).unwrap();
        assert!(is_session_stale(now - Duration::days(8), now));
        assert!(!is_session_stale(now - Duration::days(6), now));
    }

    #[test]
    fn miss_pending_unless_verified_or_gone() {
        assert!(miss_still_pending(true, &[]));
        assert!(miss_still_pending(true, &[Role::Unverified]));
        assert!(!miss_still_pending(true, &[Role::Member]));
        assert!(!miss_still_pending(true, &[Role::DuesExpired]));
        assert!(!miss_still_pending(false, &[]));
    }
}
