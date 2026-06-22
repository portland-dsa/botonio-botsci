//! The moderator verify-and-assign use case: match a Discord member to their
//! Solidarity Tech record, repair the stored identity link, and assign the role their
//! standing earns.
//!
//! [`locate`] reads id-first then handle; [`decide`] is the pure decision over where
//! the record was found, with a guard that never re-links a record already bound to a
//! different account. [`Member`] is the facade handle the verbs hang off: build a
//! [`DataStore`] from the four backends, wrap it with [`Member::new`], and call the
//! verb (`verify`, `resync`, `verify_by_email`, `override_approve`, `clear_override`,
//! `forget`). [`MemberError`] is the one concrete error the verbs surface.

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
/// or operator can tell the automatic id/handle path ([`verify`](Member::verify)) from the
/// manual email path ([`verify_by_email`](Member::verify_by_email)) from the hand-approval
/// path ([`override_approve`](Member::override_approve)). A closed
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

/// The non-identifying audit detail for a verify-family outcome: the outcome verb, the method,
/// and (when a role is granted) its name - never PII or the conflicting account.
fn verify_detail(outcome: &str, role: Option<Role>, method: VerifyMethod) -> serde_json::Value {
    match role {
        Some(r) => {
            serde_json::json!({ "outcome": outcome, "role": r.as_str(), "method": method.as_str() })
        }
        None => serde_json::json!({ "outcome": outcome, "method": method.as_str() }),
    }
}

/// A member's identity: the immutable id and the current handle. A *required* pair - every verb
/// starts from a live Discord target, so both are always present (the email path adds an email
/// argument, not an optional identity).
#[derive(Debug, Clone)]
pub struct Target {
    pub id: DiscordUserId,
    pub handle: DiscordHandle,
}

/// A member the verbs operate on: a borrow of the facade plus the target's identity, so a call
/// site reads `Member::new(&store, target).verify(invoker)` rather than re-listing collaborators.
/// The capability-gated `impl` blocks below mean a caller holding a read-only facade cannot reach
/// a write. The acting moderator is not part of `Member` - the member is the subject, the
/// moderator a separate actor - so write methods still take `invoker`.
pub struct Member<'a, M> {
    store: &'a M,
    target: Target,
}

impl<'a, M> Member<'a, M> {
    /// Bind a facade and a target into a handle the verbs hang off.
    pub fn new(store: &'a M, target: Target) -> Self {
        Self { store, target }
    }
}

impl<M: MemberWrite> Member<'_, M> {
    /// Assign `role`, and if the write fails, append a `verify_failed` follow-up to the audit log
    /// (best-effort: the role write already failed, so the follow-up's own failure is logged, not
    /// surfaced) before returning the original error. Shared by every granting verb.
    async fn assign_or_record_failure(
        &self,
        invoker: DiscordUserId,
        role: Role,
        method: VerifyMethod,
    ) -> Result<(), MemberError> {
        let Err(e) = self.store.assign_role(self.target.id, role).await else {
            return Ok(());
        };
        if let Err(audit_err) = self
            .store
            .record(
                invoker,
                self.target.id,
                "member_verify",
                verify_detail("verify_failed", Some(role), method),
            )
            .await
        {
            tracing::warn!(
                error = %audit_err,
                "verify: could not record the verify_failed follow-up after a failed role write"
            );
        }
        Err(e)
    }

    /// Hand-approve the member past Solidarity Tech: grant `Member` plus the additive Manual
    /// Override marker, stamping the approval permanently with an optional `note`. Fail-closed
    /// twice before the grant (audit row, then the stamp), so "your approval has been logged"
    /// holds whenever a role lands; the marker is added last and best-effort.
    pub async fn override_approve(
        &self,
        invoker: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), MemberError> {
        self.store
            .record(
                invoker,
                self.target.id,
                "member_verify",
                verify_detail("override", Some(Role::Member), VerifyMethod::Override),
            )
            .await?;
        self.store
            .stamp_override(self.target.id, invoker, note)
            .await?;
        self.assign_or_record_failure(invoker, Role::Member, VerifyMethod::Override)
            .await?;
        if let Err(e) = self.store.set_override_marker(self.target.id).await {
            tracing::warn!(error = %e, "override: marker role write failed; Member granted and stamped, will re-add on retry");
            if let Err(audit_err) = self
                .store
                .record(
                    invoker,
                    self.target.id,
                    "member_verify",
                    verify_detail("override_marker_failed", None, VerifyMethod::Override),
                )
                .await
            {
                tracing::warn!(error = %audit_err, "override: could not record the marker-failure follow-up");
            }
        }
        Ok(())
    }
}

