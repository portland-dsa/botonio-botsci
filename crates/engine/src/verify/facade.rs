//! Member access traits and error type for the verify engine.

use crate::store::MemberRecord;
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use domain::Role;

use super::decision::{HealAction, Located};

/// Everything a member facade operation can fail with - the one concrete error the verbs
/// surface. Each backend's associated error is stringified once, inside [`DataStore`], rather
/// than at every call in the verbs; the typed variants stay intact so the bot can match the
/// same generic, PII-free cases.
#[derive(Debug, thiserror::Error)]
pub enum MemberError {
    #[error("cache read/write failed: {0}")]
    Store(String),
    #[error("discord write failed: {0}")]
    Discord(String),
    #[error("solidarity tech failed: {0}")]
    SolidarityTech(String),
    #[error("override write failed: {0}")]
    Override(String),
    #[error("audit write failed: {0}")]
    Audit(String),
}

/// Member-oriented reads that hide which backend answers. The read half of the facade;
/// siblings with [`MemberWrite`] (a writer need not read - see [`Heal`] for the fusion).
#[async_trait::async_trait]
pub trait MemberRead: Send + Sync {
    /// Read a member id-first then handle, as a [`Located`] (the read [`locate`](super::decision::locate) performs).
    async fn lookup(
        &self,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<Located, MemberError>;

    /// The Solidarity Tech records a membership email resolves to, projected to [`MemberRecord`]s.
    async fn find_by_email(&self, email: &Email) -> Result<Vec<MemberRecord>, MemberError>;

    /// The member's currently-held managed status roles.
    async fn held_roles(&self, id: DiscordUserId) -> Result<Vec<Role>, MemberError>;

    /// Whether `id` has a moderator grace override active today. Read through the facade so
    /// every verify verb honors grace without each re-reading the store directly.
    async fn active_grace(&self, id: DiscordUserId) -> Result<bool, MemberError>;

    /// Whether `id` carries an active manual-override stamp. Read through the facade so the
    /// verify verbs hold a hand-approved member at Member without each re-reading the store.
    async fn active_override(&self, id: DiscordUserId) -> Result<bool, MemberError>;
}

/// Member-oriented writes. Independent of [`MemberRead`] because not every writer reads (an
/// override grant writes without ever reading the member). The identity write-back primitives
/// ([`push_identity`](MemberWrite::push_identity), [`link_cache`](MemberWrite::link_cache)) are
/// the pieces [`Heal::self_heal`] composes.
#[async_trait::async_trait]
pub trait MemberWrite: Send + Sync {
    /// Set the member's status role to exactly `role`, stripping any other managed role.
    async fn assign_role(&self, id: DiscordUserId, role: Role) -> Result<(), MemberError>;

    /// Remove every role in `roles` from the member.
    async fn strip_roles(&self, id: DiscordUserId, roles: &[Role]) -> Result<(), MemberError>;

    /// Clear the member's cached Discord identity, returning them to an unlinked state.
    async fn unlink(&self, id: DiscordUserId) -> Result<(), MemberError>;

    /// Stamp that `target` was hand-approved by `approver`, with an optional `note`. Insert-once.
    async fn stamp_override(
        &self,
        target: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), MemberError>;

    /// Remove `target`'s override stamp (the reset path; fails closed where the grant is withheld).
    async fn delete_override(&self, target: DiscordUserId) -> Result<(), MemberError>;

    /// Add the additive Manual Override marker role.
    async fn set_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError>;

    /// Remove the Manual Override marker role.
    async fn clear_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError>;

    /// Append one audited action by `actor` upon `subject`.
    async fn record(
        &self,
        actor: DiscordUserId,
        subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), MemberError>;

    /// Push the discovered Discord identity to Solidarity Tech (the source of truth): an
    /// [`UpdateHandle`](HealAction::UpdateHandle) refreshes the handle, a
    /// [`BackfillId`](HealAction::BackfillId) sets the full identity. A composition primitive of
    /// [`Heal::self_heal`].
    async fn push_identity(
        &self,
        st: &StUserId,
        heal: &HealAction,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError>;

    /// Write the discovered Discord identity through to the cache. A composition primitive of
    /// [`Heal::self_heal`].
    async fn link_cache(
        &self,
        st: &StUserId,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError>;
}

/// The combiner for operations that read a record and then write a repair. Its
/// [`self_heal`](Heal::self_heal) is a defaulted method: push the discovered identity to the
/// source, then write it through to the cache. Best-effort is the *verb's* concern - `self_heal`
/// surfaces the error and the verb logs and discards it, so the best-effort-ness reads as a gate
/// at the call site rather than being hidden here.
#[async_trait::async_trait]
pub trait Heal: MemberRead + MemberWrite {
    /// Repair the stored identity link a successful match implies: Solidarity Tech first, then
    /// the cache. A [`None`](HealAction::None) heal is a no-op. The cache write runs only if the
    /// source write succeeded (the `?` short-circuits), preserving the existing ordering.
    async fn self_heal(
        &self,
        record: &MemberRecord,
        id: DiscordUserId,
        handle: &DiscordHandle,
        heal: &HealAction,
    ) -> Result<(), MemberError> {
        if matches!(heal, HealAction::None) {
            return Ok(());
        }
        self.push_identity(&record.st_user_id, heal, handle).await?;
        self.link_cache(&record.st_user_id, id, handle).await?;
        Ok(())
    }
}
