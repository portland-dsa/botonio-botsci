//! The localhost-only HTTP surface: two POST routes over a unix-domain socket,
//! gated by a constant-time bearer. No TCP, no public port. The OS (socket owner +
//! mode) is the first gate on who may connect; the bearer is the second.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State as AxState;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use arc_swap::ArcSwap;
use engine::backends::discord::DiscordOAuthHttp;
use engine::store::GuildConfig;
use engine::util::DiscordUserId;
use persistence::Auditor;

use super::assertion::Signer;
use super::flow::{self, SsoDenial, SsoOutcome};
use super::store::PendingAuthStore;

/// Everything a request handler needs, all shared by [`Arc`].
///
/// `SsoState` is cheaply cloned: every field is already behind an `Arc`, so
/// a clone only bumps reference counts. Axum requires the state type to be
/// `Clone` to hand it into each handler task.
#[derive(Clone)]
pub struct SsoState {
    pub oauth: Arc<DiscordOAuthHttp>,
    /// The gateway's shared HTTP and the guild id, kept so the complete handler can rebuild
    /// the role-write client from the *live* config on each request - the role map must never
    /// be frozen at boot, so a /setup role change takes effect without a restart.
    pub http: Arc<serenity::http::Http>,
    pub guild_id: u64,
    pub auditor: Arc<Auditor>,
    pub signer: Arc<Signer>,
    pub store: Arc<PendingAuthStore>,
    pub bot_id: DiscordUserId,
    pub audience: Arc<str>,
    /// Token bucket on `/sso/begin`. One caller (the relay), so keyed on a constant.
    pub begin_limiter: Arc<crate::lookup::RateLimiter>,
    /// The live guild config, read per request for the mod-facing `sso_enabled` gate -
    /// the runtime half of the two-gate model. Held as the shared `ArcSwap` the rest of
    /// the bot updates, so a `/setup` toggle takes effect with no restart.
    pub guild_config: Arc<ArcSwap<GuildConfig>>,
}

/// The single caller key for the `/sso/begin` rate limit (there is one relay).
const SSO_CALLER: DiscordUserId = DiscordUserId(0);

/// Pad every `/sso/complete` outcome to this budget so denials and grants are
/// timing-uniform across our branches (it cannot mask Discord's upstream variance).
const COMPLETE_FLOOR: Duration = Duration::from_millis(800);

#[derive(Serialize)]
struct BeginResp {
    authorize_url: String,
    state: String,
}

#[derive(Deserialize)]
struct CompleteReq {
    code: String,
    state: String,
}

#[derive(Serialize)]
struct CompleteResp {
    assertion: String,
}

/// Constant-time, length-non-leaking bearer check.
///
/// Both sides are SHA-256'd to a fixed 32 bytes first, so [`subtle::ConstantTimeEq`]
/// compares constant-width inputs regardless of the presented length. An **empty
/// configured bearer never matches**, blocking the half-provisioned-deploy trap
/// where `"" == ""` would leave the endpoint open.
///
/// # Example
///
/// ```rust,ignore
/// # use discord_bot::sso::server::bearer_matches;
/// assert!(bearer_matches("s3cret", "s3cret"));
/// assert!(!bearer_matches("wrong", "s3cret"));
/// assert!(!bearer_matches("anything", ""));  // half-provisioned trap
/// ```
pub fn bearer_matches(presented: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let p = Sha256::digest(presented.as_bytes());
    let e = Sha256::digest(expected.as_bytes());
    p.as_slice().ct_eq(e.as_slice()).into()
}

