//! The begin/complete state machine, fail-closed. `Denied` is the safe default arm,
//! and every `Denied` renders one identical response - a restart or TTL eviction
//! mid-flow simply denies. "Not a member" is NOT a denial: once a verified id exists
//! the bot signs a negative `standing`, so a member and a non-member are one shape.

use engine::audit::AuditLog;
use engine::backends::discord::{DiscordClient, DiscordOAuthClient};
use engine::util::DiscordUserId;

use super::assertion::{PasetoToken, Signer, SsoStanding};
use super::store::PendingAuthStore;

/// The URL and state token returned by [`begin`].
///
/// Hand `authorize_url` to the relay to redirect the user, and hold `state`
/// for the matching [`complete`] call (or verify it on return before calling
/// complete).
pub struct BeginResponse {
    pub authorize_url: String,
    pub state: String,
}

/// The result of [`complete`].
///
/// `Asserted` carries the signed token (with a possibly negative standing);
/// `Denied` is a protocol failure rendered uniformly to the caller so no
/// timing difference or error shape leaks which branch was taken.
pub enum SsoOutcome {
    Asserted(PasetoToken),
    Denied(SsoDenial),
}

/// Why a [`complete`] denied.
///
/// All variants render identically on the wire; the variant only drives the
/// `tracing` counter on the `sso_abuse` target that feeds the abuse alert.
/// A spike in `CodeExchangeFailed`, for instance, can signal a revoked or
/// rotated upstream credential without exposing the underlying distinction to
/// the caller.
#[derive(Debug, Clone, Copy)]
pub enum SsoDenial {
    /// The caller exceeded the `/sso/begin` rate limit (checked in the server handler).
    RateLimited,
    /// Unknown, expired, or already-consumed `state`.
    StateInvalid,
    /// The Discord token exchange failed - bad/expired code, or a revoked OAuth secret
    /// (a 401 here is an upstream-credential breach signal worth alerting on).
    CodeExchangeFailed,
    /// The token exchange succeeded but the follow-up `/users/@me` read failed (a Discord
    /// 5xx, a transient 401, or a malformed body). Kept distinct from [`CodeExchangeFailed`]
    /// so a userinfo outage cannot masquerade as a revoked-credential breach signal in the
    /// `sso_abuse` counter.
    UserReadFailed,
    GuildReadFailed,
    SignFailed,
    /// The `sso_check` audit row could not be written. Fail closed - the assertion is
    /// a Workspace-login credential and must never leave the bot unlogged.
    AuditUnavailable,
    StoreFull,
}

/// Mint an authorize URL and stash the pending auth.
///
/// Returns `Err(SsoDenial::StoreFull)` if every slot in `store` is occupied by a
/// non-expired entry. The caller should render a uniform denial - not a "try again
/// later" - so a flooder cannot measure remaining capacity.
///
/// # Example
///
/// ```rust,ignore
/// let resp = begin(&oauth, &store)?;
/// // Redirect the relay to resp.authorize_url; hold resp.state for the callback.
/// ```
pub fn begin(
    oauth: &impl DiscordOAuthClient,
    store: &PendingAuthStore,
) -> Result<BeginResponse, SsoDenial> {
    let (url, state, verifier) = oauth.authorize_url();
    store
        .begin(state.clone(), verifier)
        .map_err(|_| SsoDenial::StoreFull)?;
    Ok(BeginResponse {
        authorize_url: url.0,
        state: state.0,
    })
}

