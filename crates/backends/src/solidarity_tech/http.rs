//! The live `SolidarityTechHttp` client: paced, retrying reqwest calls.

use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};

use crate::MemberPage;
use crate::util::{DiscordHandle, DiscordUserId, DryRun, Email, Phone};

use super::client::{SolidarityTechClient, StClearFlags};
use super::error::SolidarityTechError;
use super::member::{CustomUserProperty, SolidarityTechMember};
use super::wire::{
    CustomPropsListResponse, StWriteProps, UserUpdate, UsersListResponse, decode_members,
};

/// Page size (`_limit`) for filtered reads.
///
/// A unique email or phone returns 0-1 rows in practice, so one page is plenty;
/// the cap only matters if a filter ever returns several, which `find_members`
/// warns about rather than silently truncating.
const ST_PAGE_SIZE: u32 = 100;

/// Leading pause before every request, holding us under the 60 req / 30 s
/// (~2 req/s) cap with headroom; 429s are still honored via `Retry-After` on
/// top of this floor.
const REQUEST_PACING: Duration = Duration::from_millis(500);
/// Times `send_with_retry` re-attempts a request that returns HTTP 429 before
/// giving up with [`SolidarityTechError::RateLimitExhausted`].
const RATE_LIMIT_RETRIES: u32 = 3;
/// Fallback wait when a 429 response omits a parseable `Retry-After` header.
const DEFAULT_RETRY_AFTER_SECS: u64 = 30;

/// The rate-limit-retry policy this backend hands to the shared
/// [`send_with_retry`](crate::util::http::send_with_retry).
const RETRY_POLICY: crate::util::http::RetryPolicy = crate::util::http::RetryPolicy {
    pacing: REQUEST_PACING,
    max_retries: RATE_LIMIT_RETRIES,
    default_retry_after: Duration::from_secs(DEFAULT_RETRY_AFTER_SECS),
    label: "solidarity tech",
};

/// Query-parameter name for filtering `GET /users` by email address.
///
/// The published API description omitted the filter param names, so confirm
/// this against a live read before trusting an empty result: an unknown param
/// is typically ignored, which returns *everything* rather than erroring.
const Q_EMAIL: &str = "email";
/// Query-parameter name for filtering `GET /users` by phone number.
///
/// Note the value is `phone_number`, not `phone`. Same VERIFY caveat as
/// [`Q_EMAIL`]: an unrecognized param is silently ignored.
const Q_PHONE: &str = "phone_number";
/// Query-parameter name for filtering `GET /users` to one or more user lists
/// (comma-separated ids). Confirmed against the live Solidarity Tech API
/// reference (`GET /users`), unlike the email/phone params it sits beside.
const Q_USER_LIST_IDS: &str = "user_list_ids";

/// Live [`SolidarityTechClient`] backed by a `reqwest` client.
///
/// The bearer token is held as a [`SecretString`] and is only ever exposed
/// inside `send_with_retry` when building the `Authorization` header, so it
/// never lands in logs or `Debug` output. `base_url` is fixed to the public API
/// in production and pointed at a mock server by [`with_base_url`] in tests.
///
/// [`SecretString`]: secrecy::SecretString
/// [`with_base_url`]: SolidarityTechHttp::with_base_url
pub struct SolidarityTechHttp {
    base_url: String,
    token: SecretString,
    client: reqwest::Client,
}

impl SolidarityTechHttp {
    /// The real Solidarity Tech API base URL: the default when no
    /// `SOLIDARITY_TECH_BASE_URL` override is set, and the URL the live suite pins
    /// (it must reach prod regardless of any override set for the mock).
    pub const API_BASE_URL: &'static str = "https://api.solidarity.tech/v1";

    /// Builds the client from the `SOLIDARITY_TECH_TOKEN` environment variable.
    ///
    /// Returns [`SolidarityTechError::MissingEnv`] if the variable is unset, or
    /// [`SolidarityTechError::Http`] if the shared HTTP client fails to build.
    pub async fn from_env() -> Result<Self, SolidarityTechError> {
        let token = SecretString::from(
            crate::util::secret::from_credstore_or_env(
                "solidarity_tech_token",
                "SOLIDARITY_TECH_TOKEN",
            )
            .ok_or(SolidarityTechError::MissingEnv("SOLIDARITY_TECH_TOKEN"))?,
        );
        let client = crate::util::http::default_client()?;
        Ok(Self {
            // Defaults to the public API; `SOLIDARITY_TECH_BASE_URL` overrides it so a
            // divorced staging instance can point at a mock (see `crate::util::base_url`).
            base_url: crate::util::base_url("SOLIDARITY_TECH_BASE_URL", Self::API_BASE_URL),
            token,
            client,
        })
    }