/// Bind the unix-domain socket and serve until the process ends.
///
/// The socket path is per environment - `/run/botonio-staging/sso.sock` or
/// `/run/botonio-production/sso.sock` - so the two instances are isolated. The
/// containing directory is created by systemd `RuntimeDirectory=botonio-<instance>`
/// at mode `2750`; this function then hands the directory and the socket to
/// `socket_group` (this environment's SSO group, e.g. `botonio-staging-sso`) so the
/// bot and that environment's `workspace-sync` - and no other local user, nor the
/// other environment's instance - can reach it. The bot owns both and joins the
/// group via the unit's `SupplementaryGroups`, so the `chown` needs no privilege.
///
/// When `socket_group` is `None` (a local by-hand test with no provisioned group)
/// the group step is skipped and the socket keeps the bot's own group, reachable by
/// the bot user alone.
///
/// This function refuses to clobber a **live** instance: it probes the existing
/// socket path with a `connect`; if that succeeds another process is already serving
/// and we return `AddrInUse`. A stale (crash-leftover) socket is removed before
/// binding. The socket finishes at `0o660` (group-rw, world-none).
#[cfg(unix)]
pub async fn serve(
    state: SsoState,
    socket_path: std::path::PathBuf,
    socket_group: Option<String>,
    bearer: SecretString,
) -> std::io::Result<()> {
    use axum::Router;
    use axum::middleware;
    use axum::routing::post;
    if socket_path.exists() {
        if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another instance is already serving the SSO socket",
            ));
        }
        std::fs::remove_file(&socket_path)?;
    }

    let listener = tokio::net::UnixListener::bind(&socket_path)?;

    // Hand the directory and the socket to this environment's SSO group, then lock
    // the socket to group-rw / world-none. The bot owns both and joins the group via
    // SupplementaryGroups, so the chown needs no privilege. Skipped when no group is
    // configured (a local by-hand test) - the socket then keeps the bot's own group.
    if let Some(group) = socket_group.as_deref() {
        let gid = group_gid(group).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("SSO socket group {group:?} not found in /etc/group"),
            )
        })?;
        // The directory is this instance's RuntimeDirectory; chowning it lets the
        // group traverse to the socket without opening it to other local users.
        if let Some(dir) = socket_path.parent() {
            std::os::unix::fs::chown(dir, None, Some(gid))?;
        }
        std::os::unix::fs::chown(&socket_path, None, Some(gid))?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o660))?;
    }

    // The bearer must be Clone to use as axum middleware state; Arc the
    // SecretString so we don't need to expose or copy the raw bytes.
    let bearer = Arc::new(bearer);

    let app = Router::new()
        .route("/sso/begin", post(begin))
        .route("/sso/complete", post(complete))
        .route_layer(middleware::from_fn_with_state(bearer, require_bearer))
        .with_state(state);

    tracing::info!(path = %socket_path.display(), "sso: listening on unix socket");
    // Graceful shutdown on the same SIGTERM/SIGINT that closes the gateway: stop accepting and
    // let in-flight requests finish. main awaits this task so the drain completes before the
    // runtime is dropped.
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown_signal())
        .await
}

/// Resolve a system group name to its gid by reading `/etc/group`.
///
/// Deliberately dependency- and `unsafe`-free: a libc `getgrnam` would need `unsafe`
/// (the crate forbids it), and a group created with `groupadd` always has an
/// `/etc/group` entry, which is all this needs. `None` if the file is unreadable or
/// the group is absent - the caller fails the bind loudly rather than serving a
/// socket the intended group cannot reach.
#[cfg(unix)]
fn group_gid(name: &str) -> Option<u32> {
    parse_group_gid(&std::fs::read_to_string("/etc/group").ok()?, name)
}

/// Find `name`'s gid in `/etc/group`-format `contents` (`name:passwd:gid:members`).
///
/// Split out from [`group_gid`] so the parsing is unit-testable without the file.
fn parse_group_gid(contents: &str, name: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        // After the name, skip the password field and read the gid.
        fields.nth(1)?.parse().ok()
    })
}

