//! Solidarity Tech backend: read members for verification, and stamp Discord
//! identity onto their profiles.
//!
//! Reads go through `GET /users` filtered by email or phone; writes go through
//! `PUT /users/{id}`. Because the Discord fields are *org-level* custom user
//! properties, that PUT is a safe merge - only the keys in the body change and
//! every other property is left untouched, so this backend can stamp identity
//! without round-tripping the rest of a member's record.
//!
//! The Solidarity Tech API allows 60 requests per 30 seconds (~2 req/s), so
//! [`SolidarityTechHttp`] paces every request with a leading sleep and retries a
//! few times on HTTP 429, honoring `Retry-After`.
//!
//! Custom-property keys are the `#[serde(rename)]` strings on the `wire` module's
//! `StReadProps`/`StWriteProps` structs. The `x-date`, `membership-type`,
//! `membership-status`, and `discord-handle` keys are confirmed against the live
//! org; the `discord-user-id` and dues-status keys are still marked VERIFY -
//! confirm the internal key via the
//! [`list_custom_user_properties`](SolidarityTechClient::list_custom_user_properties)
//! read and a live filtered read before trusting a production write, since a wrong
//! key fails silently.

mod client;
mod error;
pub mod fixtures;
mod http;
mod member;
mod status;
mod wire;

pub use client::{SolidarityTechClient, StClearFlags};
pub use error::SolidarityTechError;
pub use http::SolidarityTechHttp;
pub use member::{CustomUserProperty, SolidarityTechMember};
pub use status::{DuesStatus, MembershipType};

#[cfg(feature = "fakes")]
mod fakes;

#[cfg(feature = "fakes")]
pub use fakes::FakeSolidarityTech;
