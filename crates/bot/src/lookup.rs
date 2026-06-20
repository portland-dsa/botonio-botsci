//! The moderator-lookup decision core and its in-memory rate limiter.
//!
//! This module is deliberately self-contained: it depends only on `engine`,
//! `domain`, and `std`, with no `crate::` paths and no poise `Context`. That lets
//! the Cucumber suite pull it straight into a test binary with
//! `#[path = "../src/lookup.rs"]` (the bot is a binary crate with no library
//! target), the same way the guild-guard suite reaches its module.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::util::DiscordUserId;

/// The length of one rate-limit window.
const WINDOW: Duration = Duration::from_secs(60);

/// A per-moderator fixed-window rate limiter held in process memory.
///
/// Its job is to make bulk PII extraction by a compromised moderator account slow
/// and noisy - every allowed lookup is audited regardless - not to inconvenience a
/// moderator clearing a handful of requests. Keyed by moderator id; the map is
/// bounded by the number of moderators, so it needs no eviction. State resets on
/// restart, which is acceptable: the durable record of every lookup is the audit
/// log, and an attacker cannot force cheap restarts.
pub struct RateLimiter {
    max_per_min: u32,
    windows: Mutex<HashMap<u64, Window>>,
}

/// One moderator's current window: how many lookups they have spent since it opened.
struct Window {
    count: u32,
    opened_at: Instant,
}

impl RateLimiter {
    /// A limiter allowing `max_per_min` lookups per moderator per minute.
    pub fn new(max_per_min: u32) -> Self {
        Self {
            max_per_min,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Try to spend one token for `who` at `now`. Returns `true` (and records the
    /// spend) when under the ceiling; `false` when the moderator has already used
    /// their allowance for the current window. `now` is a parameter so the policy is
    /// testable without sleeping; callers pass [`Instant::now`].
    pub fn check(&self, who: DiscordUserId, now: Instant) -> bool {
        let mut windows = self.windows.lock().expect("rate-limiter lock poisoned");
        let window = windows.entry(who.0).or_insert(Window {
            count: 0,
            opened_at: now,
        });
        // Roll the window over once a full minute has elapsed since it opened.
        if now.duration_since(window.opened_at) >= WINDOW {
            window.count = 0;
            window.opened_at = now;
        }
        if window.count >= self.max_per_min {
            return false;
        }
        window.count += 1;
        true
    }
}

#[cfg(test)]
mod rate_limiter_tests {
    use super::*;

    #[test]
    fn allows_up_to_the_ceiling_then_refuses() {
        let limiter = RateLimiter::new(3);
        let now = Instant::now();
        let who = DiscordUserId(7);
        assert!(limiter.check(who, now));
        assert!(limiter.check(who, now));
        assert!(limiter.check(who, now));
        // Fourth in the same window is refused.
        assert!(!limiter.check(who, now));
    }

    #[test]
    fn window_resets_after_a_minute() {
        let limiter = RateLimiter::new(1);
        let t0 = Instant::now();
        let who = DiscordUserId(7);
        assert!(limiter.check(who, t0));
        assert!(!limiter.check(who, t0)); // spent
        // A minute later the allowance is fresh.
        assert!(limiter.check(who, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn limits_are_per_moderator() {
        let limiter = RateLimiter::new(1);
        let now = Instant::now();
        assert!(limiter.check(DiscordUserId(1), now));
        // A different moderator has their own allowance.
        assert!(limiter.check(DiscordUserId(2), now));
    }
}
