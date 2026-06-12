//! Shared HTTP plumbing for the REST backends, so timeout, user-agent, and
//! rate-limit-retry policy live in exactly one place.

use std::time::Duration;

/// The user-agent every HTTP backend sends.
const USER_AGENT: &str = "discord-bulk-update/0.1.0";

/// Build a [`reqwest::Client`] carrying the crate's shared timeout policy and
/// the given `user_agent`.
///
/// Centralizing construction keeps the HTTP backend on a 30-second response and
/// 10-second connect timeout, so one hung remote can never stall the whole run.
/// Solidarity Tech builds its client through this helper; Discord is the
/// exception, talking through `serenity::http::Http` and never reaching this
/// builder.
///
/// A returned [`reqwest::Error`] means the TLS backend failed to initialize,
/// which in practice only happens at process startup.
pub fn build_client(user_agent: &str) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
}

/// The shared [`reqwest::Client`] every HTTP backend uses: the common timeout
/// policy plus the shared `USER_AGENT`. Both each backend's `from_env` and its
/// `with_base_url` test constructor go through here, so they cannot drift.
pub fn default_client() -> reqwest::Result<reqwest::Client> {
    build_client(USER_AGENT)
}

/// Per-backend knobs for [`send_with_retry`]: the leading pace, the 429 retry
/// cap, the fallback wait when a 429 omits a usable `Retry-After`, and a label
/// for the retry log line.
pub struct RetryPolicy {
    /// Leading pause before every request, holding the caller under its API's
    /// rate cap with a little headroom.
    pub pacing: Duration,
    /// How many times an HTTP 429 is re-attempted before giving up.
    pub max_retries: u32,
    /// Wait used when a 429 carries no parseable `Retry-After` header.
    pub default_retry_after: Duration,
    /// Backend name, used only in the 429 warn log.
    pub label: &'static str,
}

/// Sends `req` under `policy`, the one rate-limit-retry path the three REST
/// backends share.
///
/// The pace sleep is up front, not only between retries, so a tight loop
/// (paginate, then follow-up calls) cannot spend its whole budget in the first
/// second. On HTTP 429 it retries up to [`max_retries`](RetryPolicy::max_retries)
/// times, honoring `Retry-After` and falling back to
/// [`default_retry_after`](RetryPolicy::default_retry_after); any other status is
/// returned as-is for the caller to inspect. The caller applies auth to `req`
/// before calling; each attempt re-clones it, sound because every request here
/// carries a fully in-memory body, never a stream. When the retries are
/// exhausted the error is built by `exhausted` - each backend's own
/// `RateLimitExhausted` variant.
///
/// Design note: a maintained retry layer (`reqwest-retry` + `retry-policies`)
/// was evaluated to replace this loop and declined. That stack waits only
/// *between* retries, so it gives no leading per-request pace - the floor that
/// holds a pagination sweep under these low API caps - and by default it retries
/// 5xx and transport errors rather than gating on 429 alone. Reproducing both
/// would add three dependencies plus two custom impls for no net simplification.
/// Revisit only if the leading pace stops being a requirement.
pub async fn send_with_retry<E>(
    req: reqwest::RequestBuilder,
    policy: &RetryPolicy,
    exhausted: impl Fn(u32) -> E,
) -> Result<reqwest::Response, E>
where
    E: From<reqwest::Error>,
{
    tokio::time::sleep(policy.pacing).await;
    for attempt in 0..policy.max_retries {
        let cloned = req
            .try_clone()
            .unwrap_or_else(|| panic!("{} requests carry no streaming body", policy.label));
        let r = cloned.send().await?;
        if r.status().as_u16() != 429 {
            return Ok(r);
        }
        let wait = r
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(policy.default_retry_after);
        // Nothing to gain by waiting after the final attempt: we are about to
        // give up regardless, so don't burn one more `default_retry_after` (~30s
        // per backend) on a hard 429 (revoked token / blown quota).
        if attempt + 1 == policy.max_retries {
            break;
        }
        tracing::warn!(
            backend = policy.label,
            attempt,
            wait_secs = wait.as_secs(),
            "429; sleeping"
        );
        tokio::time::sleep(wait).await;
    }
    Err(exhausted(policy.max_retries))
}