/// Resolve a callback to a signed assertion or a uniform denial.
///
/// Walks the flow in order:
///
/// 1. Consume `state` from `store` - unknown or expired state denies immediately.
/// 2. Exchange `code` for a user access token via `oauth`.
/// 3. Read the verified Discord user id from the token.
/// 4. Read the user's guild role via `discord`.
/// 5. Sign the standing assertion with `signer`.
/// 6. Write an `sso_check` row to `audit` - **fail closed**: if the write fails,
///    the assertion is withheld and the caller gets `Denied(AuditUnavailable)`.
///
/// A "not in guild" result is NOT a denial: the bot signs a negative [`SsoStanding`]
/// and returns `Asserted` so the relay receives a verdict, not an error.
#[allow(clippy::too_many_arguments)]
pub async fn complete(
    oauth: &impl DiscordOAuthClient,
    discord: &impl DiscordClient,
    audit: &impl AuditLog,
    signer: &Signer,
    store: &PendingAuthStore,
    bot_id: DiscordUserId,
    audience: &str,
    code: &str,
    state: &str,
) -> SsoOutcome {
    let Some(verifier) = store.take(state) else {
        return deny(SsoDenial::StateInvalid);
    };
    let token = match oauth.exchange_code(code, &verifier).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "sso: code exchange failed");
            return deny(SsoDenial::CodeExchangeFailed);
        }
    };
    let subject = match oauth.current_user(&token).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "sso: user read failed");
            return deny(SsoDenial::UserReadFailed);
        }
    };
    let standing: SsoStanding = match discord.member_status_role(subject).await {
        Ok(role) => role.into(),
        Err(e) => {
            tracing::warn!(error = %e, "sso: guild role read failed");
            return deny(SsoDenial::GuildReadFailed);
        }
    };

    let (assertion, jti) = match signer.sign(subject, standing) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(error = %e, "sso: signing failed");
            return deny(SsoDenial::SignFailed);
        }
    };

    // Fail closed: actor = the bot, subject = the verified id. Negatives are recorded
    // too - the log answers who tried and got in, as what. The assertion is a
    // Workspace-login credential, so it must NOT be returned unless its append-only
    // row is durably written first; a failed write denies, never grants unlogged.
    let detail = serde_json::json!({
        "standing": standing.as_str(),
        "audience": audience,
        "jti": jti,
    });
    if let Err(e) = audit.record(bot_id, subject, "sso_check", detail).await {
        tracing::error!(error = %e, "sso: audit write failed - denying");
        return deny(SsoDenial::AuditUnavailable);
    }

    SsoOutcome::Asserted(assertion)
}

