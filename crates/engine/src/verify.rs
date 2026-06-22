//! The moderator verify-and-assign use case: match a Discord member to their
//! Solidarity Tech record, repair the stored identity link, and assign the role their
//! standing earns.
//!
//! [`locate`] reads id-first then handle; [`decide`] is the pure decision over where
//! the record was found, with a guard that never re-links a record already bound to a
//! different account. [`verify`] is the orchestrator that executes a decision against
//! the backends, the cache, and the audit log.

use domain::Role;

use crate::audit::AuditLog;
use crate::backends::discord::DiscordClient;
use crate::backends::solidarity_tech::SolidarityTechClient;
use crate::store::{IdentityWrite, MemberRecord, MemberStore, OverrideLog};
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};

/// The identity repair a successful match implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealAction {
    /// Matched by id; the stored handle drifted and is updated to the current one.
    UpdateHandle(DiscordHandle),
    /// Matched by handle and the record had no id; backfill it.
    BackfillId(DiscordUserId),
    /// The stored identity already agrees; nothing to write back.
    None,
}

/// What [`decide`] decided.
///
/// `#[must_use]`: discarding this throws away the conflict guard and the miss decision,
/// silently running an assignment against an unresolved member, so a caller that drops it
/// is a compile error rather than a security hole.
#[derive(Debug)]
#[must_use]
pub enum MatchOutcome {
    /// A clean match, with the repair (if any) to apply.
    Matched {
        record: MemberRecord,
        heal: HealAction,
    },
    /// The handle resolves to a record already linked to a different account.
    Conflict,
    /// Solidarity Tech does not know this member by id or handle.
    Miss,
}

/// What [`match_by_email`] decided from the records a membership email resolved to.
///
/// `#[must_use]` for the same reason as [`MatchOutcome`]: dropping it would discard the
/// conflict guard and silently run an assignment against an unresolved member.
#[derive(Debug)]
#[must_use]
pub enum EmailMatchOutcome {
    /// One record is safe to claim for this Discord account, with the repair to apply.
    Matched {
        record: MemberRecord,
        heal: HealAction,
    },
    /// The email belongs to a record bound to a different account, or to several
    /// records none of which is already this account's - resolve by hand.
    Conflict,
    /// No Solidarity Tech record carries this email.
    Miss,
}

