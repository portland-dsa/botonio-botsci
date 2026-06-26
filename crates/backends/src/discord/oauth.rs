//! The Discord OAuth 2.0 surface the SSO broker uses: build an authorize URL with
//! PKCE + CSRF state, exchange the returned code for a user token, and read the
//! verified user id from it. Separate from the bot-token `serenity` client - this
//! is the person-delegated `identify` flow, used once per sign-in and never stored.

use async_trait::async_trait;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use super::error::DiscordError;
use crate::util::DiscordUserId;

const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const USERINFO_URL: &str = "https://discord.com/api/users/@me";

/// A person's Discord access token, scope `identify`. Used once for `/users/@me`
/// then dropped; held in `SecretString` so it cannot be logged.
pub struct AccessToken(SecretString);

impl AccessToken {
    pub fn new(raw: String) -> Self {
        Self(SecretString::from(raw))
    }
    fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

/// A built Discord authorize URL - the front-channel target the relay redirects to.
pub struct AuthorizeUrl(pub String);

/// The opaque CSRF `state` minted for one flow, echoed back at the callback.
#[derive(Clone)]
pub struct State(pub String);

/// The PKCE code verifier held between begin and complete; secret.
pub struct PkceVerifier(SecretString);

impl PkceVerifier {
    pub fn new(raw: String) -> Self {
        Self(SecretString::from(raw))
    }
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

/// The person-delegated Discord OAuth flow, behind a flat trait so the SSO logic
/// tests against a fake with no network.
#[async_trait]
pub trait DiscordOAuthClient: Send + Sync {
    /// Mint an authorize URL with a fresh CSRF `state` and PKCE (S256) challenge,
    /// returning the `state` and verifier to hold until the callback. No I/O.
    fn authorize_url(&self) -> (AuthorizeUrl, State, PkceVerifier);

    /// Exchange `code` (with its matching PKCE `verifier`) for a user access token.
    async fn exchange_code(
        &self,
        code: &str,
        verifier: &PkceVerifier,
    ) -> Result<AccessToken, DiscordError>;

    /// Read the verified `DiscordUserId` from `GET /users/@me` using `token`.
    async fn current_user(&self, token: &AccessToken) -> Result<DiscordUserId, DiscordError>;
}

/// Live implementation over `oauth2` (for the flow) and `reqwest` (for `/users/@me`).
pub struct DiscordOAuthHttp {
    oauth: BasicClient<
        oauth2::EndpointSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointSet,
    >,
    http: reqwest::Client,
}

impl DiscordOAuthHttp {
    /// Build from the registered OAuth app credentials. `redirect_uri` is the relay's
    /// exact public callback; Discord rejects any code whose redirect does not match.
    pub fn new(
        client_id: String,
        client_secret: SecretString,
        redirect_uri: String,
    ) -> Result<Self, DiscordError> {
        let oauth = BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(client_secret.expose_secret().to_owned()))
            .set_auth_uri(AuthUrl::new(AUTHORIZE_URL.to_owned()).expect("static authorize URL"))
            .set_token_uri(TokenUrl::new(TOKEN_URL.to_owned()).expect("static token URL"))
            .set_redirect_uri(
                RedirectUrl::new(redirect_uri).map_err(|_| DiscordError::OAuthExchange)?,
            );
        // The token-exchange http client must NOT follow redirects (SSRF-hardening
        // recommended by the oauth2 crate).
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(DiscordError::OAuthUserRead)?;
        Ok(Self { oauth, http })
    }
}

#[derive(Deserialize)]
struct UserInfo {
    id: String,
}

#[async_trait]
impl DiscordOAuthClient for DiscordOAuthHttp {
    fn authorize_url(&self) -> (AuthorizeUrl, State, PkceVerifier) {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = self
            .oauth
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("identify".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        (
            AuthorizeUrl(url.to_string()),
            State(csrf.into_secret()),
            PkceVerifier::new(verifier.into_secret()),
        )
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &PkceVerifier,
    ) -> Result<AccessToken, DiscordError> {
        let token = self
            .oauth
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.expose().to_owned()))
            .request_async(&self.http)
            .await
            .map_err(|_| DiscordError::OAuthExchange)?;
        Ok(AccessToken::new(token.access_token().secret().to_owned()))
    }

    async fn current_user(&self, token: &AccessToken) -> Result<DiscordUserId, DiscordError> {
        let info: UserInfo = self
            .http
            .get(USERINFO_URL)
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(DiscordError::OAuthUserRead)?
            .error_for_status()
            .map_err(DiscordError::OAuthUserRead)?
            .json()
            .await
            .map_err(DiscordError::OAuthUserRead)?;
        let id = info
            .id
            .parse::<u64>()
            .map_err(|_| DiscordError::OAuthUserMalformed)?;
        Ok(DiscordUserId(id))
    }
}

#[cfg(feature = "fakes")]
mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// State-based fake: seed `code -> (token, user)`; `authorize_url` returns a
    /// fixed canned triple so a flow test can drive begin -> complete deterministically.
    #[derive(Default)]
    pub struct FakeDiscordOAuth {
        by_code: Mutex<HashMap<String, (String, DiscordUserId)>>,
        by_token: Mutex<HashMap<String, DiscordUserId>>,
    }

    impl FakeDiscordOAuth {
        pub fn seed(&self, code: &str, token: &str, user: DiscordUserId) {
            self.by_code
                .lock()
                .unwrap()
                .insert(code.to_owned(), (token.to_owned(), user));
            self.by_token.lock().unwrap().insert(token.to_owned(), user);
        }
    }

    #[async_trait]
    impl DiscordOAuthClient for FakeDiscordOAuth {
        fn authorize_url(&self) -> (AuthorizeUrl, State, PkceVerifier) {
            (
                AuthorizeUrl("https://discord.test/authorize".to_owned()),
                State("fake-state".to_owned()),
                PkceVerifier::new("fake-verifier".to_owned()),
            )
        }
        async fn exchange_code(
            &self,
            code: &str,
            _verifier: &PkceVerifier,
        ) -> Result<AccessToken, DiscordError> {
            match self.by_code.lock().unwrap().get(code) {
                Some((token, _)) => Ok(AccessToken::new(token.clone())),
                None => Err(DiscordError::OAuthExchange),
            }
        }
        async fn current_user(&self, token: &AccessToken) -> Result<DiscordUserId, DiscordError> {
            match self.by_token.lock().unwrap().get(token.expose()) {
                Some(id) => Ok(*id),
                None => Err(DiscordError::OAuthExchange),
            }
        }
    }
}

#[cfg(feature = "fakes")]
pub use fake::FakeDiscordOAuth;
