//! The Solidarity Tech backend's error type.

use domain::MigsStatusError;

/// Everything that can go wrong in a [`SolidarityTechClient`](super::SolidarityTechClient) call.
#[derive(Debug, thiserror::Error)]
pub enum SolidarityTechError {
    /// A transport-level failure from `reqwest` (connection, timeout, TLS). The message
    /// is a failure-kind description, never `reqwest`'s own `Display`: that embeds the
    /// request URL, which for a `find_members` request carries the member's email (PII).
    #[error("solidarity tech http error: {0}")]
    Http(String),
    /// The API returned a non-2xx status; `body` is the raw response text, kept
    /// for diagnostics (it carries no token).
    #[error("solidarity tech returned status {status}: {body}")]
    Status { status: u16, body: String },
    /// Every retry attempt still saw HTTP 429; the value is the number of
    /// attempts made.
    #[error("solidarity tech rate limit exhausted after {0} retries")]
    RateLimitExhausted(u32),
    /// [`find_members`] was called with neither an email nor a phone. Returned
    /// before any request is sent, guarding against an unfiltered full scan.
    ///
    /// [`find_members`]: super::SolidarityTechClient::find_members
    #[error("find_members requires at least one of email/phone")]
    NoQueryCriteria,
    /// A required environment variable was absent at startup; the value names it.
    #[error("missing env var: {0}")]
    MissingEnv(&'static str),
    /// A dues-status custom property held a value this backend doesn't recognize;
    /// the string is the unrecognized value. Surfaced
    /// rather than silently dropped, so a new dues option fails loudly.
    #[error("unrecognized solidarity tech dues status: {0}")]
    UnknownDuesStatus(String),
    /// The `membership-type` custom property held a value this backend doesn't
    /// recognize; the string is the unrecognized value.
    #[error("unrecognized solidarity tech membership type: {0}")]
    UnknownMembershipType(String),
    /// The `membership-status` custom property held a value that is not a live
    /// [`MigsStatus`](domain::MigsStatus) - unrecognized or one of the retired
    /// statuses. Carries the decode error (which retains the offending text).
    #[error("bad solidarity tech membership status: {0}")]
    BadMembershipStanding(#[from] MigsStatusError),
    /// A user record could not be projected (e.g. a missing/unparseable email);
    /// the string explains which field. Non-fatal by convention: `find_members`
    /// skips such a record rather than failing the whole lookup.
    #[error("malformed solidarity tech member: {0}")]
    MalformedMember(String),
}

impl From<reqwest::Error> for SolidarityTechError {
    fn from(e: reqwest::Error) -> Self {
        SolidarityTechError::Http(redact_request_url(&e))
    }
}

/// A PII-safe description of a `reqwest` transport failure.
///
/// `reqwest::Error`'s `Display` embeds the request URL, and a [`find_members`] URL carries
/// the member's email. Report the failure by kind only - never the URL - so a transport
/// error can be logged without leaking the address being looked up.
///
/// [`find_members`]: super::SolidarityTechClient::find_members
fn redact_request_url(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "request timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else if e.is_decode() {
        "could not decode the response".to_string()
    } else {
        "transport failure".to_string()
    }
}