/// The [`HealAction`] a successful match implies, from the stored record and the
/// member's current identity. A record that already carries a Discord id only ever
/// needs its handle refreshed; one with no id is backfilled. Shared by
/// [`decide`] (id/handle path) and [`match_by_email`] (email path) so both agree.
fn heal_for(
    record: &MemberRecord,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> HealAction {
    match record.discord_user_id {
        Some(_) => {
            if record.discord_handle.as_ref() == Some(target_handle) {
                HealAction::None
            } else {
                HealAction::UpdateHandle(target_handle.clone())
            }
        }
        None => HealAction::BackfillId(target),
    }
}

/// Where a cache lookup found a member's record - the input to [`decide`], replacing the
/// `(Option, Option)` pair the verbs used to thread into the old read-and-match block.
///
/// [`ById`](Located::ById) is the authoritative hit on the immutable key; [`ByHandle`](Located::ByHandle)
/// is the repair-or-conflict branch reached only after an id miss; [`Unknown`](Located::Unknown)
/// is a miss by both. `#[must_use]`: dropping it discards the decision a verify must act on.
#[derive(Debug)]
#[must_use]
pub enum Located {
    /// Found by Discord id - authoritative; at most the stored handle drifted.
    ById(MemberRecord),
    /// Found by handle after the id missed - a backfill, or a conflict if the record
    /// already carries a different id.
    ByHandle(MemberRecord),
    /// No record by id or handle.
    Unknown,
}

/// Decide the verification outcome from where the record was found.
///
/// An [`ById`](Located::ById) hit is authoritative and repairs only a drifted handle. An
/// [`ByHandle`](Located::ByHandle) hit either backfills a record that has no id yet or - if an
/// id is already present, which (since the id lookup missed) must be a *different* account's -
/// is a [`Conflict`](MatchOutcome::Conflict) the caller must not resolve automatically. The
/// conflict guard is the security boundary against handle recycling.
pub fn decide(
    found: Located,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> MatchOutcome {
    match found {
        Located::ById(record) => {
            let heal = heal_for(&record, target, target_handle);
            MatchOutcome::Matched { record, heal }
        }
        Located::ByHandle(record) => match record.discord_user_id {
            None => {
                let heal = heal_for(&record, target, target_handle);
                MatchOutcome::Matched { record, heal }
            }
            // An id is present and necessarily differs from `target` (an equal id would have
            // been found by the id lookup), so the handle points at another account's record.
            Some(_) => MatchOutcome::Conflict,
        },
        Located::Unknown => MatchOutcome::Miss,
    }
}

/// Read a member by id, then by handle on a miss, into a [`Located`]. The single definition of
/// the id-first / handle-fallback read the verify path and the bulk preview share; the id hit
/// wins, so the handle is read only when the id lookup misses.
pub async fn locate<S: MemberStore>(
    store: &S,
    id: DiscordUserId,
    handle: &DiscordHandle,
) -> Result<Located, S::Error> {
    if let Some(record) = store.by_discord_id(id).await? {
        return Ok(Located::ById(record));
    }
    Ok(match store.by_handle(handle).await? {
        Some(record) => Located::ByHandle(record),
        None => Located::Unknown,
    })
}

/// Decide which (if any) of the records a typed email resolved to may be claimed for
/// `target`.
///
/// A unique email yields zero or one row in practice. Zero is a [`Miss`]. One is a clean
/// [`Matched`] when its stored Discord id is empty or already `target`'s, and a
/// [`Conflict`] when it is some other account's. More than one is a [`Matched`] only when
/// exactly one already carries `target`'s id (an idempotent re-verify); otherwise it is a
/// [`Conflict`], because the code never guesses which of several records to bind.
///
/// This is the email key's form of Part 1's impersonation guard: never bind an account to
/// a record that already belongs to a different one.
///
/// [`Miss`]: EmailMatchOutcome::Miss
/// [`Matched`]: EmailMatchOutcome::Matched
/// [`Conflict`]: EmailMatchOutcome::Conflict
pub fn match_by_email(
    matches: Vec<MemberRecord>,
    target: DiscordUserId,
    target_handle: &DiscordHandle,
) -> EmailMatchOutcome {
    // A record already bound to this account wins outright, at any match count.
    if let Some(record) = matches
        .iter()
        .find(|r| r.discord_user_id == Some(target))
        .cloned()
    {
        let heal = heal_for(&record, target, target_handle);
        return EmailMatchOutcome::Matched { record, heal };
    }
    match matches.len() {
        0 => EmailMatchOutcome::Miss,
        1 => {
            let record = matches.into_iter().next().expect("len checked");
            match record.discord_user_id {
                None => {
                    let heal = heal_for(&record, target, target_handle);
                    EmailMatchOutcome::Matched { record, heal }
                }
                Some(_) => EmailMatchOutcome::Conflict,
            }
        }
        _ => EmailMatchOutcome::Conflict,
    }
}

/// What a verification resolved to - the moderator-facing result.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Matched; the member was assigned this standing-derived role.
    Verified(Role),
    /// No record found; the member was assigned `Unverified`.
    Unverified,
    /// A manual email lookup found no record; nothing was changed (the member already
    /// holds `Unverified` from the automatic miss that opened the manual flow).
    NotFound,
    /// The handle is on record for a different account; nothing was changed.
    Conflict,
}

/// What a bulk resync did to one member - finer-grained than [`VerifyOutcome`] so the
/// sweep can tally the real role changes apart from the members it left untouched.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum ResyncOutcome {
    /// Matched and the role was applied (a Discord write happened).
    Changed(Role),
    /// Matched and already holding exactly this role: no audit, no role write - only the
    /// best-effort identity heal ran.
    Unchanged(Role),
    /// Unknown to Solidarity Tech; left for the wizard. `Unverified` was assigned unless
    /// the member already held exactly it, in which case nothing was written.
    Miss,
    /// Handle bound to another account; nothing was changed.
    Conflict,
}

/// Why a verification could not complete. The two store/audit failures are stringified
/// (their concrete types are the store's and audit log's associated errors); the role
/// write keeps its own message. Each maps to a generic, PII-free reply at the bot.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("cache read failed: {0}")]
    Store(String),
    #[error("audit write failed: {0}")]
    Audit(String),
    #[error("discord role write failed: {0}")]
    Discord(String),
    #[error("solidarity tech read failed: {0}")]
    SolidarityTech(String),
    #[error("override stamp write failed: {0}")]
    Override(String),
}

/// Everything a member facade operation can fail with - the one concrete error the verbs
/// surface. Each backend's associated error is stringified once, inside [`DataStore`], rather
/// than at every call in the verbs; the typed variants stay intact so the bot can match the
/// same generic, PII-free cases. Its shape is exactly [`VerifyError`]'s, which folds into it.
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
    /// Read a member id-first then handle, as a [`Located`] (the read [`locate`] performs).
    async fn lookup(
        &self,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<Located, MemberError>;

    /// The Solidarity Tech records a membership email resolves to, projected to [`MemberRecord`]s.
    async fn find_by_email(&self, email: &Email) -> Result<Vec<MemberRecord>, MemberError>;

    /// The member's currently-held managed status roles.
    async fn held_roles(&self, id: DiscordUserId) -> Result<Vec<Role>, MemberError>;
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
        id: DiscordUserId,
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
        self.push_identity(&record.st_user_id, heal, id, handle)
            .await?;
        self.link_cache(&record.st_user_id, id, handle).await?;
        Ok(())
    }
}

/// The production [`MemberRead`] / [`MemberWrite`] / [`Heal`] implementor: holds borrows of the
/// four backends, stringifies each backend's error into [`MemberError`] in one place, and owns
/// the choreography the facade hides - the strip-stale-roles dance in
/// [`assign_role`](MemberWrite::assign_role) and the per-[`HealAction`] source write in
/// [`push_identity`](MemberWrite::push_identity). Generic over the backend traits (not the
/// concrete `Http` types) so the same code runs over the fakes in tests; production pins the
/// type parameters at the bot call site.
pub struct DataStore<'a, St, Dc, S, A> {
    st: &'a St,
    discord: &'a Dc,
    store: &'a S,
    audit: &'a A,
}

