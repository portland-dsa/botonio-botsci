//! The Solidarity Tech backend's error type.

use domain::MigsStatusError;

/// Everything that can go wrong in a [`SolidarityTechClient`](super::SolidarityTechClient) call.
#[derive(Debug, thiserror::Error)]
pub enum SolidarityTechError {
    /// A transport-level failure from `reqwest` (connection, timeout, TLS).
    #[error("solidarity tech http error: {0}")]
    Http(#[from] reqwest::Error),
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
