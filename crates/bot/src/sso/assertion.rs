//! The signed answer: a PASETO v4.public token whose `standing` claim carries the
//! authoritative verdict on whether a Discord user holds the configured Member role.
//!
//! Asymmetric signing is intentional: the bot holds the Ed25519 private key and is
//! the only party that can mint a token. The relay (`workspace-sync`) verifies with
//! the public key alone, so a compromised relay cannot forge an assertion - it can
//! only decide whether to accept the one the bot issued.
//!
//! The lifecycle of an assertion is short by design. These are server-to-server
//! handoff tokens, not sessions: a few seconds to a minute is enough. The `jti`
//! (token identifier) is a fresh 128-bit random hex string per call; the caller
//! audits it against a short-lived seen-set to prevent replay.

use chrono::{Duration, Utc};
use pasetors::claims::Claims;
use pasetors::keys::AsymmetricSecretKey;
use pasetors::public;
use pasetors::version4::V4;

use domain::Role;
use engine::util::{DiscordGuildId, DiscordUserId};

/// The authoritative verdict the bot carries in a signed assertion.
///
/// `Member` means the subject holds the configured `Member` role specifically -
/// `DuesExpired` members are not in good standing and are a distinct case. A
/// `NotInGuild` result is still a signed negative: the bot checked and the user
/// was not present in the guild, which is meaningfully different from an error.
///
/// The [`From<Option<Role>>`] impl is the single derivation site. Every path
/// from a Discord role read to an SSO claim flows through it so the mapping
/// cannot diverge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SsoStanding {
    /// The subject holds the configured `Member` role.
    Member,
    /// The subject is present in the guild but holds `DuesExpired`, not `Member`.
    DuesExpired,
    /// The subject is present in the guild but holds `Unverified`.
    Unverified,
    /// The subject's Discord id was not found in the guild at query time.
    ///
    /// Still issued as a signed token - the relay receives a verdict, not an error.
    NotInGuild,
}

impl SsoStanding {
    /// The stable wire string for this standing, as it appears in the `standing` claim.
    ///
    /// These strings are part of the protocol contract between the bot and the relay.
    /// Change them only with a coordinated relay deploy.
    pub fn as_str(self) -> &'static str {
        match self {
            SsoStanding::Member => "member",
            SsoStanding::DuesExpired => "dues_expired",
            SsoStanding::Unverified => "unverified",
            SsoStanding::NotInGuild => "not_in_guild",
        }
    }
}

/// Maps an observed Discord role to the SSO verdict.
///
/// `None` means the user was not found in the guild at all - that becomes
/// [`SsoStanding::NotInGuild`] rather than an error, because the bot can
/// issue a signed negative.
impl From<Option<Role>> for SsoStanding {
    fn from(role: Option<Role>) -> Self {
        match role {
            None => SsoStanding::NotInGuild,
            Some(Role::Member) => SsoStanding::Member,
            Some(Role::DuesExpired) => SsoStanding::DuesExpired,
            Some(Role::Unverified) => SsoStanding::Unverified,
        }
    }
}

/// A minted PASETO v4.public token, ready to hand to the relay.
///
/// The inner `String` is the full `v4.public....` wire value. Treat it as
/// opaque outside of tests; only the relay's verifier should inspect the
/// contents.
pub struct PasetoToken(pub String);

