//! On-demand and scheduled member-cache warming.
//!
//! [`refresh_once`] is the single sweep-and-replace step the background loop in `main`
//! and the `/refresh-cache` command both run: pull the Solidarity Tech roster and write
//! it through to the cache, keeping the last good roster on an empty or failed sweep.
//! [`Cooldown`] is the in-memory throttle that keeps `/refresh-cache` from hammering the
//! API - one refresh per window, shared across moderators. Both are deliberately
//! self-contained (no poise `Context`) so the cooldown can be pulled into a Cucumber
//! binary the way `lookup.rs` is.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::backends::solidarity_tech::SolidarityTechHttp;
use engine::store::{RosterWrite, sweep_roster};
use persistence::PgStore;

/// How long a manual `/refresh-cache` is locked out after a refresh runs. Long enough to
/// blunt repeated hammering, short enough to stay useful for back-to-back testing.
pub const REFRESH_COOLDOWN: Duration = Duration::from_secs(180);

/// What a single refresh did, for the caller to report or log.
#[derive(Debug, PartialEq, Eq)]
pub enum RefreshReport {
    /// The sweep succeeded and the cache now holds this many members.
    Loaded(usize),
    /// The sweep returned no members; the previous roster was kept (almost always an
    /// upstream blip rather than a real membership of zero).
    Empty,
    /// The sweep or the cache write failed; the previous roster was kept.
    Failed,
}

/// Sweep the Solidarity Tech list and replace the cached roster, keeping the last good
/// roster on an empty or failed sweep. Logs the outcome itself, so both the background
/// loop and the command get the same diagnostics; the returned [`RefreshReport`] is for
/// the caller's own reply or tally.
pub async fn refresh_once(
    store: &PgStore,
    solidarity_tech: &SolidarityTechHttp,
    list_id: &str,
) -> RefreshReport {
    match sweep_roster(solidarity_tech, list_id).await {
        // An empty sweep is almost always an upstream glitch, not a real membership of
        // zero - keep the last good roster rather than wiping it.
        Ok(records) if records.is_empty() => {
            tracing::warn!("solidarity tech sweep returned zero members; keeping last good roster");
            RefreshReport::Empty
        }
        Ok(records) => {
            let count = records.len();
            match store.replace_roster(records).await {
                Ok(()) => {
                    tracing::info!(members = count, "member roster refreshed");
                    RefreshReport::Loaded(count)
                }
                // Keep the last good roster on a write failure - do not clear it.
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "member roster refresh failed to write; keeping last good roster"
                    );
                    RefreshReport::Failed
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "member roster refresh failed; keeping last good roster");
            RefreshReport::Failed
        }
    }
}

/// A single-slot, process-wide refresh throttle.
///
/// One window shared by everyone (not per-moderator like the lookup limiter): the goal is
/// to cap how often the Solidarity Tech list is swept, regardless of who asks. The spend
/// is recorded the moment a check passes, before the (slow) sweep runs, so two
/// near-simultaneous invocations cannot both sweep and a failed sweep still counts against
/// the window - it hit the API, which is what is being throttled. State is in-memory and
/// resets on restart, which is fine: the background loop refreshes regardless.
pub struct Cooldown {
    window: Duration,
    last: Mutex<Option<Instant>>,
}

impl Cooldown {
    /// A cooldown of length `window`.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last: Mutex::new(None),
        }
    }

    /// Try to claim a refresh at `now`. Returns `Ok(())` (and records the spend) when the
    /// window is open; `Err(remaining)` with the time left when it is still closed. `now`
    /// is a parameter so the policy is testable without sleeping; callers pass
    /// [`Instant::now`].
    pub fn check(&self, now: Instant) -> Result<(), Duration> {
        let mut last = self.last.lock().expect("cooldown lock poisoned");
        if let Some(prev) = *last {
            let elapsed = now.saturating_duration_since(prev);
            if elapsed < self.window {
                return Err(self.window - elapsed);
            }
        }
        *last = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod cooldown_tests {
    use super::*;

    #[test]
    fn first_check_is_allowed() {
        let cooldown = Cooldown::new(Duration::from_secs(180));
        assert!(cooldown.check(Instant::now()).is_ok());
    }

    #[test]
    fn second_check_in_the_window_is_refused_with_the_time_left() {
        let cooldown = Cooldown::new(Duration::from_secs(180));
        let t0 = Instant::now();
        cooldown.check(t0).unwrap();
        // 60s later, 120s remain.
        let remaining = cooldown
            .check(t0 + Duration::from_secs(60))
            .expect_err("still in the window");
        assert_eq!(remaining, Duration::from_secs(120));
    }

    #[test]
    fn check_after_the_window_is_allowed_again() {
        let cooldown = Cooldown::new(Duration::from_secs(180));
        let t0 = Instant::now();
        cooldown.check(t0).unwrap();
        assert!(cooldown.check(t0 + Duration::from_secs(181)).is_ok());
    }
}
