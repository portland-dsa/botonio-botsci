//! The network-free planner for the scheduled scan: decide every enumerated guild member
//! against the cache, classify each role change as a promotion or a demotion, and return a
//! plan plus a mass-demote tripwire verdict. Read-only - the bot drives the paced apply
//! (via `verify::Member::verify`) and the abort alert. Mirrors `bulk::preview`, but returns
//! the actionable per-member list and the demotion classification, not just counts.

use chrono::NaiveDate;
use domain::{DiscordGuildId, Role};

use crate::backends::discord::DiscordRosterMember;
use crate::bulk::already_in_role;
use crate::store::{GraceStore, MemberStore, OverrideLog};
use crate::util::{DiscordHandle, DiscordUserId};
use crate::verify::{MatchOutcome, decide, effective_role, locate};

/// Standing rank for demotion comparison: `Member` is highest, `Unverified` lowest.
fn rank(role: Role) -> u8 {
    match role {
        Role::Member => 2,
        Role::DuesExpired => 1,
        Role::Unverified => 0,
    }
}

/// Whether moving a member to `target` lowers their standing. True only when they
/// currently hold a managed *status* role (`Member` or `DuesExpired`) and `target` ranks
/// below the highest such role they hold. Gaining a role from nothing, or any promotion,
/// is never a demotion.
pub fn is_demotion(held: &[Role], target: Role) -> bool {
    let highest = held
        .iter()
        .copied()
        .filter(|r| matches!(r, Role::Member | Role::DuesExpired))
        .map(rank)
        .max();
    matches!(highest, Some(h) if rank(target) < h)
}

/// One member's intended role change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedChange {
    pub id: DiscordUserId,
    pub handle: DiscordHandle,
    pub target: Role,
    pub demotion: bool,
}

/// The tripwire bounds: a pass aborts when planned demotions reach BOTH the floor AND the
/// percentage of scanned members.
#[derive(Debug, Clone, Copy)]
pub struct ScanThreshold {
    pub percent: u8,
    pub floor: usize,
}

/// The tripwire verdict for a planned pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    Proceed,
    Abort { demotions: usize, scanned: usize },
}

/// A read-only reconciliation plan: the changes a pass would make, the partition counts,
/// and the tripwire verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPlan {
    pub scanned: usize,
    pub changes: Vec<PlannedChange>,
    pub demotions: usize,
    pub misses: usize,
    pub conflicts: usize,
    pub malformed: usize,
    /// Members held at their hand-approved standing this pass: a planned demotion was
    /// skipped because they carry an active manual-override stamp. Not a demotion, so it
    /// never feeds the tripwire.
    pub overridden: usize,
    pub verdict: ScanVerdict,
}

/// Plan a reconciliation pass over `members` (already enumerated and scope-filtered by the
/// caller, as `bulk::preview` expects). Decides each against the cache, collects the role
/// changes, counts demotions, and computes the tripwire verdict. No writes.
pub async fn plan<S, E>(
    store: &S,
    members: &[DiscordRosterMember],
    threshold: ScanThreshold,
    guild: DiscordGuildId,
    today: NaiveDate,
) -> Result<ScanPlan, E>
where
    S: MemberStore<Error = E> + GraceStore<Error = E> + OverrideLog<Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut changes = Vec::new();
    let mut demotions = 0usize;
    let mut misses = 0usize;
    let mut conflicts = 0usize;
    let mut malformed = 0usize;
    let mut overridden = 0usize;

    for m in members {
        let located = locate(store, m.id, &m.handle).await?;
        let target = match decide(located, m.id, &m.handle) {
            MatchOutcome::Matched { record, .. } => {
                let grace = store.active_grace(guild, m.id, today).await?;
                match effective_role(&record, grace) {
                    Ok(role) => role,
                    Err(()) => {
                        // No usable standing and no grace: never auto-demote a record we cannot
                        // decide, and keep it out of the demotion count so it cannot feed the
                        // mass-demote tripwire.
                        malformed += 1;
                        continue;
                    }
                }
            }
            MatchOutcome::Miss => {
                // The cache does not know them: the join check assigns Unverified.
                misses += 1;
                Role::Unverified
            }
            MatchOutcome::Conflict => {
                // Handle bound to another account: never written, left for a manual verify.
                conflicts += 1;
                continue;
            }
        };
        if already_in_role(&m.held, target) {
            continue; // already correct - verify would only heal, no role write
        }
        let demotion = is_demotion(&m.held, target);
        if demotion {
            // A manually-overridden member is held at their hand-approved standing: the
            // scheduled scan never demotes them, the same way an active grace override is
            // never demoted. Checked only on the demotion path, so an unaffected member
            // costs no extra read. (The hold is indefinite today; a DB-backed expiry can
            // retire a stale override later.)
            if store.get_override(m.id).await?.is_some() {
                overridden += 1;
                continue;
            }
            demotions += 1;
        }
        changes.push(PlannedChange {
            id: m.id,
            handle: m.handle.clone(),
            target,
            demotion,
        });
    }

    let scanned = members.len();
    // Abort only when demotions clear BOTH the absolute floor and the percentage of
    // scanned members - so normal churn on a small guild never trips, and a corrupt cache
    // on a large guild always does. `* 100 >= percent * scanned` avoids float math.
    let over_percent =
        demotions.saturating_mul(100) >= (threshold.percent as usize).saturating_mul(scanned);
    let verdict = if demotions >= threshold.floor && demotions > 0 && over_percent {
        ScanVerdict::Abort { demotions, scanned }
    } else {
        ScanVerdict::Proceed
    };

    Ok(ScanPlan {
        scanned,
        changes,
        demotions,
        misses,
        conflicts,
        malformed,
        overridden,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demotion_only_from_a_held_status_role_to_a_lower_one() {
        assert!(is_demotion(&[Role::Member], Role::DuesExpired));
        assert!(is_demotion(&[Role::Member], Role::Unverified));
        assert!(is_demotion(&[Role::DuesExpired], Role::Unverified));
        // Promotions and gains-from-nothing are not demotions.
        assert!(!is_demotion(&[Role::DuesExpired], Role::Member));
        assert!(!is_demotion(&[], Role::Unverified));
        assert!(!is_demotion(&[], Role::Member));
        assert!(!is_demotion(&[Role::Unverified], Role::Member));
        // Holding only Unverified is not a standing to be demoted from.
        assert!(!is_demotion(&[Role::Unverified], Role::Unverified));
    }
}
