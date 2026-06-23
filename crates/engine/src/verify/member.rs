//! Member handle and verify/resync outcome types.

use crate::store::MemberRecord;
use crate::util::{DiscordHandle, DiscordUserId, Email};
use domain::Role;

use super::decision::{EmailMatchOutcome, HealAction, MatchOutcome, decide, match_by_email};
use super::facade::{Heal, MemberError, MemberRead, MemberWrite};

/// What a verification resolved to - the moderator-facing result.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Matched; the member was assigned this standing-derived role.
    Verified(Role),
    /// No record found; the member was assigned `Unverified`.
    Unverified,
    /// A record matched but carries no usable standing - nothing was assigned; the
    /// moderator is offered a hand-override.
    Malformed,
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

/// What a staging reset did to one member - the finer outcome `/strip-roles` tallies. A
/// previously-overridden member is fully forgotten (the DB stamp and cache link go with the
/// roles); anyone else only loses their managed Discord roles and the override marker.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum StripOutcome {
    /// The member was hand-approved, so the whole reset ran: status roles, the override
    /// marker, the cache identity link, and the override stamp were all cleared (audited).
    Forgotten,
    /// The member was not overridden: their managed status roles and the override marker
    /// were stripped, and nothing in the database was touched.
    Stripped,
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

    /// Reset one swept member for the staging-only `/strip-roles` test affordance.
    ///
    /// An `overridden` member - one carrying a hand-approval stamp - is handed to
    /// [`forget`](Member::forget), so their cache link and stamp are cleared along with their
    /// roles. Everyone else only has their `held` managed status roles stripped and the
    /// override marker removed - a defensive sweep that clears any stale marker without
    /// touching the database, since the marker may have drifted from its stamp in staging.
    /// `held` is the member's current managed roles from the sweep, so this needs no role read
    /// of its own (the `forget` path reads them itself).
    pub async fn strip(
        &self,
        invoker: DiscordUserId,
        overridden: bool,
        held: &[Role],
    ) -> Result<StripOutcome, MemberError> {
        if overridden {
            self.forget(invoker).await?;
            return Ok(StripOutcome::Forgotten);
        }
        self.store.strip_roles(self.target.id, held).await?;
        self.store.clear_override_marker(self.target.id).await?;
        Ok(StripOutcome::Stripped)
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
            MatchOutcome::Matched { record, heal } => match Role::try_from(record.membership()) {
                Ok(role) => {
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
                Err(_) => {
                    // Matched a record with no usable standing: assign no role, audit the encounter,
                    // and let the caller offer a hand-override.
                    self.store
                        .record(
                            invoker,
                            self.target.id,
                            "member_verify",
                            verify_detail("malformed", None, VerifyMethod::Discord),
                        )
                        .await?;
                    Ok(VerifyOutcome::Malformed)
                }
            },
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
                match Role::try_from(record.membership()) {
                    Ok(role) => {
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
                    Err(_) => {
                        // Matched a record with no usable standing: assign no role, audit the encounter,
                        // and let the caller offer a hand-override.
                        self.store
                            .record(
                                invoker,
                                self.target.id,
                                "member_verify",
                                verify_detail("malformed", None, VerifyMethod::Email),
                            )
                            .await?;
                        Ok(VerifyOutcome::Malformed)
                    }
                }
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
mod resync_tests {
    use super::*;
    use crate::audit::AuditLog;
    use crate::backends::discord::FakeDiscord;
    use crate::backends::solidarity_tech::FakeSolidarityTech;
    use crate::store::{InMemoryStore, Index, MemberRecord};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use crate::verify::DataStore;
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
