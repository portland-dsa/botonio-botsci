//! The network-free planner for the scheduled scan: decide every enumerated guild member
//! against the cache, classify each role change as a promotion or a demotion, and return a
//! plan plus a mass-demote tripwire verdict. Read-only - the bot drives the paced apply
//! (via `verify::Member::verify`) and the abort alert. Mirrors `bulk::preview`, but returns
//! the actionable per-member list and the demotion classification, not just counts.

use domain::Role;

use crate::backends::discord::DiscordRosterMember;
use crate::bulk::already_in_role;
use crate::store::MemberStore;
use crate::util::{DiscordHandle, DiscordUserId};
use crate::verify::{MatchOutcome, decide, locate};

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
    pub verdict: ScanVerdict,
}

/// Plan a reconciliation pass over `members` (already enumerated and scope-filtered by the
/// caller, as `bulk::preview` expects). Decides each against the cache, collects the role
/// changes, counts demotions, and computes the tripwire verdict. No writes.
pub async fn plan<S: MemberStore>(
    store: &S,
    members: &[DiscordRosterMember],
    threshold: ScanThreshold,
) -> Result<ScanPlan, S::Error> {
    let mut changes = Vec::new();
    let mut demotions = 0usize;
    let mut misses = 0usize;
    let mut conflicts = 0usize;

    for m in members {
        let located = locate(store, m.id, &m.handle).await?;
        let target = match decide(located, m.id, &m.handle) {
            MatchOutcome::Matched { record, .. } => record.role(),
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

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::backends::discord::DiscordRosterMember;
    use crate::store::{InMemoryStore, Index, MemberRecord};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use domain::MigsStatus;

    fn roster(id: u64, handle: &str, held: Vec<Role>) -> DiscordRosterMember {
        DiscordRosterMember {
            id: DiscordUserId(id),
            handle: DiscordHandle(handle.into()),
            held,
            bot: false,
        }
    }

    fn record(st: &str, id: u64, handle: &str, standing: MigsStatus) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId(st.into()),
            discord_user_id: Some(DiscordUserId(id)),
            discord_handle: Some(DiscordHandle(handle.into())),
            email: Email(format!("{handle}@b.test")),
            full_name: None,
            standing: Some(standing),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    const THRESH: ScanThreshold = ScanThreshold {
        percent: 20,
        floor: 5,
    };

    #[tokio::test]
    async fn proceeds_with_a_mix_of_promotion_and_demotion() {
        // Sonic is in good standing but holds nothing -> promote to Member.
        // Tails lapsed but still holds Member -> demote.
        let store = InMemoryStore::new(Index::from_records(vec![
            record("st-1", 1, "sonic", MigsStatus::MemberInGoodStanding),
            record("st-2", 2, "tails", MigsStatus::Lapsed),
        ]));
        let members = vec![
            roster(1, "sonic", vec![]),
            roster(2, "tails", vec![Role::Member]),
        ];
        let p = plan(&store, &members, THRESH).await.unwrap();
        assert_eq!(p.verdict, ScanVerdict::Proceed);
        assert_eq!(p.demotions, 1);
        assert_eq!(p.changes.len(), 2);
    }

    #[tokio::test]
    async fn an_empty_cache_demotes_everyone_and_aborts() {
        let store = InMemoryStore::new(Index::default());
        let members: Vec<_> = (1..=10)
            .map(|i| roster(i, &format!("m{i}"), vec![Role::Member]))
            .collect();
        let p = plan(&store, &members, THRESH).await.unwrap();
        assert_eq!(p.demotions, 10);
        assert_eq!(p.misses, 10);
        assert_eq!(
            p.verdict,
            ScanVerdict::Abort {
                demotions: 10,
                scanned: 10
            }
        );
    }

    #[tokio::test]
    async fn demotions_below_the_floor_still_proceed_on_a_tiny_guild() {
        // 2 of 2 is 100% (over the percent), but below the floor of 5 -> proceed.
        let store = InMemoryStore::new(Index::default());
        let members = vec![
            roster(1, "a", vec![Role::Member]),
            roster(2, "b", vec![Role::DuesExpired]),
        ];
        let p = plan(&store, &members, THRESH).await.unwrap();
        assert_eq!(p.demotions, 2);
        assert_eq!(p.verdict, ScanVerdict::Proceed);
    }

    #[tokio::test]
    async fn a_handle_conflict_is_skipped_not_demoted() {
        // The cache binds handle "ghost" to id 99; the roster member with that handle is id
        // 1 -> conflict. Nothing is planned and it is not counted as a demotion.
        let store = InMemoryStore::new(Index::from_records(vec![record(
            "st-9",
            99,
            "ghost",
            MigsStatus::MemberInGoodStanding,
        )]));
        let members = vec![roster(1, "ghost", vec![Role::Member])];
        let p = plan(&store, &members, THRESH).await.unwrap();
        assert_eq!(p.conflicts, 1);
        assert_eq!(p.demotions, 0);
        assert!(p.changes.is_empty());
        assert_eq!(p.verdict, ScanVerdict::Proceed);
    }
}
