//! Member handle and verify/resync outcome types.

use crate::store::MemberRecord;
use crate::util::{DiscordHandle, DiscordUserId, Email};
use domain::{MalformedMembership, Role};

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
    /// Matched but the record carries no usable standing; no role written, queued for
    /// the wizard's override-only path.
    Malformed,
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

/// The outcome of an email verify, carrying the matched record on success so a
/// caller can render details the role alone cannot (the self-service log post and
/// its name double-check). [`verify_by_email`](Member::verify_by_email) maps this
/// down to a [`VerifyOutcome`] for callers that do not need the record.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum EmailGrant {
    /// Matched and the role was assigned; the matched record is returned.
    Verified { role: Role, record: MemberRecord },
    /// Matched a record that carries no usable standing - no role was assigned.
    Malformed,
    /// The email belongs to a different account, or to several records none of
    /// which is already this account's - nothing was changed.
    Conflict,
    /// No Solidarity Tech record carries this email.
    NotFound,
}

/// How long past its expiry date a `Lapsed` record is still treated as current.
///
/// Seven days covers the observed two-day processing window plus a weekend and a retry,
/// while keeping a genuinely lapsed member's unearned grace to under a week.
#[cfg(feature = "lapse-grace")]
const PAYMENT_PROCESSING_DAYS: i64 = 7;

/// Whether `record` sits inside the window where a `Lapsed` standing is more likely an
/// upstream payment-processing artifact than a real lapse.
///
/// Requires both a `Lapsed` standing and an `expires` date to anchor against: a `Lapsed`
/// record with no expiry has nothing to measure, so it decides on standing alone exactly as
/// it did before this workaround existed. A *future* expiry yields a negative day count and
/// so is covered too - a record claiming to be lapsed before its own expiry date is
/// self-contradictory, which is the shape the defect would take if the upstream data shifts
/// again.
#[cfg(feature = "lapse-grace")]
fn payment_processing(record: &MemberRecord, today: chrono::NaiveDate) -> bool {
    if record.standing != Some(domain::MigsStatus::Lapsed) {
        return false;
    }
    let Some(xdate) = record.expires else {
        return false;
    };
    (today - xdate).num_days() <= PAYMENT_PROCESSING_DAYS
}

/// Why a member is held at [`Role::Member`] despite what Solidarity Tech says about them.
///
/// A hold is the *reason* a role decision departs from the record's standing, so a caller can
/// report which one applied rather than only that one did - the scheduled scan counts
/// [`PaymentProcessing`](Hold::PaymentProcessing) holds so the upstream defect's lifetime
/// stays visible without reading logs on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// No hold - the record's standing decides the role.
    None,
    /// A moderator granted an explicit grace window through `/grace set`.
    Moderator,
    /// The record is `Lapsed` but still inside the payment-processing window.
    ///
    /// Temporary: this variant and every `#[cfg]` beside it go when the upstream defect is
    /// fixed.
    #[cfg(feature = "lapse-grace")]
    PaymentProcessing,
}

/// The hold in force for `record` on `today`.
///
/// `moderator_grace` is the caller's already-read [`GraceStore`](crate::store::GraceStore)
/// answer, and it wins: a hand-granted window is never mistaken for the automatic one, so
/// `/grace set` and `/grace clear` behave exactly as a moderator expects even while the
/// temporary workaround is live.
pub fn hold_for(record: &MemberRecord, moderator_grace: bool, today: chrono::NaiveDate) -> Hold {
    if moderator_grace {
        return Hold::Moderator;
    }
    #[cfg(feature = "lapse-grace")]
    if payment_processing(record, today) {
        return Hold::PaymentProcessing;
    }
    #[cfg(not(feature = "lapse-grace"))]
    let _ = (record, today);
    Hold::None
}

