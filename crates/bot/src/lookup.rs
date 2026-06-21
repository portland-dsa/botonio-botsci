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

use engine::audit::AuditLog;
use engine::card::{self, CardView};
use engine::store::{MemberRecord, MemberStore, OverrideLog, OverrideRecord};

/// What a lookup resolved to. The poise adapters render each variant; the Cucumber
/// suite asserts on them directly. `Debug` so the Cucumber `World` (which must be
/// `Debug`) can hold one.
#[derive(Debug)]
pub enum LookupOutcome {
    /// The invoker viewing their own card - `None` when they have no record. Never
    /// gated, rate-limited, or audited.
    SelfCard(Option<MemberRecord>),
    /// The invoker viewing their own override card (manually verified, no record).
    SelfOverride(OverrideRecord),
    /// A moderator's successful view of another member's card.
    Card(MemberRecord),
    /// A moderator's successful view of a manually-verified member's override card.
    OverrideCard(OverrideRecord),
    /// A privileged lookup that found no record for the target.
    NotFound,
    /// A non-moderator tried to view someone other than themselves.
    NotModerator,
    /// The moderator exceeded their per-minute lookup allowance.
    RateLimited,
    /// The store (or the audit write) failed; the caller shows a generic reply.
    StoreError(String),
}

/// Resolve a lookup, applying the privilege rule for viewing *another* member.
///
/// Viewing yourself is always allowed and never recorded. Viewing anyone else
/// requires the moderator role, is rate-limited, and - whether or not a record is
/// found - is written to the audit log. The audit write is fail-closed: if it
/// cannot be recorded, no card is revealed (a [`LookupOutcome::StoreError`]), so a
/// successful reveal always has a matching audit row.
pub async fn lookup<S, O, A>(
    store: &S,
    overrides: &O,
    audit: &A,
    limiter: &RateLimiter,
    invoker: DiscordUserId,
    target: DiscordUserId,
    is_moderator: bool,
) -> LookupOutcome
where
    S: MemberStore,
    O: OverrideLog,
    A: AuditLog,
{
    // Self lookups bypass the gate, the limiter, and the audit log entirely.
    if target == invoker {
        return match card::resolve_view(store, overrides, &card::PresentMember { id: invoker })
            .await
        {
            Ok(CardView::Member(rec)) => LookupOutcome::SelfCard(Some(rec)),
            Ok(CardView::Override(stamp)) => LookupOutcome::SelfOverride(stamp),
            Ok(CardView::Unknown) => LookupOutcome::SelfCard(None),
            Err(e) => LookupOutcome::StoreError(e.to_string()),
        };
    }
    if !is_moderator {
        return LookupOutcome::NotModerator;
    }
    if !limiter.check(invoker, Instant::now()) {
        return LookupOutcome::RateLimited;
    }

    // A moderator viewing another member: resolve, then record before revealing.
    let view = match card::resolve_view(store, overrides, &card::PresentMember { id: target }).await
    {
        Ok(v) => v,
        Err(e) => return LookupOutcome::StoreError(e.to_string()),
    };
    // Map the view to its audit label and the outcome to reveal in one place, so the
    // recorded label can never disagree with the card that ends up shown.
    let (reveal, outcome) = match view {
        CardView::Member(rec) => (LookupOutcome::Card(rec), "found"),
        CardView::Override(stamp) => (LookupOutcome::OverrideCard(stamp), "override"),
        CardView::Unknown => (LookupOutcome::NotFound, "not_found"),
    };
    if let Err(e) = audit
        .record(
            invoker,
            target,
            "card_lookup",
            serde_json::json!({ "outcome": outcome }),
        )
        .await
    {
        // Fail closed: never reveal a card we could not audit.
        tracing::error!(error = %e, "audit write failed; refusing the lookup");
        return LookupOutcome::StoreError(e.to_string());
    }
    reveal
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

#[cfg(test)]
mod core_tests {
    use super::*;
    use std::convert::Infallible;

    use engine::store::{InMemoryStore, Index};

    /// An audit sink that always fails, to exercise the fail-closed path.
    struct FailingAudit;

    #[derive(Debug, thiserror::Error)]
    #[error("audit unavailable")]
    struct AuditDown;

    #[async_trait::async_trait]
    impl AuditLog for FailingAudit {
        type Error = AuditDown;
        async fn record(
            &self,
            _: DiscordUserId,
            _: DiscordUserId,
            _: &str,
            _: serde_json::Value,
        ) -> Result<(), AuditDown> {
            Err(AuditDown)
        }
    }

    /// A never-failing sink for the happy-path assertion.
    struct NoopAudit;

    #[async_trait::async_trait]
    impl AuditLog for NoopAudit {
        type Error = Infallible;
        async fn record(
            &self,
            _: DiscordUserId,
            _: DiscordUserId,
            _: &str,
            _: serde_json::Value,
        ) -> Result<(), Infallible> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn audit_failure_refuses_the_reveal() {
        let store = InMemoryStore::new(Index::default());
        let limiter = RateLimiter::new(10);
        let outcome = lookup(
            &store,
            &store,
            &FailingAudit,
            &limiter,
            DiscordUserId(1),
            DiscordUserId(2),
            true,
        )
        .await;
        assert!(matches!(outcome, LookupOutcome::StoreError(_)));
    }

    #[tokio::test]
    async fn non_moderator_is_refused_without_audit() {
        let store = InMemoryStore::new(Index::default());
        let limiter = RateLimiter::new(10);
        let outcome = lookup(
            &store,
            &store,
            &NoopAudit,
            &limiter,
            DiscordUserId(1),
            DiscordUserId(2),
            false,
        )
        .await;
        assert!(matches!(outcome, LookupOutcome::NotModerator));
    }
}