    /// Construct a `SolidarityTechHttp` pointing at an arbitrary base URL.
    /// Used by integration tests against a `wiremock` server.
    pub fn with_base_url(base_url: String, token: SecretString) -> Self {
        let client = crate::util::http::default_client().expect("test http client builds");
        Self {
            base_url,
            token,
            client,
        }
    }

    /// Sends `req` under the ~2 req/s [`RETRY_POLICY`] via the shared
    /// [`send_with_retry`](crate::util::http::send_with_retry): a leading pace
    /// plus `Retry-After`-honoring 429 retries. Auth is already applied to `req`
    /// by the caller.
    async fn send_with_retry(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, SolidarityTechError> {
        crate::util::http::send_with_retry(
            req,
            &RETRY_POLICY,
            SolidarityTechError::RateLimitExhausted,
        )
        .await
    }

    /// One page of `GET /users`, optionally filtered to the given user-list ids
    /// (comma-separated). `user_list_ids = None` is the unfiltered collection.
    /// `lenient` skips a member whose custom property doesn't decode (with a warning)
    /// instead of failing the page; both whole-roster sweeps pass `true` so one bad
    /// record never aborts the run.
    async fn users_page(
        &self,
        cursor: Option<&str>,
        user_list_ids: Option<&str>,
        lenient: bool,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError> {
        // `cursor` is the `_offset` to request, as a string; `None` is the first
        // page (offset 0).
        let offset: u32 = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let url = format!("{}/users", self.base_url);
        let mut query: Vec<(&str, String)> = vec![
            ("_limit", ST_PAGE_SIZE.to_string()),
            ("_offset", offset.to_string()),
        ];
        if let Some(ids) = user_list_ids {
            query.push((Q_USER_LIST_IDS, ids.to_string()));
        }
        let req = self
            .client
            .get(&url)
            .bearer_auth(self.token.expose_secret())
            .query(&query);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }

        let list: UsersListResponse = resp.json().await?;
        let total = list.meta.as_ref().map(|m| u64::from(m.total_count));
        // `scanned` is the raw page size before any decode-skip filtering.
        let scanned = list.data.len() as u64;
        let members = decode_members(list.data, lenient)?;

        // A short/empty page ends the read; the `_offset >= total` check is a
        // backstop so a misread page param can never loop forever.
        let advanced = offset.saturating_add(ST_PAGE_SIZE);
        let next = if (scanned as usize) < ST_PAGE_SIZE as usize
            || total.is_some_and(|t| u64::from(advanced) >= t)
        {
            None
        } else {
            Some(advanced.to_string())
        };
        Ok(MemberPage {
            members,
            scanned,
            total,
            next,
        })
    }
}

#[async_trait]
impl SolidarityTechClient for SolidarityTechHttp {
    async fn find_members(
        &self,
        email: Option<&Email>,
        phone: Option<&Phone>,
    ) -> Result<Vec<SolidarityTechMember>, SolidarityTechError> {
        if email.is_none() && phone.is_none() {
            return Err(SolidarityTechError::NoQueryCriteria);
        }

        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(e) = email {
            query.push((Q_EMAIL, e.as_str().to_string()));
        }
        if let Some(p) = phone {
            query.push((Q_PHONE, p.as_str().to_string()));
        }
        query.push(("_limit", ST_PAGE_SIZE.to_string()));

        let url = format!("{}/users", self.base_url);
        let req = self
            .client
            .get(&url)
            .bearer_auth(self.token.expose_secret())
            .query(&query);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }

        let list: UsersListResponse = resp.json().await?;
        if let Some(meta) = &list.meta
            && (meta.total_count as usize) > list.data.len()
        {
            tracing::warn!(
                total_count = meta.total_count,
                returned = list.data.len(),
                "solidarity tech find_members: more matches than returned; only first page used"
            );
        }

        let mut members = Vec::with_capacity(list.data.len());
        for r in list.data {
            match SolidarityTechMember::try_from(r) {
                Ok(m) => members.push(m),
                // A record we can't even project (e.g. no parseable email) is
                // skipped, preserving the prior tolerant behavior.
                Err(e @ SolidarityTechError::MalformedMember(_)) => {
                    tracing::warn!(error = %e, "skipping malformed solidarity tech member")
                }
                // A bad custom-property value (dues status, membership type, or
                // membership status) is a data problem worth surfacing, not a
                // member to silently drop.
                Err(e) => return Err(e),
            }
        }
        Ok(members)
    }