/// The role to assign for a matched record, with any [`Hold`] applied: a held member is forced
/// to `Member` (ignoring `expires` and membership status); otherwise the role is the
/// standing-derived one. A [`MalformedMembership`] error means a matched record with no usable
/// standing and no hold - the "malformed" branch the callers already handle.
pub fn effective_role(record: &MemberRecord, hold: Hold) -> Result<Role, MalformedMembership> {
    match hold {
        Hold::Moderator => return Ok(Role::Member),
        #[cfg(feature = "lapse-grace")]
        Hold::PaymentProcessing => return Ok(Role::Member),
        Hold::None => {}
    }
    Role::try_from(record.membership())
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

#[cfg(all(test, feature = "lapse-grace"))]
mod payment_processing_tests {
    use super::*;
    use crate::util::{Email, StUserId};
    use chrono::NaiveDate;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 10).expect("valid date")
    }

    /// A record with `standing` whose expiry sits `days_ago` days before [`today`].
    fn record(standing: Option<domain::MigsStatus>, days_ago: i64) -> MemberRecord {
        MemberRecord {
            st_user_id: StUserId("st-1".into()),
            discord_user_id: None,
            discord_handle: None,
            email: Email("m@b.test".into()),
            full_name: None,
            standing,
            join_date: None,
            expires: Some(today() - chrono::Duration::days(days_ago)),
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }
    }

    #[test]
    fn the_window_is_inclusive_at_its_edge() {
        let lapsed = |days| record(Some(domain::MigsStatus::Lapsed), days);
        assert!(payment_processing(&lapsed(7), today()), "7 days is inside");
        assert!(
            !payment_processing(&lapsed(8), today()),
            "8 days is outside"
        );
    }

    #[test]
    fn a_future_expiry_is_covered() {
        // Lapsed before its own expiry date is self-contradictory - the shape the defect
        // would take if the upstream data shifts again.
        let rec = record(Some(domain::MigsStatus::Lapsed), -30);
        assert!(payment_processing(&rec, today()));
    }

    #[test]
    fn an_absent_expiry_has_no_anchor() {
        let mut rec = record(Some(domain::MigsStatus::Lapsed), 1);
        rec.expires = None;
        assert!(!payment_processing(&rec, today()));
    }

    #[test]
    fn only_a_lapsed_standing_qualifies() {
        assert!(!payment_processing(
            &record(Some(domain::MigsStatus::MemberInGoodStanding), 1),
            today()
        ));
        assert!(!payment_processing(&record(None, 1), today()));
    }

    #[test]
    fn moderator_grace_outranks_the_window() {
        // Both apply; the hold must name the moderator so the scan does not count a
        // hand-granted window as an upstream artifact.
        let rec = record(Some(domain::MigsStatus::Lapsed), 1);
        assert_eq!(hold_for(&rec, true, today()), Hold::Moderator);
        assert_eq!(hold_for(&rec, false, today()), Hold::PaymentProcessing);
    }

    #[test]
    fn a_held_record_resolves_to_member() {
        let rec = record(Some(domain::MigsStatus::Lapsed), 1);
        assert_eq!(
            effective_role(&rec, hold_for(&rec, false, today())),
            Ok(Role::Member)
        );
        // Outside the window the standing decides, as it always has.
        let stale = record(Some(domain::MigsStatus::Lapsed), 30);
        assert_eq!(
            effective_role(&stale, hold_for(&stale, false, today())),
            Ok(Role::DuesExpired)
        );
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
        // A hand-approved member is held at Member regardless of Solidarity Tech - the manual
        // override is a standing vouch that outranks the ST decision, the same way an active
        // grace forces Member, but it applies even when ST has no record at all, so neither a
        // miss nor a lapsed/malformed record can demote them. (Also re-grants Member to anyone a
        // prior scan demoted before this guard existed.)
        if self.store.active_override(self.target.id).await? {
            self.store
                .record(
                    invoker,
                    self.target.id,
                    "member_verify",
                    verify_detail("verified", Some(Role::Member), VerifyMethod::Override),
                )
                .await?;
            self.assign_or_record_failure(invoker, Role::Member, VerifyMethod::Override)
                .await?;
            return Ok(VerifyOutcome::Verified(Role::Member));
        }
        let located = self
            .store
            .lookup(self.target.id, &self.target.handle)
            .await?;
        match decide(located, self.target.id, &self.target.handle) {
            MatchOutcome::Matched { record, heal } => {
                let hold = self.store.hold(&record, self.target.id).await?;
                match effective_role(&record, hold) {
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
                }
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
        // As in `verify`, an active manual override holds the member at Member regardless of
        // Solidarity Tech. Honour the resync skip-no-op contract: already at Member means no
        // write and no audit.
        if self.store.active_override(self.target.id).await? {
            if crate::bulk::already_in_role(held, Role::Member) {
                return Ok(ResyncOutcome::Unchanged(Role::Member));
            }
            self.store
                .record(
                    invoker,
                    self.target.id,
                    "member_verify",
                    verify_detail("verified", Some(Role::Member), VerifyMethod::Override),
                )
                .await?;
            self.assign_or_record_failure(invoker, Role::Member, VerifyMethod::Override)
                .await?;
            return Ok(ResyncOutcome::Changed(Role::Member));
        }
        let located = self
            .store
            .lookup(self.target.id, &self.target.handle)
            .await?;
        match decide(located, self.target.id, &self.target.handle) {
            MatchOutcome::Matched { record, heal } => {
                let hold = self.store.hold(&record, self.target.id).await?;
                match effective_role(&record, hold) {
                    Ok(role) => {
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
                    Err(_) => {
                        // Matched a record with no usable standing: audit the encounter and queue
                        // for the wizard's override-only path. No role is written.
                        self.store
                            .record(
                                invoker,
                                self.target.id,
                                "member_verify",
                                verify_detail("malformed", None, VerifyMethod::Discord),
                            )
                            .await?;
                        Ok(ResyncOutcome::Malformed)
                    }
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

    /// Shared core of the two email-verify entry points: read Solidarity Tech by
    /// `email`, decide via [`match_by_email`], grant on a clean match, and audit every
    /// outcome with `method: "email"` (never the email itself).
    async fn email_grant(
        &self,
        invoker: DiscordUserId,
        email: Email,
    ) -> Result<EmailGrant, MemberError> {
        let records = self.store.find_by_email(&email).await?;
        Ok(
            match match_by_email(records, self.target.id, &self.target.handle) {
                EmailMatchOutcome::Matched { record, heal } => {
                    let hold = self.store.hold(&record, self.target.id).await?;
                    match effective_role(&record, hold) {
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
                            EmailGrant::Verified { role, record }
                        }
                        Err(_) => {
                            // Matched a record with no usable standing: assign no role, audit the
                            // encounter, and let the caller offer a hand-override.
                            self.store
                                .record(
                                    invoker,
                                    self.target.id,
                                    "member_verify",
                                    verify_detail("malformed", None, VerifyMethod::Email),
                                )
                                .await?;
                            EmailGrant::Malformed
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
                    EmailGrant::Conflict
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
                    EmailGrant::NotFound
                }
            },
        )
    }

    /// Verify from a moderator-supplied membership `email`, the manual fallback when
    /// the automatic id/handle match misses. Reads Solidarity Tech live by email,
    /// decides via [`match_by_email`], and otherwise mirrors [`verify`](Member::verify).
    /// Every outcome is audited with `method: "email"`, never the email itself.
    pub async fn verify_by_email(
        &self,
        invoker: DiscordUserId,
        email: Email,
    ) -> Result<VerifyOutcome, MemberError> {
        Ok(match self.email_grant(invoker, email).await? {
            EmailGrant::Verified { role, .. } => VerifyOutcome::Verified(role),
            EmailGrant::Malformed => VerifyOutcome::Malformed,
            EmailGrant::Conflict => VerifyOutcome::Conflict,
            EmailGrant::NotFound => VerifyOutcome::NotFound,
        })
    }

    /// As [`verify_by_email`](Member::verify_by_email), but returns the matched
    /// [`MemberRecord`] on success so the caller can render details that the role
    /// alone cannot carry (the self-service verification log and its name check). The
    /// decision and the writes are identical; only what is surfaced differs.
    pub async fn verify_by_email_with_record(
        &self,
        invoker: DiscordUserId,
        email: Email,
    ) -> Result<EmailGrant, MemberError> {
        self.email_grant(invoker, email).await
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
