//! The pure milestone selection and the roster planner for dues reminders.

use chrono::NaiveDate;

use crate::backends::solidarity_tech::DuesStatus;
use crate::reminders::{DueReminder, ExpiryStatus, Milestone, ReminderPlan};
use crate::store::{GraceStore, MemberStore, ReminderStore};
use domain::DiscordGuildId;

/// The milestone a member is due right now, or `None` if none is.
///
/// `status` is built from `xdate - today`. A lapsed member (`ExpiryStatus::Lapsed`)
/// is always a candidate for [`Lapse`](Milestone::Lapse). A member inside the
/// 14-day window is a candidate for [`Renewal`](Milestone::Renewal). A member with
/// current dues has no candidate. The candidate is returned only when it is strictly
/// more urgent than `last_sent` (so each fires once and a lapse without a prior renewal
/// collapses straight to `Lapse`).
pub fn select_milestone(status: ExpiryStatus, last_sent: Option<Milestone>) -> Option<Milestone> {
    let candidate = match status {
        ExpiryStatus::Current => return None,
        ExpiryStatus::Expiring { .. } => Milestone::Renewal,
        ExpiryStatus::Lapsed => Milestone::Lapse,
    };
    match last_sent {
        Some(prev) if candidate <= prev => None,
        _ => Some(candidate),
    }
}

/// Plan the dues reminders due across the cached roster on `today`. Reads each member's
/// record, grace, opt-out, and cycle state; applies the gates (grace > opt-out >
/// auto-renew, with `Lapse` bypassing opt-out and auto-renew); and returns the due list.
/// No I/O beyond the store reads; no writes.
pub async fn plan<S, E>(
    store: &S,
    guild: DiscordGuildId,
    today: NaiveDate,
) -> Result<ReminderPlan, E>
where
    S: MemberStore<Error = E> + ReminderStore<Error = E> + GraceStore<Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut due = Vec::new();
    for record in store.all_records().await? {
        let Some(id) = record.discord_user_id else {
            continue; // unlinked: nothing to thread
        };
        let Some(xdate) = record.expires else {
            continue; // no lapse date: nothing to schedule
        };
        let status = ExpiryStatus::from(xdate - today);

        // Cycle state, treating a stale cycle as reset.
        let state = store.reminder_state(guild, id).await?;
        let last_sent = match state {
            Some(s) if s.cycle_xdate == xdate => s.last_sent,
            _ => None,
        };

        let Some(candidate) = select_milestone(status, last_sent) else {
            continue;
        };

        // Gate 1: grace suppresses everything, including Lapse.
        if store.active_grace(guild, id, today).await? {
            continue;
        }
        // The remaining gates apply only to the pre-lapse nudge; Lapse bypasses them.
        if candidate != Milestone::Lapse {
            if store.is_opted_out(guild, id).await? {
                continue;
            }
            // Auto-renew: either cadence currently Active.
            let auto = matches!(record.monthly_dues, Some(DuesStatus::Active))
                || matches!(record.yearly_dues, Some(DuesStatus::Active));
            if auto {
                continue;
            }
        }

        due.push(DueReminder {
            id,
            milestone: candidate,
            membership_type: record.membership_type,
            xdate,
        });
    }
    Ok(ReminderPlan { due })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn expiry_status_boundaries() {
        assert!(matches!(
            ExpiryStatus::from(TimeDelta::days(15)),
            ExpiryStatus::Current
        ));
        assert!(matches!(
            ExpiryStatus::from(TimeDelta::days(14)),
            ExpiryStatus::Expiring { .. }
        ));
        assert!(matches!(
            ExpiryStatus::from(TimeDelta::days(0)),
            ExpiryStatus::Expiring { .. }
        ));
        assert!(matches!(
            ExpiryStatus::from(TimeDelta::days(-1)),
            ExpiryStatus::Lapsed
        ));
    }

    #[test]
    fn select_fires_once_and_collapses_offline_lapse() {
        let exp = ExpiryStatus::Expiring {
            time_left: chrono::TimeDelta::days(10),
        };
        assert_eq!(select_milestone(exp, None), Some(Milestone::Renewal));
        assert_eq!(select_milestone(exp, Some(Milestone::Renewal)), None);
        // Offline through the whole window -> straight to Lapse, never the stale Renewal.
        assert_eq!(
            select_milestone(ExpiryStatus::Lapsed, None),
            Some(Milestone::Lapse)
        );
        assert_eq!(select_milestone(ExpiryStatus::Current, None), None);
    }
}