    async fn members_page(
        &self,
        cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError> {
        // Lenient: a single undecodable member (e.g. a retired status tier) is skipped
        // with a warning, not allowed to abort the whole roster sweep.
        self.users_page(cursor, None, true).await
    }

    async fn members_in_list_page(
        &self,
        list_id: &str,
        cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError> {
        self.users_page(cursor, Some(list_id), true).await
    }

    async fn set_discord_handle(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError> {
        if dry_run.is_dry() {
            tracing::info!(
                member_id,
                handle = handle.as_str(),
                "dry-run: solidarity tech set_discord_handle"
            );
            return Ok(());
        }

        let url = format!("{}/users/{}", self.base_url, member_id);
        let body = UserUpdate {
            custom_user_properties: StWriteProps {
                discord_handle: Some(handle.as_str().to_string()),
                ..StWriteProps::default()
            },
        };
        let req = self
            .client
            .put(&url)
            .bearer_auth(self.token.expose_secret())
            .json(&body);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }
        Ok(())
    }

    async fn set_alternate_email(
        &self,
        member_id: &str,
        alternate_email: &Email,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError> {
        if dry_run.is_dry() {
            tracing::info!(
                member_id,
                alternate_email = alternate_email.as_str(),
                "dry-run: solidarity tech set_alternate_email"
            );
            return Ok(());
        }

        let url = format!("{}/users/{}", self.base_url, member_id);
        let body = UserUpdate {
            custom_user_properties: StWriteProps {
                alternate_email: Some(alternate_email.as_str().to_string()),
                ..StWriteProps::default()
            },
        };
        let req = self
            .client
            .put(&url)
            .bearer_auth(self.token.expose_secret())
            .json(&body);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }
        Ok(())
    }

    async fn set_discord_identity(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
        id: DiscordUserId,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError> {
        if dry_run.is_dry() {
            tracing::info!(
                member_id,
                handle = handle.as_str(),
                discord_user_id = %id,
                "dry-run: solidarity tech set_discord_identity"
            );
            return Ok(());
        }

        let url = format!("{}/users/{}", self.base_url, member_id);
        let body = UserUpdate {
            custom_user_properties: StWriteProps {
                discord_handle: Some(handle.as_str().to_string()),
                discord_user_id: Some(id.to_string()),
                ..StWriteProps::default()
            },
        };
        let req = self
            .client
            .put(&url)
            .bearer_auth(self.token.expose_secret())
            .json(&body);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }
        Ok(())
    }

    async fn clear_discord_identity(
        &self,
        member_id: &str,
        flags: StClearFlags,
        dry_run: DryRun,
    ) -> Result<(), SolidarityTechError> {
        if !flags.handle && !flags.user_id {
            tracing::debug!(
                member_id,
                "clear_discord_identity: nothing requested, nothing to do"
            );
            return Ok(());
        }
        if dry_run.is_dry() {
            tracing::info!(
                member_id,
                clear_handle = flags.handle,
                clear_user_id = flags.user_id,
                "dry-run: solidarity tech clear_discord_identity"
            );
            return Ok(());
        }

        // `then` yields `Some("")` only for the requested keys; the others stay
        // `None` and are skipped on serialize, so the PUT merge leaves them alone.
        let url = format!("{}/users/{}", self.base_url, member_id);
        let body = UserUpdate {
            custom_user_properties: StWriteProps {
                discord_handle: flags.handle.then(String::new),
                discord_user_id: flags.user_id.then(String::new),
                ..StWriteProps::default()
            },
        };
        let req = self
            .client
            .put(&url)
            .bearer_auth(self.token.expose_secret())
            .json(&body);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }
        Ok(())
    }

    async fn list_custom_user_properties(
        &self,
    ) -> Result<Vec<CustomUserProperty>, SolidarityTechError> {
        let url = format!("{}/custom_user_properties", self.base_url);
        let req = self
            .client
            .get(&url)
            .bearer_auth(self.token.expose_secret())
            .query(&[("_limit", ST_PAGE_SIZE.to_string())]);

        let resp = self.send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SolidarityTechError::Status { status, body });
        }

        let list: CustomPropsListResponse = resp.json().await?;
        Ok(list.data)
    }
}
