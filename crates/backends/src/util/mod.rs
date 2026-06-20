//! Backend-internal helpers.
//!
//! The shared `reqwest` client builder ([`http`]) and the serde deserialize
//! helper ([`nonempty_string`]) live here. The id newtypes are re-exported from
//! `domain` so the backend modules keep referring to them as `crate::util::...`,
//! unchanged from before the workspace split.

pub mod http;
pub mod secret;
pub mod serde_de;

pub use domain::ids::{
    DiscordChannelId, DiscordGuildId, DiscordHandle, DiscordUserId, Email, Phone, StUserId,
};
pub use serde_de::nonempty_string;
pub use serde_de::select_label;

/// The base URL for a backend HTTP client: the `<BACKEND>_BASE_URL` environment
/// override when it is set to a non-empty value, otherwise `default`.
///
/// The override is the seam a **divorced** staging instance uses to point a backend at a
/// mock server (set via its `systemd` drop-in), so staging reads no real member records;
/// production sets no such variable and falls through to the real API URL. A blank value
/// is ignored, so an accidentally empty `Environment=..._BASE_URL=` can't break the client.
///
/// Read here in [`crate::Clients::from_env`]'s per-backend constructors, the one place a
/// backend touches the environment.
pub fn base_url(env_var: &str, default: &str) -> String {
    match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::base_url;

    #[test]
    // Targeted allow: this test sets/removes a uniquely-named env var (safe in a
    // single-threaded test). The crate root denies unsafe under test, forbids elsewhere.
    #[allow(unsafe_code)]
    fn prefers_a_non_empty_override() {
        // SAFETY: single-threaded test; a unique var name avoids cross-test races.
        unsafe { std::env::set_var("TEST_BASE_URL_OVERRIDE_A", "http://127.0.0.1:9") };
        assert_eq!(
            base_url("TEST_BASE_URL_OVERRIDE_A", "https://real"),
            "http://127.0.0.1:9"
        );
        unsafe { std::env::remove_var("TEST_BASE_URL_OVERRIDE_A") };
    }

    #[test]
    // Same env-var rationale as `prefers_a_non_empty_override`.
    #[allow(unsafe_code)]
    fn falls_back_when_unset_or_blank() {
        unsafe { std::env::remove_var("TEST_BASE_URL_OVERRIDE_B") };
        assert_eq!(
            base_url("TEST_BASE_URL_OVERRIDE_B", "https://real"),
            "https://real"
        );
        // A blank override is ignored rather than producing an unusable empty base URL.
        unsafe { std::env::set_var("TEST_BASE_URL_OVERRIDE_B", "   ") };
        assert_eq!(
            base_url("TEST_BASE_URL_OVERRIDE_B", "https://real"),
            "https://real"
        );
        unsafe { std::env::remove_var("TEST_BASE_URL_OVERRIDE_B") };
    }
}