impl<'a, St, Dc, S, A> DataStore<'a, St, Dc, S, A> {
    /// Bundle the four backends into one facade value.
    pub fn new(st: &'a St, discord: &'a Dc, store: &'a S, audit: &'a A) -> Self {
        Self {
            st,
            discord,
            store,
            audit,
        }
    }
}

#[async_trait::async_trait]
impl<St, Dc, S, A> MemberRead for DataStore<'_, St, Dc, S, A>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite + OverrideLog,
    A: AuditLog,
{
    async fn lookup(
        &self,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<Located, MemberError> {
        locate(self.store, id, handle)
            .await
            .map_err(|e| MemberError::Store(e.to_string()))
    }

    async fn find_by_email(&self, email: &Email) -> Result<Vec<MemberRecord>, MemberError> {
        let members = self
            .st
            .find_by_email(email)
            .await
            .map_err(|e| MemberError::SolidarityTech(e.to_string()))?;
        Ok(members.into_iter().map(MemberRecord::from).collect())
    }

    async fn held_roles(&self, id: DiscordUserId) -> Result<Vec<Role>, MemberError> {
        Ok(self
            .discord
            .member_roles(id)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))?
            .held)
    }
}

#[async_trait::async_trait]
impl<St, Dc, S, A> MemberWrite for DataStore<'_, St, Dc, S, A>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite + OverrideLog,
    A: AuditLog,
{
    async fn assign_role(&self, id: DiscordUserId, role: Role) -> Result<(), MemberError> {
        // Set the status role to exactly `role`: add it and strip every other managed role.
        // `set_role` removes only the single role handed to it as `current`, so drive it with
        // one stale role (stripped in the same call as the add) and remove any further stale
        // roles after. See the original assign_role for the full rationale.
        let held = self
            .discord
            .member_roles(id)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))?
            .held;
        let stale: Vec<Role> = held.iter().copied().filter(|&r| r != role).collect();
        let current = stale
            .first()
            .copied()
            .or_else(|| held.contains(&role).then_some(role));
        self.discord
            .set_role(id, current, role)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))?;
        if stale.len() > 1 {
            self.discord
                .remove_roles(id, &stale[1..])
                .await
                .map_err(|e| MemberError::Discord(e.to_string()))?;
        }
        Ok(())
    }

    async fn strip_roles(&self, id: DiscordUserId, roles: &[Role]) -> Result<(), MemberError> {
        if roles.is_empty() {
            return Ok(());
        }
        self.discord
            .remove_roles(id, roles)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))
    }

    async fn unlink(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.store
            .unlink_by_discord_id(id)
            .await
            .map_err(|e| MemberError::Store(e.to_string()))
    }

    async fn stamp_override(
        &self,
        target: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), MemberError> {
        self.store
            .stamp_override(target, approver, note)
            .await
            .map_err(|e| MemberError::Override(e.to_string()))
    }

    async fn delete_override(&self, target: DiscordUserId) -> Result<(), MemberError> {
        self.store
            .delete_override(target)
            .await
            .map_err(|e| MemberError::Override(e.to_string()))
    }

    async fn set_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.discord
            .assign_override_marker(id)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))
    }

    async fn clear_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.discord
            .remove_override_marker(id)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))
    }

    async fn record(
        &self,
        actor: DiscordUserId,
        subject: DiscordUserId,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), MemberError> {
        self.audit
            .record(actor, subject, action, detail)
            .await
            .map_err(|e| MemberError::Audit(e.to_string()))
    }

    async fn push_identity(
        &self,
        st: &StUserId,
        heal: &HealAction,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError> {
        let st_id = st.as_str();
        let result = match heal {
            HealAction::UpdateHandle(h) => self.st.set_discord_handle(st_id, h).await,
            HealAction::BackfillId(backfill_id) => {
                self.st
                    .set_discord_identity(st_id, handle, *backfill_id)
                    .await
            }
            HealAction::None => return Ok(()),
        };
        let _ = id; // BackfillId carries the id to write; UpdateHandle needs only the handle.
        result.map_err(|e| MemberError::SolidarityTech(e.to_string()))
    }

    async fn link_cache(
        &self,
        st: &StUserId,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), MemberError> {
        self.store
            .link_identity(st, id, handle)
            .await
            .map_err(|e| MemberError::Store(e.to_string()))
    }
}

// Empty impl: `self_heal` uses the trait default. No method bodies, so no `#[async_trait]`.
impl<St, Dc, S, A> Heal for DataStore<'_, St, Dc, S, A>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite + OverrideLog,
    A: AuditLog,
{
}

/// How a verification was initiated, written to the audit row's `method` field so a query
/// or operator can tell the automatic id/handle path ([`verify`]) from the manual email
/// path ([`verify_by_email`]) from the hand-approval path ([`override_approve`]). A closed
/// set, so a call site cannot record a typo'd value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyMethod {
    /// The automatic match on the member's Discord id then handle.
    Discord,
    /// The moderator-supplied membership-email lookup.
    Email,
    /// The moderator's hand approval past Solidarity Tech.
    Override,
}

impl VerifyMethod {
    /// The stable string written to the audit `method` field.
    fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Email => "email",
            Self::Override => "override",
        }
    }
}