/// Wrap a denial reason as the uniform [`SsoOutcome::Denied`].
///
/// Logging is deliberately left out here: the server renders every denial
/// identically and emits the `sso_abuse` signal at that boundary (reading the
/// reason as it does), so begin- and complete-path denials are observed the same
/// way and the flow layer stays free of that side effect.
fn deny(reason: SsoDenial) -> SsoOutcome {
    SsoOutcome::Denied(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use backends::discord::{FakeDiscord, FakeDiscordOAuth};
    use domain::Role;
    use pasetors::keys::{AsymmetricKeyPair, Generate};
    use pasetors::version4::V4;

    use super::super::store::PendingAuthStore;
    use engine::util::DiscordGuildId;

    /// Records every audit row so a test can assert how many were written.
    #[derive(Default)]
    struct RecordingAudit {
        rows: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl AuditLog for RecordingAudit {
        type Error = Infallible;
        async fn record(
            &self,
            _actor: DiscordUserId,
            _subject: DiscordUserId,
            action: &str,
            _detail: serde_json::Value,
        ) -> Result<(), Infallible> {
            self.rows.lock().unwrap().push(action.to_owned());
            Ok(())
        }
    }

    fn signer() -> Signer {
        let kp = AsymmetricKeyPair::<V4>::generate().unwrap();
        Signer::new(
            kp.secret,
            "botonio".into(),
            "workspace-sync".into(),
            DiscordGuildId(1),
            "v1".into(),
            60,
        )
    }

    // A flow needs a begin (to stash the fake's "fake-state") before complete.
    fn store_with_pending() -> PendingAuthStore {
        let store = PendingAuthStore::new(8, Duration::from_secs(60));
        let oauth = FakeDiscordOAuth::default();
        begin(&oauth, &store).unwrap(); // stashes State("fake-state")
        store
    }

    #[tokio::test]
    async fn member_yields_a_signed_member_assertion_and_one_audit_row() {
        let oauth = FakeDiscordOAuth::default();
        oauth.seed("good-code", "tok", DiscordUserId(7));
        let discord = FakeDiscord::default();
        discord.seed_status(DiscordUserId(7), Role::Member);
        let audit = RecordingAudit::default();
        let store = store_with_pending();

        let out = complete(
            &oauth,
            &discord,
            &audit,
            &signer(),
            &store,
            DiscordUserId(99),
            "workspace-sync",
            "good-code",
            "fake-state",
        )
        .await;

        assert!(matches!(out, SsoOutcome::Asserted(_)));
        assert_eq!(audit.rows.lock().unwrap().as_slice(), ["sso_check"]);
    }

    #[tokio::test]
    async fn not_in_guild_is_asserted_not_denied() {
        let oauth = FakeDiscordOAuth::default();
        oauth.seed("good-code", "tok", DiscordUserId(7)); // present in OAuth...
        let discord = FakeDiscord::default(); // ...but not seeded as a guild member
        let audit = RecordingAudit::default();
        let store = store_with_pending();

        let out = complete(
            &oauth,
            &discord,
            &audit,
            &signer(),
            &store,
            DiscordUserId(99),
            "workspace-sync",
            "good-code",
            "fake-state",
        )
        .await;
        assert!(matches!(out, SsoOutcome::Asserted(_))); // signed negative, not an error
    }

    #[tokio::test]
    async fn unknown_state_denies_and_writes_no_row() {
        let oauth = FakeDiscordOAuth::default();
        let discord = FakeDiscord::default();
        let audit = RecordingAudit::default();
        let store = PendingAuthStore::new(8, Duration::from_secs(60)); // no begin

        let out = complete(
            &oauth,
            &discord,
            &audit,
            &signer(),
            &store,
            DiscordUserId(99),
            "workspace-sync",
            "code",
            "fake-state",
        )
        .await;
        assert!(matches!(out, SsoOutcome::Denied(SsoDenial::StateInvalid)));
        assert!(audit.rows.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_code_denies() {
        let oauth = FakeDiscordOAuth::default(); // no seeded code
        let discord = FakeDiscord::default();
        let audit = RecordingAudit::default();
        let store = store_with_pending();

        let out = complete(
            &oauth,
            &discord,
            &audit,
            &signer(),
            &store,
            DiscordUserId(99),
            "workspace-sync",
            "wrong",
            "fake-state",
        )
        .await;
        assert!(matches!(out, SsoOutcome::Denied(_)));
    }

    /// An audit double that always fails - proves a verified check fails closed.
    struct FailingAudit;
    #[async_trait]
    impl AuditLog for FailingAudit {
        type Error = std::io::Error;
        async fn record(
            &self,
            _: DiscordUserId,
            _: DiscordUserId,
            _: &str,
            _: serde_json::Value,
        ) -> Result<(), std::io::Error> {
            Err(std::io::Error::other("audit down"))
        }
    }

    #[tokio::test]
    async fn audit_failure_denies_and_mints_no_grant() {
        let oauth = FakeDiscordOAuth::default();
        oauth.seed("good-code", "tok", DiscordUserId(7));
        let discord = FakeDiscord::default();
        discord.seed_status(DiscordUserId(7), Role::Member);
        let store = store_with_pending();

        let out = complete(
            &oauth,
            &discord,
            &FailingAudit,
            &signer(),
            &store,
            DiscordUserId(99),
            "workspace-sync",
            "good-code",
            "fake-state",
        )
        .await;
        // A real member, but the audit row could not be written: deny, no assertion.
        assert!(matches!(
            out,
            SsoOutcome::Denied(SsoDenial::AuditUnavailable)
        ));
    }
}