impl<M: MemberRead + MemberWrite> Member<'_, M> {
    /// Reset the member to a pristine, unknown state: strip every held status role and the
    /// override marker, unlink the cached identity, and delete the override stamp. Records one
    /// `member_forget` row first (audit-before-write), so the erase stays attributable even if a
    /// later write fails.
    pub async fn forget(&self, invoker: DiscordUserId) -> Result<(), MemberError> {
        let held = self.store.held_roles(self.target.id).await?;
        self.store
            .record(
                invoker,
                self.target.id,
                "member_forget",
                serde_json::json!({
                    "roles_stripped": held.len(),
                    "cache_unlinked": true,
                    "stamp_deleted": true
                }),
            )
            .await?;
        self.store.strip_roles(self.target.id, &held).await?;
        self.store.clear_override_marker(self.target.id).await?;
        self.store.unlink(self.target.id).await?;
        self.store.delete_override(self.target.id).await?;
        Ok(())
    }
}

impl<M: Heal> Member<'_, M> {
    /// Verify the member and assign their standing role, on behalf of moderator `invoker`. Reads
    /// id-then-handle, decides via [`decide`], records the decided outcome before any write (an
    /// audit failure refuses the grant), assigns the role, then best-effort repairs the stored
    /// identity link.
    pub async fn verify(&self, invoker: DiscordUserId) -> Result<VerifyOutcome, MemberError> {
        let located = self
            .store
            .lookup(self.target.id, &self.target.handle)
            .await?;
        match decide(located, self.target.id, &self.target.handle) {
            MatchOutcome::Matched { record, heal } => {
                let role = record.role();
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("verified", Some(role), VerifyMethod::Discord),
                    )
                    .await?;
                self.assign_or_record_failure(invoker, role, VerifyMethod::Discord)
                    .await?;
                self.heal(&record, &heal).await;
                Ok(VerifyOutcome::Verified(role))
            }
            MatchOutcome::Miss => {
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("unverified", Some(Role::Unverified), VerifyMethod::Discord),
                    )
                    .await?;
                self.assign_or_record_failure(invoker, Role::Unverified, VerifyMethod::Discord)
                    .await?;
                Ok(VerifyOutcome::Unverified)
            }
            MatchOutcome::Conflict => {
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("conflict", None, VerifyMethod::Discord),
                    )
                    .await?;
                Ok(VerifyOutcome::Conflict)
            }
        }
    }

    /// Resync one swept member: like [`verify`](Member::verify), but skip the work when nothing
    /// would change. `held` is the member's current managed roles from the sweep; an already-
    /// correct member gets no audit and no role write, only the best-effort heal.
    pub async fn resync(
        &self,
        invoker: DiscordUserId,
        held: &[Role],
    ) -> Result<ResyncOutcome, MemberError> {
        let located = self
            .store
            .lookup(self.target.id, &self.target.handle)
            .await?;
        match decide(located, self.target.id, &self.target.handle) {
            MatchOutcome::Matched { record, heal } => {
                let role = record.role();
                if crate::bulk::already_in_role(held, role) {
                    self.heal(&record, &heal).await;
                    Ok(ResyncOutcome::Unchanged(role))
                } else {
                    self.store
                        .record(
                            invoker,
                            self.target.id,
                            "member_verify",
                            verify_detail("verified", Some(role), VerifyMethod::Discord),
                        )
                        .await?;
                    self.assign_or_record_failure(invoker, role, VerifyMethod::Discord)
                        .await?;
                    self.heal(&record, &heal).await;
                    Ok(ResyncOutcome::Changed(role))
                }
            }
            MatchOutcome::Miss => {
                if crate::bulk::already_in_role(held, Role::Unverified) {
                    return Ok(ResyncOutcome::Miss);
                }
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("unverified", Some(Role::Unverified), VerifyMethod::Discord),
                    )
                    .await?;
                self.assign_or_record_failure(invoker, Role::Unverified, VerifyMethod::Discord)
                    .await?;
                Ok(ResyncOutcome::Miss)
            }
            MatchOutcome::Conflict => {
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("conflict", None, VerifyMethod::Discord),
                    )
                    .await?;
                Ok(ResyncOutcome::Conflict)
            }
        }
    }

    /// Verify from a moderator-supplied membership `email`, the manual fallback when the automatic
    /// id/handle match misses. Reads Solidarity Tech live by email, decides via [`match_by_email`],
    /// and otherwise mirrors [`verify`](Member::verify). Every outcome is audited with
    /// `method: "email"`, never the email itself.
    pub async fn verify_by_email(
        &self,
        invoker: DiscordUserId,
        email: Email,
    ) -> Result<VerifyOutcome, MemberError> {
        let records = self.store.find_by_email(&email).await?;
        match match_by_email(records, self.target.id, &self.target.handle) {
            EmailMatchOutcome::Matched { record, heal } => {
                let role = record.role();
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("verified", Some(role), VerifyMethod::Email),
                    )
                    .await?;
                self.assign_or_record_failure(invoker, role, VerifyMethod::Email)
                    .await?;
                self.heal(&record, &heal).await;
                Ok(VerifyOutcome::Verified(role))
            }
            EmailMatchOutcome::Conflict => {
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("conflict", None, VerifyMethod::Email),
                    )
                    .await?;
                Ok(VerifyOutcome::Conflict)
            }
            EmailMatchOutcome::Miss => {
                self.store
                    .record(
                        invoker,
                        self.target.id,
                        "member_verify",
                        verify_detail("not_found", None, VerifyMethod::Email),
                    )
                    .await?;
                Ok(VerifyOutcome::NotFound)
            }
        }
    }

    /// Best-effort identity repair shared by the granting verbs: run `self_heal` and log (never
    /// surface) a failure - the role is already granted, and the next run re-heals. The
    /// best-effort gate lives here at the call site, not inside the facade.
    async fn heal(&self, record: &MemberRecord, heal: &HealAction) {
        if let Err(e) = self
            .store
            .self_heal(record, self.target.id, &self.target.handle, heal)
            .await
        {
            tracing::warn!(error = %e, "verify: self-heal failed; role granted, will re-heal");
        }
    }
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

        let ds = DataStore::new(&st, &discord, &store, &audit);
        let outcome = Member::new(
            &ds,
            Target {
                id: DiscordUserId(7),
                handle: DiscordHandle("rosy".into()),
            },
        )
        .resync(DiscordUserId(1), &[Role::Member])
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

        let ds = DataStore::new(&st, &discord, &store, &audit);
        let outcome = Member::new(
            &ds,
            Target {
                id: DiscordUserId(7),
                handle: DiscordHandle("rosy".into()),
            },
        )
        .resync(DiscordUserId(1), &[])
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
    async fn self_heal_skips_the_cache_when_the_source_push_fails() {
        // The `?` between push_identity and link_cache is the source-of-truth-first guarantee:
        // a failed Solidarity Tech write must skip the cache write entirely, so the cache never
        // disagrees with an unhealed source.
        let record = linked_record("st-7", 7, "rosy");
        let st = FakeSolidarityTech::new()
            .with_members(vec![SolidarityTechMember {
                id: StUserId("st-7".into()),
                email: Email("rosy@b.test".into()),
                discord_handle: Some(DiscordHandle("old".into())),
                discord_user_id: Some(DiscordUserId(7)),
                membership_standing: Some(MigsStatus::MemberInGoodStanding),
                ..Default::default()
            }])
            .failing_writes();
        let discord = FakeDiscord::new();
        // Seed the cache with the stale handle so a stray link_cache would be observable.
        let store = InMemoryStore::new(Index::from_records(vec![linked_record("st-7", 7, "old")]));
        let audit = NoopAudit;
        let ds = DataStore::new(&st, &discord, &store, &audit);

        let err = ds
            .self_heal(
                &record,
                DiscordUserId(7),
                &DiscordHandle("rosy".into()),
                &HealAction::UpdateHandle(DiscordHandle("rosy".into())),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, MemberError::SolidarityTech(_)));
        // link_cache must not have run: the cached handle is still the stale one.
        let cached = store
            .by_discord_id(DiscordUserId(7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            cached.discord_handle,
            Some(DiscordHandle("old".into())),
            "the cache write must be skipped when the source push fails"
        );
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
