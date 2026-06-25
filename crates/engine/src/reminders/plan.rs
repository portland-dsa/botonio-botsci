//! The pure milestone selection and the roster planner for dues reminders.

use chrono::NaiveDate;

use crate::backends::solidarity_tech::DuesStatus;
use crate::reminders::{DueReminder, Milestone, ReminderPlan};
use crate::store::{GraceStore, MemberStore, ReminderStore};
use domain::DiscordGuildId;

/// The milestone a member is due right now, or `None` if none is.
///
/// `days_until` is `xdate - today`. A lapsed member (`days_until < 0`) is always a
/// candidate for [`Expired`](Milestone::Expired). Otherwise the candidate is, on a
/// **timely** sweep, the most urgent pre-lapse milestone whose day has arrived
/// (`lead_days >= days_until`); on a **delayed** sweep (the bot missed a crossing), the
/// milestone *nearest* `days_until`, ties broken toward the more urgent mark. The
/// candidate is returned only when it is strictly more urgent than `last_sent` (so each
/// fires once and a delayed sweep collapses the backlog to one message).
pub fn select_milestone(
    days_until: i64,
    last_sent: Option<Milestone>,
    timely: bool,
) -> Option<Milestone> {
    let candidate = if days_until < 0 {
        Milestone::Expired
    } else if timely {
        // Most urgent pre-lapse mark whose day has arrived (lead >= days_until).
        // PRE_LAPSE is longest-lead first, so scan from the most urgent (last) backward.
        Milestone::PRE_LAPSE
            .into_iter()
            .rev()
            .find(|m| m.lead_days().is_some_and(|lead| lead >= days_until))?
    } else {
        // Only a member already inside the outermost reminder window (days_until <= the
        // widest lead) has reached a mark a catch-up sweep should round to; one further
        // out has entered no window, so rounding them up would fire a premature nudge.
        // This mirrors the timely arm, which returns None once days_until exceeds every
        // lead. Within the window: nearest mark to days_until, ties toward the more urgent
        // (smaller lead) - iterate most-urgent-first so an exact tie keeps the more urgent.
        let widest = Milestone::PRE_LAPSE
            .into_iter()
            .filter_map(|m| m.lead_days())
            .max()
            .expect("PRE_LAPSE always has lead-bearing marks");
        if days_until > widest {
            return None;
        }
        Milestone::PRE_LAPSE
            .into_iter()
            .rev()
            .min_by_key(|m| (m.lead_days().unwrap() - days_until).abs())?
    };
    // Fire only if strictly more urgent than what we have already sent this cycle.
    match last_sent {
        Some(prev) if candidate <= prev => None,
        _ => Some(candidate),
    }
}

/// Plan the dues reminders due across the cached roster on `today`. Reads each member's
/// record, grace, opt-out, and cycle state; applies the gates (grace > opt-out > snooze >
/// auto-renew, with `Expired` bypassing all but grace); and returns the due list. No I/O
/// beyond the store reads; no writes.
pub async fn plan<S, E>(
    store: &S,
    guild: DiscordGuildId,
    today: NaiveDate,
    timely: bool,
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
        let days_until = (xdate - today).num_days();

        // Cycle state, treating a stale cycle as reset.
        let state = store.reminder_state(guild, id).await?;
        let (last_sent, snoozed) = match state {
            Some(s) if s.cycle_xdate == xdate => (s.last_sent, s.snoozed),
            _ => (None, false),
        };

        let Some(candidate) = select_milestone(days_until, last_sent, timely) else {
            continue;
        };

        // Gate 1: grace suppresses everything, including Expired.
        if store.active_grace(guild, id, today).await? {
            continue;
        }
        // The remaining gates apply only to the pre-lapse nudges; Expired bypasses them.
        if candidate != Milestone::Expired {
            if store.is_opted_out(guild, id).await? {
                continue;
            }
            if snoozed {
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
            days_until,
        });
    }
    Ok(ReminderPlan { due })
}
