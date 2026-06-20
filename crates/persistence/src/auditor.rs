//! The Postgres-backed [`AuditLog`]: HMAC the member ids and append one
//! `audit_log` row.
//!
//! Kept separate from [`PgStore`](crate::PgStore) so the audit secret and the
//! hashing concern do not mingle with the cache store, even though both ride the
//! same bot-owned pool and the same DML runtime role. The `audit_log` grant is
//! INSERT-only by design, so this type only ever appends.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use sqlx::PgPool;

use engine::audit::AuditLog;
use engine::util::DiscordUserId;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::PersistenceError;

/// Appends HMAC-hashed, append-only audit rows over the bot-owned pool.
///
/// Hashing the actor and subject ids keeps the audit table itself free of member
/// PII: a row is attributable only to whoever holds the keyed secret. The `key_id`
/// stored beside each row names the key that produced it, so the key can be rotated
/// without orphaning old rows.
pub struct Auditor {
    pool: PgPool,
    key: SecretString,
    key_id: String,
}

impl Auditor {
    /// Build over an existing pool. The bot owns the one pool and shares a clone
    /// here (`PgPool` is internally an `Arc`, so the clone is cheap).
    pub fn new(pool: PgPool, key: SecretString, key_id: String) -> Self {
        Self { pool, key, key_id }
    }
}

/// HMAC-SHA256 a Discord id under `key` and hex-encode it. A free function (not a
/// method) so the hashing is unit-testable without a pool: same id and key always
/// produce the same lowercase-hex string, and the id never appears in the clear.
fn hash_id(key: &[u8], id: DiscordUserId) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&id.0.to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[async_trait]
impl AuditLog for Auditor {
    type Error = PersistenceError;

    async fn record(
        &self,
        actor: DiscordUserId,
        subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), PersistenceError> {
        let actor_hash = hash_id(self.key.expose_secret().as_bytes(), actor);
        let subject_hash = hash_id(self.key.expose_secret().as_bytes(), subject);
        sqlx::query!(
            r#"INSERT INTO audit_log (actor_hash, subject_hash, action, detail, key_id)
               VALUES ($1, $2, $3, $4, $5)"#,
            actor_hash,
            subject_hash,
            action,
            detail,
            self.key_id.as_str(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_non_pii_hex() {
        let a = hash_id(b"secret-key", DiscordUserId(42));
        // Same id + key -> identical hash.
        assert_eq!(a, hash_id(b"secret-key", DiscordUserId(42)));
        // SHA-256 is 32 bytes -> 64 lowercase-hex chars; the raw id never appears.
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // The id is hashed as raw big-endian bytes, so assert the hex encoding of
        // those bytes does not leak into the digest (the decimal form is never an
        // input). This is the meaningful no-cleartext-id property.
        assert!(!a.contains(&format!("{:016x}", 42u64)));
        // A different key or id changes the hash.
        assert_ne!(a, hash_id(b"other-key", DiscordUserId(42)));
        assert_ne!(a, hash_id(b"secret-key", DiscordUserId(43)));
    }
}
