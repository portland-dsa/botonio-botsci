//! Moving a member's Discord link onto the Solidarity Tech record under a different email -
//! the repair for a member who signed up again and so has a second record and a second
//! Solidarity Tech id.

use domain::{MigsStatus, Role};

use crate::store::MemberRecord;
use crate::util::{DiscordUserId, Email};
use crate::verify::decision::{EmailMatchOutcome, HealAction, Located, match_by_email};
use crate::verify::facade::{Heal, MemberError};

use super::{Member, VerifyMethod, effective_role, verify_detail};

/// What [`relink_by_email`](Member::relink_by_email) did.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum RelinkOutcome {
    /// The member is linked to the record under the new email and holds the role it earns.
    /// `standing` is that record's raw membership status.
    Relinked {
        role: Role,
        standing: Option<MigsStatus>,
    },
    /// The member is linked to the new record, but it has no usable membership status, so no
    /// role was assigned.
    Malformed,
    /// The email already resolves to the record the member is linked to; nothing changed.
    AlreadyLinked,
    /// The email is on record for a different Discord account, or for several records none
    /// of which is this member's; nothing changed.
    Conflict,
    /// No Solidarity Tech record carries the email; nothing changed.
    NotFound,
}

impl<M: Heal> Member<'_, M> {
    /// Move this member's Discord link onto the Solidarity Tech record under `email`, on behalf
    /// of moderator `invoker`, then assign the role that record earns.
    ///
    /// The new record is decided by [`match_by_email`], so one already bound to a different
    /// account bounces.
    pub async fn relink_by_email(
        &self,
        invoker: DiscordUserId,
        email: Email,
    ) -> Result<RelinkOutcome, MemberError> {
        let records = self.store.find_by_email(&email).await?;
        let new = match match_by_email(records, self.target.id, &self.target.handle) {
            EmailMatchOutcome::Matched { record, .. } => record,
            EmailMatchOutcome::Conflict => {
                self.record_relink(invoker, "conflict", None, None, false)
                    .await?;
                return Ok(RelinkOutcome::Conflict);
            }
            EmailMatchOutcome::Miss => {
                self.record_relink(invoker, "not_found", None, None, false)
                    .await?;
                return Ok(RelinkOutcome::NotFound);
            }
        };

        // The record the cache links to this member by id is the one the link moves off. A
        // handle-only hit is not this member's link, so there is nothing to clear.
        let old = match self
            .store
            .lookup(self.target.id, &self.target.handle)
            .await?
        {
            Located::ById(old) if old.st_user_id == new.st_user_id => {
                self.record_relink(invoker, "already_linked", None, new.standing, false)
                    .await?;
                return Ok(RelinkOutcome::AlreadyLinked);
            }
            Located::ById(old) => Some(old),
            Located::ByHandle(_) | Located::Unknown => None,
        };

        // As in `verify`, an active manual override holds the member at Member. `None` is a
        // record with no usable standing: the link still moves, but no role is assigned.
        let role = if self.store.active_override(self.target.id).await? {
            Some(Role::Member)
        } else {
            effective_role(&new, self.store.hold(&new, self.target.id).await?).ok()
        };
        let outcome = match role {
            Some(_) => "relinked",
            None => "malformed",
        };
        self.record_relink(invoker, outcome, role, new.standing, old.is_some())
            .await?;

        // Write the full identity even when the new record already carries it: the old
        // record is about to lose its copy, so this one must hold it for certain.
        self.store
            .push_identity(
                &new.st_user_id,
                &HealAction::BackfillId(self.target.id),
                &self.target.handle,
            )
            .await?;
        if let Some(old) = &old {
            self.store.clear_identity(&old.st_user_id).await?;
        }
        self.relink_cache(&new).await;

        match role {
            Some(role) => {
                self.assign_or_record_failure(invoker, role, new.standing, VerifyMethod::Email)
                    .await?;
                Ok(RelinkOutcome::Relinked {
                    role,
                    standing: new.standing,
                })
            }
            None => Ok(RelinkOutcome::Malformed),
        }
    }

    /// Point the cache at `new`: unlink every row holding this member's id, add `new` if the
    /// cache does not hold it yet, then link it. Without the add, a record created since the
    /// last refresh stays invisible, and a scan before the next refresh would demote the
    /// member as unknown.
    async fn relink_cache(&self, new: &MemberRecord) {
        let result = async {
            self.store.unlink(self.target.id).await?;
            self.store.cache_member(new).await?;
            self.store
                .link_cache(&new.st_user_id, self.target.id, &self.target.handle)
                .await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                "relink: cache update failed; Solidarity Tech holds the new link and the next refresh repairs the cache"
            );
        }
    }

    /// Append the `member_relink` audit row: the verify-family detail with the email method,
    /// plus whether a previous link was moved off another record. Never the email itself.
    async fn record_relink(
        &self,
        invoker: DiscordUserId,
        outcome: &str,
        role: Option<Role>,
        standing: Option<MigsStatus>,
        moved: bool,
    ) -> Result<(), MemberError> {
        let mut detail = verify_detail(outcome, role, standing, VerifyMethod::Email);
        detail["previous_link_cleared"] = serde_json::Value::from(moved);
        self.store
            .record(invoker, self.target.id, "member_relink", detail)
            .await
    }
}
