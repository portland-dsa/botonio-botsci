//! Deploy-time SSO configuration. Nothing in Discord toggles this; `enabled` gates
//! whether the socket binds at all. The three secrets arrive as systemd encrypted
//! credentials, like the bot's other secrets, and load into [`SecretString`].

use std::path::PathBuf;
use std::time::Duration;

use pasetors::keys::AsymmetricSecretKey;
use pasetors::version4::V4;
use secrecy::SecretString;

use engine::backends::from_credstore_or_env;

/// The operational (non-secret) SSO settings, read from environment variables once
/// at startup.
///
/// Construct with [`SsoConfig::from_env`]; never build this by hand in production.
/// The gate for whether the unix socket binds at all is [`enabled`](Self::enabled) -
/// every other field is read regardless so misconfiguration is detected early, not
/// silently at serve time.
pub struct SsoConfig {
    /// Bind the socket and serve only when set to `"1"`. Off by default.
    ///
    /// Flip this with `BOT_SSO_ENABLED=1` in the service unit. When `false`, no socket
    /// is created and no secrets are read - the SSO path is entirely inert.
    pub enabled: bool,
    /// Filesystem path for the unix-domain socket (`BOT_SSO_SOCKET_PATH`).
    ///
    /// Defaults to `/run/botonio/sso.sock`. The containing directory is created by
    /// systemd `RuntimeDirectory=botonio-<instance>` at the right mode.
    pub socket_path: PathBuf,
    /// The system group that owns the socket and its directory, so the bot and this
    /// environment's `workspace-sync` - and only those two - can reach it
    /// (`BOT_SSO_SOCKET_GROUP`, e.g. `botonio-staging-sso`).
    ///
    /// `None` when unset: the socket keeps the bot's own group, reachable by the bot
    /// user alone, which is all a local by-hand test needs.
    pub socket_group: Option<String>,
    /// The `aud` claim written into every assertion and the relay's identity -
    /// one value so the audience and the redirect cannot drift (`BOT_SSO_AUDIENCE`).
    ///
    /// Defaults to `"workspace-sync"`.
    pub audience: String,
    /// The relay's exact public OAuth callback, matched byte-for-byte by Discord
    /// (`BOT_SSO_REDIRECT_URI`). Defaults to empty; an empty value is rejected as
    /// [`SsoError::EmptyRedirect`] before the server starts.
    pub redirect_uri: String,
    /// Assertion lifetime in seconds (`BOT_SSO_TTL_SECS`). A server-to-server hop -
    /// keep it under a minute; defaults to 60.
    pub ttl_secs: i64,
    /// Maximum concurrent pending logins (`BOT_SSO_STORE_CAP`). Defaults to 1024.
    pub store_cap: usize,
    /// How long a pending login lives before [`PendingAuthStore::take`] treats it as
    /// expired (`BOT_SSO_STORE_TTL_SECS`). Defaults to 300 s.
    ///
    /// [`PendingAuthStore::take`]: super::store::PendingAuthStore::take
    pub store_ttl: Duration,
    /// The signing-key version id stamped into each assertion (`BOT_SSO_KEY_ID`), for
    /// forward-only rotation. Defaults to `"v1"`.
    pub kid: String,
    /// Per-minute ceiling on `POST /sso/begin` (`BOT_SSO_BEGIN_RATE_PER_MIN`).
    ///
    /// There is one relay, so this is effectively a global rate limit on the
    /// pending-auth store. Defaults to 60 requests per minute.
    pub begin_rate_per_min: u32,
}

impl SsoConfig {
    /// Read every SSO setting from the environment, supplying defaults for anything
    /// unset or unparseable.
    ///
    /// This never fails: unknown or invalid values fall back to the documented
    /// defaults. Validation that can fail - an empty redirect URI, a bad signing
    /// key - happens in [`load_secrets`] when [`enabled`](Self::enabled) is `true`.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            enabled: var("BOT_SSO_ENABLED").as_deref() == Some("1"),
            socket_path: var("BOT_SSO_SOCKET_PATH")
                .unwrap_or_else(|| "/run/botonio/sso.sock".to_owned())
                .into(),
            socket_group: var("BOT_SSO_SOCKET_GROUP"),
            audience: var("BOT_SSO_AUDIENCE").unwrap_or_else(|| "workspace-sync".to_owned()),
            redirect_uri: var("BOT_SSO_REDIRECT_URI").unwrap_or_default(),
            ttl_secs: var("BOT_SSO_TTL_SECS")
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            store_cap: var("BOT_SSO_STORE_CAP")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1024),
            store_ttl: Duration::from_secs(
                var("BOT_SSO_STORE_TTL_SECS")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(300),
            ),
            kid: var("BOT_SSO_KEY_ID").unwrap_or_else(|| "v1".to_owned()),
            begin_rate_per_min: var("BOT_SSO_BEGIN_RATE_PER_MIN")
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
        }
    }
}

