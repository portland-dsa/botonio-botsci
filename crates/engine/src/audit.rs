//! The audit-log capability: record one privileged member action in an
//! append-only log.
//!
//! Defined here, alongside [`MemberStore`](crate::store::MemberStore), rather than
//! in the bot, so the Postgres-backed implementation in `persistence` (which sits
//! below the bot in the crate layering) can implement it. The trait takes the
//! *raw* Discord ids and leaves hashing to the implementation, so no caller can
//! accidentally persist a member id in the clear.

use async_trait::async_trait;

use crate::util::DiscordUserId;

/// Append a single audited action to a durable, append-only log.
///
/// The implementation is responsible for turning `actor` and `subject` into
/// non-reversible tokens (the production one HMACs them) and for the write itself;
/// callers only describe *what happened*. `detail` carries non-PII context - for a
/// card lookup, the outcome - and is stored verbatim.
///
/// `action` is a short, stable verb (e.g. `"card_lookup"`) shared by every row of a
/// kind, so the log can be filtered without parsing `detail`.
#[async_trait]
pub trait AuditLog: Send + Sync {
    /// How a write can fail. The production implementation surfaces its database
    /// error here; a test double that cannot fail uses [`std::convert::Infallible`].
    type Error: std::error::Error + Send + Sync + 'static;

    /// Record one action by `actor` upon `subject`.
    async fn record(
        &self,
        actor: DiscordUserId,
        subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::Mutex;

    /// A recording double proving the trait is usable and object-friendly.
    #[derive(Default)]
    struct Recorder {
        rows: Mutex<Vec<(DiscordUserId, DiscordUserId, String, serde_json::Value)>>,
    }

    #[async_trait]
    impl AuditLog for Recorder {
        type Error = Infallible;
        async fn record(
            &self,
            actor: DiscordUserId,
            subject: DiscordUserId,
            action: &str,
            detail: serde_json::Value,
        ) -> Result<(), Infallible> {
            self.rows
                .lock()
                .unwrap()
                .push((actor, subject, action.to_owned(), detail));
            Ok(())
        }
    }

    #[tokio::test]
    async fn records_one_row() {
        let rec = Recorder::default();
        rec.record(
            DiscordUserId(1),
            DiscordUserId(2),
            "card_lookup",
            serde_json::json!({ "outcome": "found" }),
        )
        .await
        .unwrap();
        let rows = rec.rows.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, "card_lookup");
    }
}