/// How assertion signing can fail.
///
/// The detail here is for internal tracing. The SSO flow renders a single
/// uniform denial to the caller regardless of which variant fires - callers
/// must not be able to distinguish RNG failures from key failures.
#[derive(Debug, thiserror::Error)]
pub enum AssertionError {
    /// Building the claims set failed (e.g. a claim value that violates the
    /// PASETO spec). Wraps the underlying [`pasetors::errors::Error`].
    #[error("building the assertion claims failed")]
    Claims(#[source] pasetors::errors::Error),
    /// The Ed25519 signing step itself failed.
    #[error("signing the assertion failed")]
    Sign(#[source] pasetors::errors::Error),
    /// The OS random-number generator was unavailable when generating the `jti`.
    ///
    /// A locked-down sandbox or a `seccomp` filter can block the underlying
    /// `getrandom(2)` syscall. Denies cleanly rather than panicking the task.
    #[error("the OS RNG failed while generating the jti")]
    Rng,
}

/// Signs PASETO v4.public assertions with the bot's Ed25519 private key.
///
/// One `Signer` is created per process and shared by reference across the SSO
/// server tasks. Sharing by reference is cheap: the key material is held as an
/// [`AsymmetricSecretKey<V4>`], and the other fields are plain `String`/`i64`.
///
/// # Claims layout
///
/// Every token carries:
/// - `iss` - the configured issuer (typically `"botonio"`)
/// - `aud` - the relay's expected audience (typically `"workspace-sync"`)
/// - `sub` - the Discord user snowflake as a decimal string
/// - `jti` - a fresh 128-bit random hex id for replay prevention
/// - `exp` - `ttl_secs` from now (seconds, not minutes - this is a handoff, not a session)
/// - `kid` - which key version signed this token
/// - `guild` - the Discord guild snowflake as a decimal string
/// - `standing` - one of the [`SsoStanding::as_str`] wire values
pub struct Signer {
    key: AsymmetricSecretKey<V4>,
    issuer: String,
    audience: String,
    guild: DiscordGuildId,
    kid: String,
    ttl_secs: i64,
}

impl Signer {
    /// Build a signer from its parts.
    ///
    /// `ttl_secs` is the token lifetime in seconds. Keep it short (30-60s is
    /// enough for a server-to-server hop); the relay's replay-prevention window
    /// should match or exceed this value.
    pub fn new(
        key: AsymmetricSecretKey<V4>,
        issuer: String,
        audience: String,
        guild: DiscordGuildId,
        kid: String,
        ttl_secs: i64,
    ) -> Self {
        Self {
            key,
            issuer,
            audience,
            guild,
            kid,
            ttl_secs,
        }
    }

    /// Sign an assertion for `subject` carrying `standing`.
    ///
    /// Returns the token and its freshly generated `jti`. The caller is
    /// responsible for recording the `jti` in the audit log and checking it
    /// against the replay-prevention store before handing the token to the relay.
    ///
    /// # Errors
    ///
    /// Returns [`AssertionError::Rng`] if the OS RNG is unavailable (never
    /// panics), [`AssertionError::Claims`] if a claim value violates the PASETO
    /// spec, or [`AssertionError::Sign`] if the Ed25519 signing step fails.
    pub fn sign(
        &self,
        subject: DiscordUserId,
        standing: SsoStanding,
    ) -> Result<(PasetoToken, String), AssertionError> {
        let jti = random_jti()?;
        let exp = (Utc::now() + Duration::seconds(self.ttl_secs)).to_rfc3339();

        let mut claims = Claims::new().map_err(AssertionError::Claims)?;
        claims
            .issuer(&self.issuer)
            .map_err(AssertionError::Claims)?;
        claims
            .audience(&self.audience)
            .map_err(AssertionError::Claims)?;
        claims
            .subject(&subject.0.to_string())
            .map_err(AssertionError::Claims)?;
        claims
            .token_identifier(&jti)
            .map_err(AssertionError::Claims)?;
        claims.expiration(&exp).map_err(AssertionError::Claims)?;
        claims
            .add_additional("kid", self.kid.as_str())
            .map_err(AssertionError::Claims)?;
        claims
            .add_additional("guild", self.guild.0.to_string())
            .map_err(AssertionError::Claims)?;
        claims
            .add_additional("standing", standing.as_str())
            .map_err(AssertionError::Claims)?;

        let token = public::sign(&self.key, &claims, None, None).map_err(AssertionError::Sign)?;
        Ok((PasetoToken(token), jti))
    }
}

/// Generate a fresh 128-bit hex `jti`.
///
/// Returns [`AssertionError::Rng`] rather than panicking if the OS RNG is
/// unavailable, so the SSO flow can issue a clean denial instead of crashing
/// the server task.
fn random_jti() -> Result<String, AssertionError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| AssertionError::Rng)?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::convert::TryFrom;
    use pasetors::claims::ClaimsValidationRules;
    use pasetors::keys::{AsymmetricKeyPair, Generate};
    use pasetors::token::{Public, UntrustedToken};