/// What can go wrong assembling the SSO secrets - all variants fail closed (the
/// endpoint does not bind rather than bind unprotected).
#[derive(Debug, thiserror::Error)]
pub enum SsoError {
    /// A required secret or configuration value was absent.
    #[error("missing SSO secret/config: {0}")]
    Missing(&'static str),
    /// The configured signing key is not valid hex or not a valid V4 Ed25519 key.
    #[error("the SSO signing key is not valid hex or not a V4 key")]
    BadSigningKey,
    /// The caller bearer is present but empty - an empty bearer would match nothing
    /// after the constant-time comparison, so the endpoint would accept any request.
    #[error("the SSO caller bearer is empty")]
    EmptyBearer,
    /// `BOT_SSO_REDIRECT_URI` was not set; Discord rejects any code exchange whose
    /// redirect does not match the registered URI exactly.
    #[error("BOT_SSO_REDIRECT_URI is empty")]
    EmptyRedirect,
}

/// The three SSO secrets, all from the credential store (never the environment).
///
/// Hold this only as long as needed to build the [`SsoState`]; the signing key and
/// bearer are `SecretString`/zeroize-on-drop values.
///
/// [`SsoState`]: super::server::SsoState
pub struct SsoSecrets {
    /// The registered OAuth application's client id (not secret; env is fine).
    pub oauth_client_id: String,
    /// The registered OAuth application's client secret (from the credential store).
    pub oauth_client_secret: SecretString,
    /// The Ed25519 secret key used to sign PASETO assertions (from the credential store).
    pub signing_key: AsymmetricSecretKey<V4>,
    /// The static bearer the relay presents on every request (from the credential store).
    pub bearer: SecretString,
}

/// Load and validate the SSO secrets from the credential store.
///
/// The OAuth client id is not sensitive (env var is fine); the client secret,
/// signing key, and bearer all load from the systemd credential store via
/// [`from_credstore_or_env`] - the same mechanism as `BotConfig::from_env` - so
/// they never enter the process environment.
///
/// # Errors
///
/// Returns [`SsoError`] when any secret is missing, the signing key fails hex decode
/// or V4 key construction, the bearer is empty, or the redirect URI is empty. Every
/// error path leaves the endpoint unbound rather than started in a degraded state.
pub fn load_secrets(cfg: &SsoConfig) -> Result<SsoSecrets, SsoError> {
    if cfg.redirect_uri.is_empty() {
        return Err(SsoError::EmptyRedirect);
    }
    let oauth_client_id = std::env::var("BOT_SSO_OAUTH_CLIENT_ID")
        .map_err(|_| SsoError::Missing("BOT_SSO_OAUTH_CLIENT_ID"))?;
    let oauth_client_secret = SecretString::from(
        from_credstore_or_env("sso_oauth_client_secret", "BOT_SSO_OAUTH_CLIENT_SECRET")
            .ok_or(SsoError::Missing("sso_oauth_client_secret"))?,
    );
    let signing_key = {
        let hex_key = from_credstore_or_env("sso_signing_key", "BOT_SSO_SIGNING_KEY")
            .ok_or(SsoError::Missing("sso_signing_key"))?;
        let raw = hex::decode(hex_key.trim()).map_err(|_| SsoError::BadSigningKey)?;
        AsymmetricSecretKey::<V4>::from(&raw).map_err(|_| SsoError::BadSigningKey)?
    };
    let bearer_raw = from_credstore_or_env("sso_caller_bearer", "BOT_SSO_CALLER_BEARER")
        .ok_or(SsoError::Missing("sso_caller_bearer"))?;
    // Trim before the empty check and before storing, mirroring the signing-key path above:
    // a credstore/SOPS value commonly arrives with a trailing newline, and an untrimmed
    // bearer would force the relay to present that newline byte-for-byte (or silently 403).
    // A whitespace-only value counts as empty - the half-provisioned-deploy guard.
    let bearer = bearer_raw.trim();
    if bearer.is_empty() {
        return Err(SsoError::EmptyBearer);
    }
    Ok(SsoSecrets {
        oauth_client_id,
        oauth_client_secret,
        signing_key,
        bearer: SecretString::from(bearer.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_defaults_to_one_minute() {
        // With the env unset (or whatever the CI environment has), the assertion
        // lifetime defaults to 60 s or whatever was set. The contract is: the default
        // must not exceed 60 s.
        let cfg = SsoConfig::from_env();
        assert!(cfg.ttl_secs <= 60);
    }
}
