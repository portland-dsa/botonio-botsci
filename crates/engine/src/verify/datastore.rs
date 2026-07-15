//! Concrete DataStore implementing the member access traits.

use crate::audit::AuditLog;
use crate::backends::discord::DiscordClient;
use crate::backends::discord::roles::MarkerRole;
use crate::backends::solidarity_tech::SolidarityTechClient;
use crate::store::{GraceStore, IdentityWrite, MemberRecord, MemberStore, OverrideLog};
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use domain::{DiscordGuildId, Role};

use super::decision::{HealAction, Located};
use super::facade::{Heal, MemberError, MemberRead, MemberWrite};

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
    guild: DiscordGuildId,
}

impl<'a, St, Dc, S, A> DataStore<'a, St, Dc, S, A> {
    /// Bundle the four backends plus the guild into one facade value.
    pub fn new(
        st: &'a St,
        discord: &'a Dc,
        store: &'a S,
        audit: &'a A,
        guild: DiscordGuildId,
    ) -> Self {
        Self {
            st,
            discord,
            store,
            audit,
            guild,
        }
    }
}

#[async_trait::async_trait]
impl<St, Dc, S, A> MemberRead for DataStore<'_, St, Dc, S, A>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite + OverrideLog + GraceStore,
    A: AuditLog,
{
    async fn lookup(
        &self,
        id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<Located, MemberError> {
        super::decision::locate(self.store, id, handle)
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

    async fn active_grace(&self, id: DiscordUserId) -> Result<bool, MemberError> {
        let today = chrono::Utc::now().date_naive();
        self.store
            .active_grace(self.guild, id, today)
            .await
            .map_err(|e| MemberError::Store(e.to_string()))
    }

    async fn active_override(&self, id: DiscordUserId) -> Result<bool, MemberError> {
        Ok(self
            .store
            .get_override(id)
            .await
            .map_err(|e| MemberError::Store(e.to_string()))?
            .is_some())
    }
}

#[async_trait::async_trait]
impl<St, Dc, S, A> MemberWrite for DataStore<'_, St, Dc, S, A>
where
    St: SolidarityTechClient,
    Dc: DiscordClient,
    S: MemberStore + IdentityWrite + OverrideLog + GraceStore,
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
            .assign_marker_role(id, MarkerRole::ManualOverride)
            .await
            .map_err(|e| MemberError::Discord(e.to_string()))
    }

    async fn clear_override_marker(&self, id: DiscordUserId) -> Result<(), MemberError> {
        self.discord
            .remove_marker_role(id, MarkerRole::ManualOverride)
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
    S: MemberStore + IdentityWrite + OverrideLog + GraceStore,
    A: AuditLog,
{
}

#[cfg(test)]
mod datastore_tests {
    use super::*;
    use crate::backends::discord::{DiscordOp, FakeDiscord};
    use crate::backends::solidarity_tech::{FakeSolidarityTech, SolidarityTechMember};
    use crate::store::{InMemoryStore, Index, MemberRecord};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use crate::verify::{HealAction, MemberError, MemberWrite};
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
        let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));

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
        let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));

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
        let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));

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
        let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));

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
        let ds = DataStore::new(&st, &discord, &store, &audit, DiscordGuildId(1));

        let err = ds
            .assign_role(DiscordUserId(7), Role::Member)
            .await
            .unwrap_err();
        assert!(matches!(err, MemberError::Discord(_)));
    }
}