/// Verify `target` and assign their role, on behalf of moderator `invoker`.
///
/// Reads the cache by id then handle via [`locate`], decides via [`decide`], records the
/// decided outcome to the audit log *before* any write (so no grant is unattributable;
/// an audit failure refuses the grant), assigns the role, and then repairs the stored
/// identity link in Solidarity Tech and the cache. The self-heal is best-effort: once
/// the role is set, a write-back failure is logged and the next run re-heals, rather
/// than denying the member a role they have earned.
pub async fn verify<St, Dc, S, A>(
    solidarity_tech: &St,
    discord: &Dc,
    store: &S,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    target_handle: DiscordHandle,
) -> Result<VerifyOutcome, VerifyError>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite,
    A: AuditLog,
{
    let located = locate(store, target, &target_handle)
        .await
        .map_err(|e| VerifyError::Store(e.to_string()))?;

    match decide(located, target, &target_handle) {
        MatchOutcome::Matched { record, heal } => {
            let role = record.role();
            record_outcome(
                audit,
                invoker,
                target,
                "verified",
                Some(role),
                VerifyMethod::Discord,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                role,
                VerifyMethod::Discord,
            )
            .await?;
            self_heal(
                solidarity_tech,
                store,
                &record,
                target,
                &target_handle,
                &heal,
            )
            .await;
            Ok(VerifyOutcome::Verified(role))
        }
        MatchOutcome::Miss => {
            record_outcome(
                audit,
                invoker,
                target,
                "unverified",
                Some(Role::Unverified),
                VerifyMethod::Discord,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                Role::Unverified,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(VerifyOutcome::Unverified)
        }
        MatchOutcome::Conflict => {
            // No role is granted, but the attempt is still recorded.
            record_outcome(
                audit,
                invoker,
                target,
                "conflict",
                None,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(VerifyOutcome::Conflict)
        }
    }
}

/// Resync one swept member: like [`verify`], but it skips the work when nothing would
/// change. A matched member already holding exactly their earned role gets no audit row and
/// no role write - only the best-effort identity heal (to refresh a drifted handle). A
/// matched member whose role differs takes the full verify path; an unknown member is
/// assigned `Unverified` (unless they already hold exactly it) and left for the wizard.
///
/// `held` is the member's current managed roles from the roster sweep; it is what lets the
/// no-op be decided without a per-member Discord read. The caller paces only the outcomes
/// that actually wrote ([`ResyncOutcome::Changed`] / a freshly-assigned [`ResyncOutcome::Miss`]).
#[allow(clippy::too_many_arguments)]
pub async fn resync_member<St, Dc, S, A>(
    solidarity_tech: &St,
    discord: &Dc,
    store: &S,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    target_handle: DiscordHandle,
    held: &[Role],
) -> Result<ResyncOutcome, VerifyError>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite,
    A: AuditLog,
{
    let located = locate(store, target, &target_handle)
        .await
        .map_err(|e| VerifyError::Store(e.to_string()))?;

    match decide(located, target, &target_handle) {
        MatchOutcome::Matched { record, heal } => {
            let role = record.role();
            if crate::bulk::already_in_role(held, role) {
                // Already in exactly the right role: no audit, no role write. Still refresh
                // a drifted handle (best-effort, as in `verify`).
                self_heal(
                    solidarity_tech,
                    store,
                    &record,
                    target,
                    &target_handle,
                    &heal,
                )
                .await;
                Ok(ResyncOutcome::Unchanged(role))
            } else {
                record_outcome(
                    audit,
                    invoker,
                    target,
                    "verified",
                    Some(role),
                    VerifyMethod::Discord,
                )
                .await?;
                assign_role_or_record_failure(
                    discord,
                    audit,
                    invoker,
                    target,
                    role,
                    VerifyMethod::Discord,
                )
                .await?;
                self_heal(
                    solidarity_tech,
                    store,
                    &record,
                    target,
                    &target_handle,
                    &heal,
                )
                .await;
                Ok(ResyncOutcome::Changed(role))
            }
        }
        MatchOutcome::Miss => {
            if crate::bulk::already_in_role(held, Role::Unverified) {
                // Already Unverified and still unknown: nothing to write, just leave them
                // for the wizard.
                return Ok(ResyncOutcome::Miss);
            }
            record_outcome(
                audit,
                invoker,
                target,
                "unverified",
                Some(Role::Unverified),
                VerifyMethod::Discord,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                Role::Unverified,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(ResyncOutcome::Miss)
        }
        MatchOutcome::Conflict => {
            record_outcome(
                audit,
                invoker,
                target,
                "conflict",
                None,
                VerifyMethod::Discord,
            )
            .await?;
            Ok(ResyncOutcome::Conflict)
        }
    }
}

/// Append one `member_verify` audit row. The detail is non-identifying: the outcome,
/// method, and (when a role is granted) its name - never the conflicting account or any PII.
async fn record_outcome<A: AuditLog>(
    audit: &A,
    actor: DiscordUserId,
    subject: DiscordUserId,
    outcome: &str,
    role: Option<Role>,
    method: VerifyMethod,
) -> Result<(), VerifyError> {
    let detail = match role {
        Some(r) => {
            serde_json::json!({ "outcome": outcome, "role": r.as_str(), "method": method.as_str() })
        }
        None => serde_json::json!({ "outcome": outcome, "method": method.as_str() }),
    };
    audit
        .record(actor, subject, "member_verify", detail)
        .await
        .map_err(|e| VerifyError::Audit(e.to_string()))
}

/// The member's currently-held managed status roles, lifting a Discord read failure into
/// [`VerifyError`]. Shared by [`assign_role`], which keeps one and strips the rest, and
/// [`forget_member`], which strips them all.
async fn held_managed_roles<Dc: DiscordClient>(
    discord: &Dc,
    target: DiscordUserId,
) -> Result<Vec<Role>, VerifyError> {
    Ok(discord
        .member_roles(target)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?
        .held)
}

/// Set the member's status role to exactly `role`: add it if missing and strip every
/// *other* managed role they hold.
///
/// Reading the member's full set of managed roles (not just one) matters because
/// [`DiscordClient::set_role`] only removes the single role handed to it as `current`. A
/// member who has somehow accumulated two managed roles - a previous assignment whose
/// removal half failed, or a hand-applied role - would otherwise keep the stale extra. So
/// one held role drives `set_role` (stripped in the same call as the add) and any further
/// held roles are removed after. A member already holding exactly `role` is a no-op.
async fn assign_role<Dc: DiscordClient>(
    discord: &Dc,
    target: DiscordUserId,
    role: Role,
) -> Result<(), VerifyError> {
    let held = held_managed_roles(discord, target).await?;
    // Every managed role to strip is everything held except the target itself.
    let stale: Vec<Role> = held.iter().copied().filter(|&r| r != role).collect();
    // Drive set_role's single removal with one stale role so it is stripped in the same
    // call as the add; with nothing stale, name the target only when it is already held,
    // which makes set_role a true no-op rather than a redundant re-add.
    let current = stale
        .first()
        .copied()
        .or_else(|| held.contains(&role).then_some(role));
    discord
        .set_role(target, current, role)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?;
    // Any held managed roles beyond the one set_role already removed.
    if stale.len() > 1 {
        discord
            .remove_roles(target, &stale[1..])
            .await
            .map_err(|e| VerifyError::Discord(e.to_string()))?;
    }
    Ok(())
}

/// Assign the role, and if the Discord write fails, append a `verify_failed` follow-up to
/// the audit log before surfacing the error.
///
/// The `verified`/`unverified` row was written *before* the attempt (audit-before-write,
/// so no grant is ever unattributable). A failed write would otherwise leave the log
/// showing a success that never landed; this reconciling row records that it did not. The
/// follow-up is best-effort - the role write already failed, so there is no granted action
/// left to gate - so its own failure is logged, not surfaced, and the caller still gets the
/// original Discord error.
async fn assign_role_or_record_failure<Dc, A>(
    discord: &Dc,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    role: Role,
    method: VerifyMethod,
) -> Result<(), VerifyError>
where
    Dc: DiscordClient,
    A: AuditLog,
{
    let Err(e) = assign_role(discord, target, role).await else {
        return Ok(());
    };
    if let Err(audit_err) =
        record_outcome(audit, invoker, target, "verify_failed", Some(role), method).await
    {
        tracing::warn!(
            error = %audit_err,
            "verify: could not record the verify_failed follow-up after a failed role write"
        );
    }
    Err(e)
}

/// Write the discovered identity back to Solidarity Tech and then the cache. Best-effort:
/// the role is already granted, so a failure here is logged, not surfaced.
async fn self_heal<St, S>(
    solidarity_tech: &St,
    store: &S,
    record: &MemberRecord,
    target: DiscordUserId,
    handle: &DiscordHandle,
    heal: &HealAction,
) where
    St: SolidarityTechClient,
    S: IdentityWrite,
{
    let st_id = record.st_user_id.as_str();
    let st_result = match heal {
        HealAction::UpdateHandle(h) => solidarity_tech.set_discord_handle(st_id, h).await,
        HealAction::BackfillId(id) => {
            solidarity_tech
                .set_discord_identity(st_id, handle, *id)
                .await
        }
        HealAction::None => return,
    };
    if let Err(e) = st_result {
        tracing::warn!(error = %e, "verify: solidarity tech self-heal failed; role granted, will re-heal");
        return;
    }
    if let Err(e) = store
        .link_identity(&record.st_user_id, target, handle)
        .await
    {
        tracing::warn!(error = %e, "verify: cache write-through failed; role granted, will re-heal");
    }
}

/// Verify `target` from a moderator-supplied membership `email`, the manual fallback when
/// the automatic id/handle match in [`verify`] misses.
///
/// Reads Solidarity Tech live by email (the cache holds no email index), projects the
/// hits, and decides with [`match_by_email`]. A match audits before writing, assigns the
/// standing role, and writes the discovered Discord identity back to Solidarity Tech and
/// the cache (best-effort, as in [`verify`]). A miss writes no role - the member already
/// holds `Unverified` from the automatic miss - and a conflict writes nothing. Every
/// outcome is audited with `method: "email"`, never the email itself.
#[allow(clippy::too_many_arguments)]
pub async fn verify_by_email<St, Dc, S, A>(
    solidarity_tech: &St,
    discord: &Dc,
    store: &S,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    target_handle: DiscordHandle,
    email: Email,
) -> Result<VerifyOutcome, VerifyError>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite,
    A: AuditLog,
{
    let members = solidarity_tech
        .find_by_email(&email)
        .await
        .map_err(|e| VerifyError::SolidarityTech(e.to_string()))?;
    let records: Vec<MemberRecord> = members.into_iter().map(MemberRecord::from).collect();

    match match_by_email(records, target, &target_handle) {
        EmailMatchOutcome::Matched { record, heal } => {
            let role = record.role();
            record_outcome(
                audit,
                invoker,
                target,
                "verified",
                Some(role),
                VerifyMethod::Email,
            )
            .await?;
            assign_role_or_record_failure(
                discord,
                audit,
                invoker,
                target,
                role,
                VerifyMethod::Email,
            )
            .await?;
            self_heal(
                solidarity_tech,
                store,
                &record,
                target,
                &target_handle,
                &heal,
            )
            .await;
            Ok(VerifyOutcome::Verified(role))
        }
        EmailMatchOutcome::Conflict => {
            record_outcome(
                audit,
                invoker,
                target,
                "conflict",
                None,
                VerifyMethod::Email,
            )
            .await?;
            Ok(VerifyOutcome::Conflict)
        }
        EmailMatchOutcome::Miss => {
            record_outcome(
                audit,
                invoker,
                target,
                "not_found",
                None,
                VerifyMethod::Email,
            )
            .await?;
            Ok(VerifyOutcome::NotFound)
        }
    }
}

/// Reset `target` to a pristine, unknown state, undoing the identity and role writes that
/// verify and override make. Records one `member_forget` row first, then strips every held
/// status role and the Manual Override marker, unlinks the cached identity, and deletes the
/// override stamp. Auditing before the writes - and never making the audit log itself a
/// forget target - keeps the erase attributable even if a later write fails.
pub async fn forget_member<Dc, S, O, A>(
    discord: &Dc,
    store: &S,
    override_log: &O,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
) -> Result<(), VerifyError>
where
    Dc: DiscordClient,
    S: IdentityWrite,
    O: OverrideLog,
    A: AuditLog,
{
    let held = held_managed_roles(discord, target).await?;
    // Audit before the destructive writes, the audit-before-write ordering verify and
    // override use, so the erase is always attributable even if a later write fails.
    audit
        .record(
            invoker,
            target,
            "member_forget",
            serde_json::json!({ "roles_stripped": held.len(), "cache_unlinked": true, "stamp_deleted": true }),
        )
        .await
        .map_err(|e| VerifyError::Audit(e.to_string()))?;
    if !held.is_empty() {
        discord
            .remove_roles(target, &held)
            .await
            .map_err(|e| VerifyError::Discord(e.to_string()))?;
    }
    discord
        .remove_override_marker(target)
        .await
        .map_err(|e| VerifyError::Discord(e.to_string()))?;
    store
        .unlink_by_discord_id(target)
        .await
        .map_err(|e| VerifyError::Store(e.to_string()))?;
    override_log
        .delete_override(target)
        .await
        .map_err(|e| VerifyError::Override(e.to_string()))?;
    Ok(())
}

/// Hand-approve `target` past Solidarity Tech, on behalf of moderator `invoker` - the
/// escape hatch when no record can be matched. Grants `Member` and the additive Manual
/// Override marker, and stamps the approval permanently with an optional `note` recording
/// why (stored only on the stamp, never in the audit log).
///
/// Fail-closed twice before the role grant: the audit row, then the override stamp, so the
/// "your approval has been logged" promise holds whenever a role is granted. The stamp is
/// insert-once, so a retry after a later role-write failure is idempotent. The marker is
/// added last and is best-effort: Member is already set and the approval stamped, so a
/// failed marker write is logged and recorded, never surfaced - the status-role logic never
/// strips the marker, and a retry re-adds it.
pub async fn override_approve<Dc, O, A>(
    discord: &Dc,
    override_log: &O,
    audit: &A,
    invoker: DiscordUserId,
    target: DiscordUserId,
    note: Option<String>,
) -> Result<(), VerifyError>
where
    Dc: DiscordClient,
    O: OverrideLog,
    A: AuditLog,
{
    record_outcome(
        audit,
        invoker,
        target,
        "override",
        Some(Role::Member),
        VerifyMethod::Override,
    )
    .await?;
    override_log
        .stamp_override(target, invoker, note)
        .await
        .map_err(|e| VerifyError::Override(e.to_string()))?;
    assign_role_or_record_failure(
        discord,
        audit,
        invoker,
        target,
        Role::Member,
        VerifyMethod::Override,
    )
    .await?;
    if let Err(e) = discord.assign_override_marker(target).await {
        // The marker is the cosmetic, additive half of the grant: Member is already set and
        // the approval stamped, so a failed marker write is logged and recorded, not
        // surfaced - a retry re-adds only the marker (idempotent).
        tracing::warn!(error = %e, "override: marker role write failed; Member granted and stamped, will re-add on retry");
        if let Err(audit_err) = record_outcome(
            audit,
            invoker,
            target,
            "override_marker_failed",
            None,
            VerifyMethod::Override,
        )
        .await
        {
            tracing::warn!(error = %audit_err, "override: could not record the marker-failure follow-up");
        }
    }
    Ok(())
}

#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::store::MemberRecord;
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use domain::MigsStatus;

    fn rec(st: &str, id: Option<u64>, handle: &str) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId(st.into()),
            discord_user_id: id.map(DiscordUserId),
            discord_handle: Some(DiscordHandle(handle.into())),
            email: Email("m@b.test".into()),
            full_name: None,
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    /// The security guard: a handle hit whose record already carries a *different* id
    /// (the id lookup missed, so it cannot be this member's) must never backfill - that
    /// would re-link another account's record. It is a conflict.
    #[test]
    fn handle_match_with_present_other_id_is_a_conflict() {
        let record = MemberRecord {
            st_user_id: StUserId("st-1".into()),
            discord_user_id: Some(DiscordUserId(5)),
            discord_handle: Some(DiscordHandle("rosy".into())),
            email: Email("m@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        };
        let out = decide(
            Located::ByHandle(record),
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, MatchOutcome::Conflict));
    }

    #[test]
    fn id_hit_is_authoritative_and_matches() {
        let out = decide(
            Located::ById(rec("st-1", Some(9), "rosy")),
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, MatchOutcome::Matched { .. }));
    }

    #[test]
    fn email_zero_matches_is_a_miss() {
        let out = match_by_email(vec![], DiscordUserId(9), &DiscordHandle("rosy".into()));
        assert!(matches!(out, EmailMatchOutcome::Miss));
    }

    #[test]
    fn email_single_unlinked_match_backfills_the_id() {
        let out = match_by_email(
            vec![rec("st-1", None, "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        match out {
            EmailMatchOutcome::Matched { heal, .. } => {
                assert_eq!(heal, HealAction::BackfillId(DiscordUserId(9)));
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn email_single_match_already_ours_with_drifted_handle_updates_handle() {
        let out = match_by_email(
            vec![rec("st-1", Some(9), "old")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        match out {
            EmailMatchOutcome::Matched { heal, .. } => {
                assert_eq!(heal, HealAction::UpdateHandle(DiscordHandle("rosy".into())));
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn email_single_match_bound_to_other_account_is_a_conflict() {
        let out = match_by_email(
            vec![rec("st-1", Some(5), "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Conflict));
    }

    #[test]
    fn email_many_matches_none_ours_is_a_conflict() {
        let out = match_by_email(
            vec![rec("st-1", None, "rosy"), rec("st-2", Some(5), "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Conflict));
    }

    #[test]
    fn email_many_matches_one_already_ours_is_matched() {
        let out = match_by_email(
            vec![rec("st-1", Some(9), "rosy"), rec("st-2", None, "rosy")],
            DiscordUserId(9),
            &DiscordHandle("rosy".into()),
        );
        assert!(matches!(out, EmailMatchOutcome::Matched { .. }));
    }
}

#[cfg(test)]
mod resync_tests {
    use super::*;
    use crate::backends::discord::FakeDiscord;
    use crate::backends::solidarity_tech::FakeSolidarityTech;
    use crate::store::{InMemoryStore, Index, MemberRecord};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use domain::MigsStatus;
    use std::convert::Infallible;
    use std::sync::Mutex;

    /// An audit double that just records the action verbs written to it.
    #[derive(Default)]
    struct Recorder {
        actions: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AuditLog for Recorder {
        type Error = Infallible;
        async fn record(
            &self,
            _actor: DiscordUserId,
            _subject: DiscordUserId,
            action: &str,
            _detail: serde_json::Value,
        ) -> Result<(), Infallible> {
            self.actions.lock().unwrap().push(action.to_owned());
            Ok(())
        }
    }

    /// A cached member in good standing (earns `Member`), linked to `id`/`handle`.
    fn member_in_good_standing(id: u64, handle: &str) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId(format!("st-{id}")),
            discord_user_id: Some(DiscordUserId(id)),
            discord_handle: Some(DiscordHandle(handle.into())),
            email: Email(format!("{handle}@b.test")),
            full_name: None,
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    #[tokio::test]
    async fn resync_leaves_an_already_correct_member_untouched() {
        let store = InMemoryStore::new(Index::from_records(vec![member_in_good_standing(
            7, "rosy",
        )]));
        // Seed Rosy holding exactly Member, so an already-correct resync touches neither
        // backend: any role write would change roles_of, any heal would bump writes().
        let discord = FakeDiscord::new().with_roles(DiscordUserId(7), vec![Role::Member]);
        let st = FakeSolidarityTech::new();
        let audit = Recorder::default();

        let outcome = resync_member(
            &st,
            &discord,
            &store,
            &audit,
            DiscordUserId(1),
            DiscordUserId(7),
            DiscordHandle("rosy".into()),
            &[Role::Member],
        )
        .await
        .unwrap();

        assert_eq!(outcome, ResyncOutcome::Unchanged(Role::Member));
        assert!(
            audit.actions.lock().unwrap().is_empty(),
            "an unchanged member must not be audited"
        );
        assert_eq!(
            discord.roles_of(DiscordUserId(7)),
            vec![Role::Member],
            "no role write"
        );
        assert_eq!(st.writes(), 0, "no self-heal write");
    }

    #[tokio::test]
    async fn resync_applies_the_role_when_it_differs() {
        let store = InMemoryStore::new(Index::from_records(vec![member_in_good_standing(
            7, "rosy",
        )]));
        // Rosy holds nothing, so resync moves her to Member.
        let discord = FakeDiscord::new();
        let st = FakeSolidarityTech::new();
        let audit = Recorder::default();

        let outcome = resync_member(
            &st,
            &discord,
            &store,
            &audit,
            DiscordUserId(1),
            DiscordUserId(7),
            DiscordHandle("rosy".into()),
            &[], // holds nothing - a real change to Member
        )
        .await
        .unwrap();

        assert_eq!(outcome, ResyncOutcome::Changed(Role::Member));
        assert_eq!(
            audit.actions.lock().unwrap().as_slice(),
            ["member_verify"],
            "the change is audited once"
        );
        assert!(
            discord.roles_of(DiscordUserId(7)).contains(&Role::Member),
            "role applied"
        );
    }
}

#[cfg(test)]
mod datastore_tests {
    use super::*;
    use crate::backends::discord::{DiscordOp, FakeDiscord};
    use crate::backends::solidarity_tech::{FakeSolidarityTech, SolidarityTechMember};
    use crate::store::{InMemoryStore, Index, MemberRecord};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use domain::MigsStatus;
    use std::convert::Infallible;

    #[derive(Default)]
    struct NoopAudit;
    #[async_trait::async_trait]
    impl AuditLog for NoopAudit {
        type Error = Infallible;
        async fn record(
            &self,
            _actor: DiscordUserId,
            _subject: DiscordUserId,
            _action: &str,
            _detail: serde_json::Value,
        ) -> Result<(), Infallible> {
            Ok(())
        }
    }

    fn linked_record(st: &str, id: u64, handle: &str) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId(st.into()),
            discord_user_id: Some(DiscordUserId(id)),
            discord_handle: Some(DiscordHandle(handle.into())),
            email: Email(format!("{handle}@b.test")),
            full_name: None,
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    #[tokio::test]
    async fn assign_role_strips_every_other_managed_role() {
        // Holds two stale managed roles; assigning Member must leave exactly Member.
        let discord = FakeDiscord::new()
            .with_roles(DiscordUserId(7), vec![Role::Unverified, Role::DuesExpired]);
        let st = FakeSolidarityTech::new();
        let store = InMemoryStore::new(Index::default());
        let audit = NoopAudit;
        let ds = DataStore::new(&st, &discord, &store, &audit);

        ds.assign_role(DiscordUserId(7), Role::Member)
            .await
            .unwrap();

        assert_eq!(discord.roles_of(DiscordUserId(7)), vec![Role::Member]);
    }

    #[tokio::test]
    async fn self_heal_writes_source_then_cache() {
        // A linked member whose handle drifted: self_heal updates the ST handle and writes
        // through to the cache. The default Heal::self_heal composes push_identity + link_cache.
        let record = linked_record("st-7", 7, "rosy");
        let st = FakeSolidarityTech::new().with_members(vec![SolidarityTechMember {
            id: StUserId("st-7".into()),
            email: Email("rosy@b.test".into()),
            discord_handle: Some(DiscordHandle("old".into())),
            discord_user_id: Some(DiscordUserId(7)),
            membership_standing: Some(MigsStatus::MemberInGoodStanding),
            ..Default::default()
        }]);
        let discord = FakeDiscord::new();
        let store = InMemoryStore::new(Index::default());
        let audit = NoopAudit;
        let ds = DataStore::new(&st, &discord, &store, &audit);

        ds.self_heal(
            &record,
            DiscordUserId(7),
            &DiscordHandle("rosy".into()),
            &HealAction::UpdateHandle(DiscordHandle("rosy".into())),
        )
        .await
        .unwrap();

        assert_eq!(st.writes(), 1, "the source handle was written once");
        assert_eq!(
            st.get("st-7").unwrap().discord_handle,
            Some(DiscordHandle("rosy".into())),
        );
    }

    #[tokio::test]
    async fn self_heal_none_writes_nothing() {
        let record = linked_record("st-7", 7, "rosy");
        let st = FakeSolidarityTech::new();
        let discord = FakeDiscord::new();
        let store = InMemoryStore::new(Index::default());
        let audit = NoopAudit;
        let ds = DataStore::new(&st, &discord, &store, &audit);

        ds.self_heal(
            &record,
            DiscordUserId(7),
            &DiscordHandle("rosy".into()),
            &HealAction::None,
        )
        .await
        .unwrap();

        assert_eq!(st.writes(), 0, "a None heal touches no backend");
    }

    #[tokio::test]
    async fn assign_role_surfaces_a_discord_failure_as_member_error() {
        let discord = FakeDiscord::new().failing(DiscordOp::SetRole);
        let st = FakeSolidarityTech::new();
        let store = InMemoryStore::new(Index::default());
        let audit = NoopAudit;
        let ds = DataStore::new(&st, &discord, &store, &audit);

        let err = ds
            .assign_role(DiscordUserId(7), Role::Member)
            .await
            .unwrap_err();
        assert!(matches!(err, MemberError::Discord(_)));
    }
}
