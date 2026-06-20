//! [`Auditor`] writes one well-formed `audit_log` row: hashed ids as hex, the
//! action, the JSON detail, and the key id.
//!
//! Gated behind `live-db` because it needs a loopback Postgres; run it from a shell
//! that can bind loopback (see `deploy/test-infra/`). A plain
//! `cargo test -p persistence` compiles this file to nothing.
#![cfg(feature = "live-db")]

use engine::audit::AuditLog;
use engine::util::DiscordUserId;
use persistence::Auditor;
use secrecy::SecretString;

#[sqlx::test(migrations = "./migrations")]
async fn record_writes_one_hashed_row(pool: sqlx::PgPool) {
    let auditor = Auditor::new(
        pool.clone(),
        SecretString::from("test-hmac-key".to_string()),
        "v1".to_owned(),
    );

    auditor
        .record(
            DiscordUserId(111),
            DiscordUserId(222),
            "card_lookup",
            serde_json::json!({ "outcome": "found" }),
        )
        .await
        .unwrap();

    let row =
        sqlx::query!(r#"SELECT actor_hash, subject_hash, action, detail, key_id FROM audit_log"#)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(row.action, "card_lookup");
    assert_eq!(row.key_id, "v1");
    assert_eq!(row.detail, serde_json::json!({ "outcome": "found" }));
    // Ids are stored hashed, never in the clear.
    assert_eq!(row.actor_hash.len(), 64);
    assert_ne!(row.actor_hash, row.subject_hash);
    assert!(!row.actor_hash.contains("111"));
    assert!(!row.subject_hash.contains("222"));
}
