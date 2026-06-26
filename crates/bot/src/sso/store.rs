//! The in-memory, bounded pending-auth store keyed by `state`. No database table:
//! half-finished logins are worth nothing and best not kept at rest. Bounded so a
//! flood cannot exhaust memory, and rejecting-when-full (not evicting the oldest)
//! so an attacker cannot flush a legitimate person's pending login.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::backends::discord::{PkceVerifier, State};

struct PendingAuth {
    verifier: PkceVerifier,
    created: Instant,
}

/// Holds one entry per in-flight sign-in until its callback or its TTL.
///
/// Every operation is atomic under a single [`Mutex`]; the store is safe to share
/// across threads via [`Arc`](std::sync::Arc). Two hard-coded policies keep it safe:
///
/// - **Reject-when-full** (`begin` returns [`PendingFull`] rather than evicting the
///   oldest entry): a flood of fresh flows cannot displace a legitimate pending login.
/// - **Single-use `take`**: the verifier is removed on the first successful read.
///   A replayed callback - whether from a bug or an attacker - returns `None`.
///
/// # Example
///
/// ```rust,ignore
/// let store = PendingAuthStore::new(64, Duration::from_secs(300));
/// store.begin(state, verifier)?;
/// // ... redirect user to Discord, receive callback ...
/// let verifier = store.take(&callback_state).ok_or(SsoError::BadState)?;
/// ```
pub struct PendingAuthStore {
    inner: Mutex<HashMap<String, PendingAuth>>,
    cap: usize,
    ttl: Duration,
}

/// Returned by [`PendingAuthStore::begin`] when the store is at capacity.
///
/// The caller should respond with a uniform denial - not a "try again later" that
/// would tip off a flooder about how many slots remain.
#[derive(Debug)]
pub struct PendingFull;

impl PendingAuthStore {
    /// Build a store with a maximum of `cap` concurrent pending logins, each valid
    /// for `ttl` before [`take`](Self::take) treats them as expired.
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            ttl,
        }
    }

    /// Record a pending sign-in. Drops expired entries first, then rejects if still
    /// at `cap` - never evicts a live entry to make room.
    ///
    /// Returns [`PendingFull`] when every slot is occupied by a non-expired entry
    /// after the sweep. The caller must not reveal which path was taken.
    pub fn begin(&self, state: State, verifier: PkceVerifier) -> Result<(), PendingFull> {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, e| now.duration_since(e.created) < self.ttl);
        if map.len() >= self.cap {
            return Err(PendingFull);
        }
        map.insert(
            state.0,
            PendingAuth {
                verifier,
                created: now,
            },
        );
        Ok(())
    }

    /// Atomically remove and return the verifier for `state`, if present and unexpired.
    ///
    /// This is the **only** read. Consuming on first access means a replayed callback
    /// (even one that races with the real one) gets `None`. An unknown, expired, or
    /// already-consumed `state` all return `None`, with no timing difference surfaced
    /// to the caller.
    pub fn take(&self, state: &str) -> Option<PkceVerifier> {
        let mut map = self.inner.lock().unwrap();
        let entry = map.remove(state)?;
        if Instant::now().duration_since(entry.created) >= self.ttl {
            return None;
        }
        Some(entry.verifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(n: u32) -> (State, PkceVerifier) {
        (State(format!("s{n}")), PkceVerifier::new(format!("v{n}")))
    }

    #[test]
    fn take_is_single_use() {
        let store = PendingAuthStore::new(8, Duration::from_secs(60));
        let (s, v) = pair(1);
        store.begin(s, v).unwrap();
        assert!(store.take("s1").is_some());
        assert!(store.take("s1").is_none()); // consumed
    }

    #[test]
    fn rejects_when_full_without_evicting() {
        let store = PendingAuthStore::new(1, Duration::from_secs(60));
        let (s1, v1) = pair(1);
        store.begin(s1, v1).unwrap();
        let (s2, v2) = pair(2);
        assert!(store.begin(s2, v2).is_err()); // full -> reject
        assert!(store.take("s1").is_some()); // the first entry survived
    }

    #[test]
    fn expired_entry_is_not_returned() {
        let store = PendingAuthStore::new(8, Duration::from_millis(1));
        let (s, v) = pair(1);
        store.begin(s, v).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.take("s1").is_none());
    }

    #[test]
    fn unknown_state_is_none() {
        let store = PendingAuthStore::new(8, Duration::from_secs(60));
        assert!(store.take("nope").is_none());
    }
}
