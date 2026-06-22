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
use crate::verify::{MatchOutcome, decide, locate};

/// Returns the staleness window (7 days). An in-progress session older than this
/// is treated as abandoned, evaluated lazily at command entry (there is no background
/// sweeper).
fn stale_after() -> Duration {
    Duration::days(7)
}

/// Whether a roster member is in the chosen sweep population.
///
/// "Unmanaged" treats a bare `Unverified` the same as no role at all: the population is
/// everyone not yet sorted into a real membership status, i.e. holding neither `Member`
/// nor `DuesExpired`. That way a member left (or skipped) at `Unverified` is re-picked-up
/// by the next unmanaged sweep instead of being stranded.
pub fn in_scope(member: &DiscordRosterMember, scope: BulkScope) -> bool {
    match scope {
        BulkScope::WholeGuild => true,
        BulkScope::UnmanagedOnly => !member
            .held
            .iter()
            .any(|r| matches!(r, Role::Member | Role::DuesExpired)),
    }
}

/// Whether a member already holds exactly `role` and nothing else managed - so verifying
/// them would be a no-op role write. The whole-server resync uses this (in the preview and
/// the apply) to count and skip members whose role does not change; only the identity heal
/// still runs for them.
pub fn already_in_role(held: &[Role], role: Role) -> bool {
    held.len() == 1 && held[0] == role
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

/// The read-only preview tally: the role changes a sweep would make, plus the already-
/// correct, miss, and conflict counts. Computed from the captured population and the
/// cache, no writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewTally {
    pub scanned: usize,
    /// Counts in `Role::ALL` order: matched members whose role WILL CHANGE to each role.
    /// A member already holding exactly their earned role is not counted here (see
    /// [`unchanged`](Self::unchanged)) so the confirmation lists only real writes.
    pub matched: Vec<(Role, usize)>,
    /// Matched members already holding exactly their earned role: no write, only a
    /// best-effort identity heal at apply time.
    pub unchanged: usize,
    /// Members the cache does not know - the wizard queue estimate.
    pub misses: usize,
    /// Handle hits bound to another account - left for a manual `/verify`.
    pub conflicts: usize,
}

/// Partition `members` by running [`locate`] then [`decide`] over cache reads
/// (id first, then handle), tallying role changes, already-correct members, misses, and
/// conflicts. A matched member already holding exactly their earned role counts as
/// `unchanged`, not a change. A fresh decision at apply time is authoritative; this is the
/// moderator's count-check.
pub async fn preview<S: MemberStore>(
    store: &S,
    members: &[DiscordRosterMember],
) -> Result<PreviewTally, S::Error> {
    let mut counts = [0usize; Role::ALL.len()];
    let mut unchanged = 0usize;
    let mut misses = 0usize;
    let mut conflicts = 0usize;
    for m in members {
        let located = locate(store, m.id, &m.handle).await?;
        match decide(located, m.id, &m.handle) {
            MatchOutcome::Matched { record, .. } => {
                let role = record.role();
                if already_in_role(&m.held, role) {
                    unchanged += 1;
                } else {
                    let idx = Role::ALL
                        .iter()
                        .position(|&r| r == role)
                        .expect("role in ALL");
                    counts[idx] += 1;
                }
            }
            MatchOutcome::Miss => misses += 1,
            MatchOutcome::Conflict => conflicts += 1,
        }
    }
    let matched = Role::ALL.into_iter().zip(counts).collect();
    Ok(PreviewTally {
        scanned: members.len(),
        matched,
        unchanged,
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

    #[test]
    fn already_in_role_only_when_holding_exactly_that_role() {
        assert!(already_in_role(&[Role::Member], Role::Member));
        // Holding nothing, a different role, or extra managed roles all count as a change.
        assert!(!already_in_role(&[], Role::Member));
        assert!(!already_in_role(&[Role::DuesExpired], Role::Member));
        assert!(!already_in_role(
            &[Role::Member, Role::Unverified],
            Role::Member
        ));
    }
}