    #[test]
    fn member_assertion_carries_the_full_claim_set() {
        let kp = AsymmetricKeyPair::<V4>::generate().unwrap();
        let signer = Signer::new(
            kp.secret,
            "botonio".to_owned(),
            "workspace-sync".to_owned(),
            DiscordGuildId(42),
            "v1".to_owned(),
            60,
        );
        let (token, jti) = signer.sign(DiscordUserId(7), SsoStanding::Member).unwrap();

        let untrusted = UntrustedToken::<Public, V4>::try_from(token.0.as_str()).unwrap();
        let mut rules = ClaimsValidationRules::new();
        rules.validate_issuer_with("botonio");
        rules.validate_audience_with("workspace-sync");
        let trusted = public::verify(&kp.public, &untrusted, &rules, None, None).unwrap();
        let claims = trusted.payload_claims().unwrap();

        assert_eq!(claims.get_claim("sub").unwrap(), "7");
        assert_eq!(claims.get_claim("standing").unwrap(), "member");
        assert_eq!(claims.get_claim("guild").unwrap(), "42");
        assert_eq!(claims.get_claim("kid").unwrap(), "v1");
        assert_eq!(claims.get_claim("jti").unwrap(), jti.as_str());
        assert!(claims.get_claim("exp").is_some());
    }

    #[test]
    fn not_in_guild_is_a_signed_negative_not_an_error() {
        let kp = AsymmetricKeyPair::<V4>::generate().unwrap();
        let signer = Signer::new(
            kp.secret,
            "i".into(),
            "a".into(),
            DiscordGuildId(1),
            "v1".into(),
            60,
        );
        let (token, _) = signer
            .sign(DiscordUserId(9), SsoStanding::NotInGuild)
            .unwrap();
        assert!(token.0.starts_with("v4.public."));
    }

    #[test]
    fn keygen_hex_round_trip_produces_a_working_signing_key() {
        // Locks the seam between the keypair generator and the secret-loader:
        // generate -> hex-encode as `as_bytes()` -> hex-decode -> `from()` -> sign -> verify.
        // This is exactly the path `sso_keygen` + `load_secrets` travel at provisioning time.
        let kp = AsymmetricKeyPair::<V4>::generate().unwrap();
        let hex_secret: String = kp
            .secret
            .as_bytes()
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();

        // Reconstruct via the same path as `config::load_secrets`.
        let raw = hex::decode(hex_secret.trim()).expect("hex decode failed");
        let reconstructed =
            AsymmetricSecretKey::<V4>::from(&raw).expect("key reconstruction failed");

        let signer = Signer::new(
            reconstructed,
            "botonio".to_owned(),
            "workspace-sync".to_owned(),
            DiscordGuildId(1),
            "v1".to_owned(),
            60,
        );
        let (token, _jti) = signer.sign(DiscordUserId(1), SsoStanding::Member).unwrap();

        // Verify with the original public key - proves the reconstructed secret is live.
        let untrusted = UntrustedToken::<Public, V4>::try_from(token.0.as_str()).unwrap();
        let mut rules = ClaimsValidationRules::new();
        rules.validate_issuer_with("botonio");
        rules.validate_audience_with("workspace-sync");
        public::verify(&kp.public, &untrusted, &rules, None, None)
            .expect("round-trip verification failed: keygen output does not reconstruct a working signing key");
    }

    #[test]
    fn dues_expired_without_member_never_resolves_to_member() {
        // The standing mapping is the gate. Lock the dangerous case: a member who
        // holds DuesExpired (not Member) must be dues_expired, never member.
        assert_eq!(SsoStanding::from(Some(Role::Member)), SsoStanding::Member);
        assert_eq!(
            SsoStanding::from(Some(Role::DuesExpired)),
            SsoStanding::DuesExpired
        );
        assert_eq!(
            SsoStanding::from(Some(Role::Unverified)),
            SsoStanding::Unverified
        );
        assert_eq!(SsoStanding::from(None), SsoStanding::NotInGuild);
    }
}
