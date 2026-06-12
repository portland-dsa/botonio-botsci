//! Shared `GET /users` response builders and a single-user decode helper.
//!
//! These live beside the wire types so the one definition of "what a Solidarity
//! Tech `/users` body looks like" is reused by both the offline contract suite
//! and the standalone mock server (`mock-st`) - they cannot drift apart, and a
//! wire-shape change breaks one place. [`decode_user`] runs a fabricated user
//! object through the real [`SolidarityTechMember`] decode, so a fixture that
//! would mis-decode fails loudly.

use serde_json::{Value, json};

use super::SolidarityTechError;
use super::member::SolidarityTechMember;
use super::wire::UserResponse;

/// Build one `/users` user object. `email` is `None` to emit a `null` email (a
/// member the strict decode rejects as malformed).
pub fn user_json(id: u64, email: Option<&str>, custom_props: Value) -> Value {
    json!({
        "id": id,
        "email": email,
        "phone_number": null,
        "custom_user_properties": custom_props,
    })
}

/// Wrap `users` in the paginated list envelope the client reads, with the
/// `meta` counters a page response carries.
pub fn users_page(users: Vec<Value>, total_count: usize, limit: u32, offset: u32) -> Value {
    json!({
        "data": users,
        "meta": { "total_count": total_count, "limit": limit, "offset": offset }
    })
}

/// Decode one `/users` user object through the real backend decode, exactly as a
/// live read would. The JSON must be well-formed for the wire shape (it is, by
/// construction in fixtures); a decode *rule* failure (malformed email, retired
/// status) is returned as the same [`SolidarityTechError`] a live read yields.
pub fn decode_user(value: &Value) -> Result<SolidarityTechMember, SolidarityTechError> {
    let resp: UserResponse =
        serde_json::from_value(value.clone()).expect("fixture user JSON is well-formed");
    SolidarityTechMember::try_from(resp)
}