/// Bearer gate. A miss returns a uniform `403` - the same response a denied
/// `/sso/complete` returns, so a probe cannot tell auth failure from member absence.
async fn require_bearer(
    AxState(expected): AxState<Arc<SecretString>>,
    req: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    match presented {
        Some(p) if bearer_matches(p, expected.expose_secret()) => next.run(req).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// Handle `POST /sso/begin`.
///
/// A per-caller token bucket guards the pending-auth store against a flood
/// (paired with the store's reject-when-full policy). A rate-limit trip returns
/// a uniform `403` - the same shape as every other denial - so capacity is not
/// measurable from outside.
async fn begin(AxState(state): AxState<SsoState>) -> axum::response::Response {
    if !state.guild_config.load().sso_enabled {
        return forbidden_quiet();
    }
    if !state.begin_limiter.check(SSO_CALLER, Instant::now()) {
        return deny_response(SsoDenial::RateLimited);
    }
    match flow::begin(&*state.oauth, &state.store) {
        Ok(r) => Json(BeginResp {
            authorize_url: r.authorize_url,
            state: r.state,
        })
        .into_response(),
        Err(reason) => deny_response(reason),
    }
}

/// Handle `POST /sso/complete`.
///
/// Pads the response to [`COMPLETE_FLOOR`] so the fast denials (an unknown
/// `state` returns with no network round-trip) are not latency-distinguishable
/// from a full member resolution. This normalizes our own branches; it cannot
/// mask Discord's upstream network variance.
async fn complete(
    AxState(state): AxState<SsoState>,
    Json(req): Json<CompleteReq>,
) -> axum::response::Response {
    // Start the clock before any branch so the disabled-quiet 403, every denial, and a grant
    // all pad to the same COMPLETE_FLOOR below. The disabled check must sit *inside* the
    // padded region: a sub-millisecond early return would leak the per-guild toggle state.
    let started = Instant::now();
    let response = if !state.guild_config.load().sso_enabled {
        forbidden_quiet()
    } else {
        // Rebuild the role-write client from the live config each request, so a /setup role
        // re-point takes effect without a restart. `None` means the managed roles were
        // unconfigured after boot - deny rather than guess.
        match crate::guild_config::build_role_writer(
            state.http.clone(),
            state.guild_id,
            &state.guild_config.load(),
        ) {
            Some(discord) => {
                let outcome = flow::complete(
                    &*state.oauth,
                    &discord,
                    &*state.auditor,
                    &state.signer,
                    &state.store,
                    state.bot_id,
                    &state.audience,
                    &req.code,
                    &req.state,
                )
                .await;
                match outcome {
                    SsoOutcome::Asserted(token) => {
                        Json(CompleteResp { assertion: token.0 }).into_response()
                    }
                    SsoOutcome::Denied(reason) => deny_response(reason),
                }
            }
            None => deny_response(SsoDenial::GuildReadFailed),
        }
    };

    // Pad every outcome to COMPLETE_FLOOR so latency never distinguishes which branch ran -
    // disabled, denied, or asserted.
    let elapsed = started.elapsed();
    if elapsed < COMPLETE_FLOOR {
        tokio::time::sleep(COMPLETE_FLOOR - elapsed).await;
    }
    response
}

/// The response when SSO is disabled for this guild (the mod toggle is off): the same
/// uniform `403` as any denial, but logged at `debug`, not the `sso_abuse` target. A
/// deliberately-disabled endpoint being polled is normal operation, not abuse, so it must
/// not feed the breach alert. The check runs before any lookup, so it leaks nothing about
/// membership.
fn forbidden_quiet() -> axum::response::Response {
    tracing::debug!("sso: check refused - disabled for this guild");
    StatusCode::FORBIDDEN.into_response()
}

/// Render any denial as the uniform 403, emitting the abuse signal.
///
/// The single boundary where a denial becomes a response, so begin- and
/// complete-path denials feed the `sso_abuse` alert identically and the reason is
/// observed before it is dropped. The wire shape is one bodyless 403 regardless of
/// `reason`, so a probe cannot tell the branches apart.
fn deny_response(reason: SsoDenial) -> axum::response::Response {
    tracing::warn!(target: "sso_abuse", reason = ?reason, "sso: denied");
    StatusCode::FORBIDDEN.into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn bearer_matches_only_on_equal() {
        assert!(super::bearer_matches("s3cret", "s3cret"));
        assert!(!super::bearer_matches("s3cret", "s3cre"));
        assert!(!super::bearer_matches("", "s3cret"));
        // The half-provisioned trap: an empty configured bearer matches nothing.
        assert!(!super::bearer_matches("anything", ""));
        assert!(!super::bearer_matches("", ""));
    }

    #[test]
    fn parse_group_gid_reads_the_named_group() {
        let etc = "root:x:0:\nbotonio-staging-sso:x:1007:botonio-botsci-staging,workspace-sync\n";
        assert_eq!(
            super::parse_group_gid(etc, "botonio-staging-sso"),
            Some(1007)
        );
        assert_eq!(super::parse_group_gid(etc, "absent"), None);
        // A malformed line (no gid field) yields no match rather than panicking.
        assert_eq!(super::parse_group_gid("garbage-no-colons", "x"), None);
    }
}
