//! The reusable member-record store: a flat [`MemberRecord`] and the
//! [`MemberStore`] trait the card resolver reads through.
//!
//! The current implementation ([`InMemoryStore`]) holds the roster in RAM, swept
//! from a Solidarity Tech user list. [`MemberRecord`] is deliberately flat and
//! built from persistence-friendly primitives so a future implementation can back the same
//! [`MemberStore`] trait with a sqlx-mapped Postgres table without changing any caller.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId, MembershipStatus, MigsStatus, Role};

use crate::backends::solidarity_tech::{
    DuesStatus, MembershipType, SolidarityTechClient, SolidarityTechMember,
};
use crate::paging::drain_pages;
use crate::seam::NoProgress;
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};

/// A member projected to the flat shape the card and the future cache share.
/// Every field is a persistence-friendly primitive (`String`,
/// `Option<NaiveDate>`, small text-mapped enums) so a future implementation maps it to one
/// Postgres-backed table with no nesting.
///
/// `PartialEq`/`Eq` let two records be compared whole - the basis of the
/// `PgStore`/`InMemoryStore` conformance test, which asserts a record survives the
/// cache's encode/store/decode round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The Solidarity Tech user id - the stable key, and the target a self-heal
    /// writes the Discord identity back to. Every cached record is sourced from a
    /// Solidarity Tech member, so this is always present.
    pub st_user_id: StUserId,
    pub discord_user_id: Option<DiscordUserId>,
    pub discord_handle: Option<DiscordHandle>,
    pub email: Email,
    pub full_name: Option<String>,
    /// Raw "Membership Status"; the [`Role`] is derived (never stored twice).
    pub standing: Option<MigsStatus>,
    pub join_date: Option<NaiveDate>,
    /// Dues-expiry date (`xdate`).
    pub expires: Option<NaiveDate>,
    pub membership_type: Option<MembershipType>,
    pub monthly_dues: Option<DuesStatus>,
    pub yearly_dues: Option<DuesStatus>,
}

impl MemberRecord {
    /// The Discord [`Role`] this record's standing grants. Absent standing is
    /// `Unverified`, via the shared `MigsStatus -> MembershipStatus -> Role` chain.
    pub fn role(&self) -> Role {
        Role::from(
            self.standing
                .map(MembershipStatus::from)
                .unwrap_or_default(),
        )
    }
}

impl From<SolidarityTechMember> for MemberRecord {
    fn from(m: SolidarityTechMember) -> Self {
        Self {
            st_user_id: m.id,
            discord_user_id: m.discord_user_id,
            discord_handle: m.discord_handle,
            email: m.email,
            full_name: match (m.first_name, m.last_name) {
                (Some(f), Some(l)) => Some(format!("{f} {l}")),
                (Some(f), None) => Some(f),
                (None, Some(l)) => Some(l),
                (None, None) => None,
            },
            standing: m.membership_standing,
            join_date: m.join_date,
            expires: m.xdate,
            membership_type: m.membership_type,
            monthly_dues: m.monthly_dues,
            yearly_dues: m.yearly_dues,
        }
    }
}

/// Deduplicate projected records the way both stores must agree on: first-wins on the
/// Solidarity Tech id, then first-wins on the Discord id (a later record claiming an id an
/// earlier one already holds is dropped, keeping the id lookups unambiguous). Records with
/// no Discord id are kept - they are exactly who a verify backfill repairs - and are found
/// afterwards by handle. The kept records are returned in input order.
///
/// This is the single definition of the dedup rule. Both the in-memory [`Index`] and the
/// Postgres store run their inputs through it, so the two stores can never silently diverge
/// on which of a pair of colliding records survives.
pub fn dedup_records(records: Vec<MemberRecord>) -> Vec<MemberRecord> {
    let mut seen_st = HashSet::new();
    let mut seen_id = HashSet::new();
    let mut kept = Vec::with_capacity(records.len());
    for rec in records {
        if !seen_st.insert(rec.st_user_id.0.clone()) {
            continue; // the same Solidarity Tech member was already kept
        }
        // A later record claiming an already-taken Discord id is dropped (first-wins).
        if let Some(id) = rec.discord_user_id
            && !seen_id.insert(id.0)
        {
            continue;
        }
        kept.push(rec);
    }
    kept
}

/// An immutable lookup index. Keyed by Discord id for the card's id-only read, and
/// also by handle so a member known to Solidarity Tech by handle but not yet linked
/// to a Discord id is still found - the population verification repairs. The card
/// still reads `by_id` only (see [`crate::card::resolve`]); the handle map exists for
/// the verify path.
#[derive(Default)]
pub struct Index {
    by_id: HashMap<u64, MemberRecord>,
    by_handle: HashMap<String, MemberRecord>,
}

impl Index {
    /// Build from a Solidarity Tech sweep.
    pub fn build(st: Vec<SolidarityTechMember>) -> Self {
        Self::from_records(st.into_iter().map(MemberRecord::from).collect())
    }

    /// Build from already-projected [`MemberRecord`]s (the shape the cache stores).
    ///
    /// Runs the input through [`dedup_records`] - the rule the Postgres store shares - so
    /// a record whose Solidarity Tech id or Discord id was already claimed is dropped from
    /// both maps, keeping the two stores equivalent.
    pub fn from_records(records: Vec<MemberRecord>) -> Self {
        let mut idx = Index::default();
        for rec in dedup_records(records) {
            idx.insert(rec);
        }
        idx
    }

    /// Insert a record the caller has already deduplicated, into whichever maps its
    /// identity supports.
    fn insert(&mut self, rec: MemberRecord) {
        if let Some(handle) = rec.discord_handle.clone() {
            self.by_handle
                .entry(handle.0)
                .or_insert_with(|| rec.clone());
        }
        if let Some(id) = rec.discord_user_id {
            self.by_id.entry(id.0).or_insert(rec);
        }
    }

    #[cfg(test)]
    pub(crate) fn default_for_test() -> Self {
        Index::default()
    }

    /// Look up by Discord user id.
    pub fn by_id(&self, id: DiscordUserId) -> Option<MemberRecord> {
        self.by_id.get(&id.0).cloned()
    }

    /// Look up by Discord handle. Used only by the verify path; the card resolves by id.
    pub fn by_handle(&self, handle: &DiscordHandle) -> Option<MemberRecord> {
        self.by_handle.get(&handle.0).cloned()
    }

    /// Whether the index holds no members (every input record was a duplicate or
    /// lacked a Discord id and a handle). Used to refuse replacing a populated roster
    /// with an empty sweep.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_handle.is_empty()
    }
}

/// The per-guild runtime configuration set through the bot's `/setup` command:
/// the moderator role, the three managed status roles, the additive Manual Override
/// marker, and the three verification channels. Every field is optional - a freshly
/// deployed guild has nothing set until a moderator configures it. Built from id
/// newtypes so a store maps it to a single nullable-column row with no nesting, exactly
/// like [`MemberRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuildConfig {
    pub moderator_role: Option<DiscordRoleId>,
    pub member_role: Option<DiscordRoleId>,
    pub dues_expired_role: Option<DiscordRoleId>,
    pub unverified_role: Option<DiscordRoleId>,
    /// The additive Manual Override marker role, granted alongside `Member` on a hand
    /// approval. Optional and outside the status trichotomy: ordinary verification works
    /// without it, and it is never stripped by the status-role logic.
    pub manual_override_role: Option<DiscordRoleId>,
    pub mod_approval_channel: Option<DiscordChannelId>,
    pub unverified_channel: Option<DiscordChannelId>,
    pub dues_expired_channel: Option<DiscordChannelId>,
}

/// Reverse lookup from a Discord id to a [`MemberRecord`]. Async and fallible from
/// the start so a later Postgres-backed implementation drops in without a signature
/// change; the in-memory impl's [`Error`](MemberStore::Error) is [`Infallible`].
#[async_trait]
pub trait MemberStore: Send + Sync {
    /// How a read can fail. [`Infallible`] for the in-memory store; a database
    /// error for a Postgres-backed one.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Look up a member by their Discord user snowflake.
    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Self::Error>;

    /// Look up a member by their current Discord handle. The repair fallback when an
    /// id lookup misses; the card never uses it.
    ///
    /// Handles are not unique in the cache - a handle can be recycled between roster
    /// sweeps - so when several records share one, which is returned is unspecified and may
    /// differ between implementations. That is acceptable because the immutable id is
    /// authoritative: verify reads this only after [`by_discord_id`](Self::by_discord_id)
    /// misses, and the conflict guard still refuses to re-link a record already bound to a
    /// different id.
    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Self::Error>;
}

/// Replace the whole cached roster in one shot - the write half of a refresh sweep.
/// Fallible from the start for the same reason as [`MemberStore`]: the in-memory
/// impl's [`Error`](RosterWrite::Error) is [`Infallible`], a Postgres-backed one's
/// is a database error.
#[async_trait]
pub trait RosterWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Atomically replace the stored roster with `records`. An empty roster is a
    /// no-op that preserves the current one: a sweep resolving to zero members is
    /// treated as an upstream glitch, never a real membership of zero.
    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Self::Error>;
}

/// Repair one member's stored Discord identity in place, keyed by their Solidarity
/// Tech id. The write-through half of verification's self-heal: distinct from
/// [`RosterWrite`], which only ever replaces the whole roster. Fallible from the
/// start for the same reason as the other store traits.
#[async_trait]
pub trait IdentityWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Set the Discord user id and handle on the member with `st_user_id`. A member
    /// the store does not hold is a silent no-op (nothing to repair).
    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Self::Error>;
}

/// Read and replace one guild's [`GuildConfig`]. Fallible and async from the start
/// for the same reason as the other store traits: the in-memory impl's
/// [`Error`](ConfigStore::Error) is [`Infallible`], a Postgres-backed one's is a
/// database error. `save_config` replaces the whole row (last-writer-wins); config
/// is admin-only and low-frequency, so no per-field write path is needed.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Load the config for `guild`, returning the default (all-unset) when the guild
    /// has no stored row yet.
    async fn load_config(&self, guild: DiscordGuildId) -> Result<GuildConfig, Self::Error>;

    /// Replace `guild`'s stored config wholesale.
    async fn save_config(
        &self,
        guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Self::Error>;
}

/// The in-memory [`MemberStore`]: a snapshot [`Index`] behind a
/// `RwLock<Arc<Index>>`. Reads clone out the `Arc` and never block a concurrent
/// rebuild; the write lock is held only for the pointer swap itself.
pub struct InMemoryStore {
    index: RwLock<Arc<Index>>,
    config: RwLock<GuildConfig>,
}

impl InMemoryStore {
    /// Construct a store from an already-built [`Index`].
    pub fn new(index: Index) -> Self {
        Self {
            index: RwLock::new(Arc::new(index)),
            config: RwLock::new(GuildConfig::default()),
        }
    }

    /// Atomically replace the live index. This is the only place the write lock
    /// is taken; in-flight reads hold their own `Arc` clone and are unaffected.
    pub fn swap(&self, index: Index) {
        *self.index.write().expect("index lock poisoned") = Arc::new(index);
    }

    fn snapshot(&self) -> Arc<Index> {
        self.index.read().expect("index lock poisoned").clone()
    }
}

#[async_trait]
impl MemberStore for InMemoryStore {
    type Error = Infallible;

    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_id(id))
    }

    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_handle(handle))
    }
}

#[async_trait]
impl RosterWrite for InMemoryStore {
    type Error = Infallible;

    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Infallible> {
        let index = Index::from_records(records);
        // Mirror PgStore: never overwrite a populated roster with an empty sweep.
        if index.is_empty() {
            return Ok(());
        }
        self.swap(index);
        Ok(())
    }
}

#[async_trait]
impl IdentityWrite for InMemoryStore {
    type Error = Infallible;

    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Infallible> {
        // Rebuild from the current snapshot with the one record's identity updated,
        // then swap - the same copy-on-write the roster refresh uses. A record mapped by
        // both id and handle is collected from both maps; the duplicates collapse in
        // `Index::from_records`, which is the single dedup point, so no pre-dedup is needed.
        let mut records: Vec<MemberRecord> = {
            let snap = self.snapshot();
            snap.by_id
                .values()
                .chain(snap.by_handle.values())
                .cloned()
                .collect()
        };
        for rec in &mut records {
            if rec.st_user_id == *st_user_id {
                rec.discord_user_id = Some(discord_id);
                rec.discord_handle = Some(handle.clone());
            }
        }
        self.swap(Index::from_records(records));
        Ok(())
    }
}

#[async_trait]
impl ConfigStore for InMemoryStore {
    type Error = Infallible;

    async fn load_config(&self, _guild: DiscordGuildId) -> Result<GuildConfig, Infallible> {
        Ok(self.config.read().expect("config lock poisoned").clone())
    }

    async fn save_config(
        &self,
        _guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), Infallible> {
        *self.config.write().expect("config lock poisoned") = config.clone();
        Ok(())
    }
}

/// Sweep the Solidarity Tech user list (pre-filtered to Discord-linked members)
/// into the flat [`MemberRecord`]s the cache stores. The store-agnostic half of the
/// refresh: the caller hands the result to [`RosterWrite::replace_roster`].
pub async fn sweep_roster(
    st: &impl SolidarityTechClient,
    list_id: &str,
) -> crate::Result<Vec<MemberRecord>> {
    let st_members = drain_pages(
        &NoProgress,
        "solidarity tech discord list",
        |cursor| async move { st.members_in_list_page(list_id, cursor.as_deref()).await },
    )
    .await?;
    tracing::info!(
        members = st_members.len(),
        list_id,
        "fetched discord-list members from solidarity tech"
    );
    Ok(st_members.into_iter().map(MemberRecord::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::solidarity_tech::SolidarityTechMember;
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use chrono::NaiveDate;
    use domain::{MigsStatus, Role};

    use crate::backends::MemberPage;
    use crate::backends::solidarity_tech::MockSolidarityTechClient;
    use crate::testkit::ready_ok;

    fn st_page(members: Vec<SolidarityTechMember>) -> MemberPage<SolidarityTechMember> {
        let scanned = members.len() as u64;
        MemberPage {
            members,
            scanned,
            total: Some(scanned),
            next: None,
        }
    }

    #[tokio::test]
    async fn sweep_roster_fetches_the_discord_list() {
        let mut st_client = MockSolidarityTechClient::new();
        st_client
            .expect_members_in_list_page()
            .returning(|_, _| ready_ok(st_page(vec![st("zoop", 42, "zoop")])));
        let records = sweep_roster(&st_client, "1234").await.unwrap();
        assert!(
            records
                .iter()
                .any(|r| r.discord_user_id == Some(DiscordUserId(42)))
        );
    }

    #[test]
    fn st_member_maps_into_record() {
        let st = SolidarityTechMember {
            id: StUserId("1".into()),
            email: Email("a@b.com".into()),
            first_name: Some("zoop".into()),
            discord_handle: Some(DiscordHandle("zoop".into())),
            discord_user_id: Some(DiscordUserId(42)),
            membership_standing: Some(MigsStatus::MemberInGoodStanding),
            xdate: NaiveDate::from_ymd_opt(2026, 12, 31),
            join_date: NaiveDate::from_ymd_opt(2021, 3, 15),
            ..Default::default()
        };
        let r = MemberRecord::from(st);
        assert_eq!(r.discord_user_id, Some(DiscordUserId(42)));
        assert_eq!(r.email.as_str(), "a@b.com");
        assert_eq!(r.full_name.as_deref(), Some("zoop"));
        assert_eq!(r.standing, Some(MigsStatus::MemberInGoodStanding));
        assert_eq!(r.role(), Role::Member);
        assert_eq!(r.join_date, NaiveDate::from_ymd_opt(2021, 3, 15));
    }

    #[test]
    fn full_name_combines_first_and_last() {
        let st = SolidarityTechMember {
            id: StUserId("9".into()),
            email: Email("z@b.com".into()),
            first_name: Some("zoop".into()),
            last_name: Some("goop".into()),
            ..Default::default()
        };
        assert_eq!(
            MemberRecord::from(st).full_name.as_deref(),
            Some("zoop goop")
        );
    }

    #[test]
    fn record_role_defaults_to_unverified_when_standing_absent() {
        let st = SolidarityTechMember {
            id: StUserId("2".into()),
            email: Email("c@d.com".into()),
            membership_standing: None,
            ..Default::default()
        };
        assert_eq!(MemberRecord::from(st).role(), Role::Unverified);
    }

    fn st(handle: &str, id: u64, name: &str) -> SolidarityTechMember {
        SolidarityTechMember {
            id: StUserId(id.to_string()),
            email: Email(format!("{name}@st.test")),
            first_name: Some(name.into()),
            discord_handle: Some(DiscordHandle(handle.into())),
            discord_user_id: Some(DiscordUserId(id)),
            membership_standing: Some(MigsStatus::MemberInGoodStanding),
            ..Default::default()
        }
    }

    #[test]
    fn index_looks_up_by_id() {
        let idx = Index::build(vec![st("zoop", 42, "zoop")]);
        assert_eq!(
            idx.by_id(DiscordUserId(42)).unwrap().email.as_str(),
            "zoop@st.test"
        );
        assert!(idx.by_id(DiscordUserId(99)).is_none());
    }

    #[tokio::test]
    async fn in_memory_store_reads_and_swaps() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some()
        );
        // Swap in an index that no longer contains 42.
        store.swap(Index::build(vec![st("rose", 99, "rose")]));
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .by_discord_id(DiscordUserId(99))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn empty_roster_does_not_wipe_a_populated_store() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        // An empty sweep must be a no-op, not a wipe.
        store.replace_roster(vec![]).await.unwrap();
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some(),
            "empty replace_roster must preserve the existing roster"
        );
    }

    #[tokio::test]
    async fn roster_of_only_unlinked_records_does_not_wipe() {
        let store = InMemoryStore::new(Index::build(vec![st("zoop", 42, "zoop")]));
        // Records with neither a Discord id nor a handle are unstorable, leaving an empty
        // index - which must be treated the same as an empty sweep, not as a wipe.
        let unlinked = MemberRecord {
            st_user_id: StUserId("ghost-1".into()),
            discord_user_id: None,
            discord_handle: None,
            email: Email("ghost@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        };
        store.replace_roster(vec![unlinked]).await.unwrap();
        assert!(
            store
                .by_discord_id(DiscordUserId(42))
                .await
                .unwrap()
                .is_some(),
            "a roster with no linkable members must preserve the existing roster"
        );
    }

    #[tokio::test]
    async fn config_round_trips_through_in_memory_store() {
        use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId};
        let store = InMemoryStore::new(Index::default_for_test());
        let guild = DiscordGuildId(7);
        // Default is all-unset.
        assert_eq!(
            store.load_config(guild).await.unwrap(),
            GuildConfig::default()
        );
        let cfg = GuildConfig {
            moderator_role: Some(DiscordRoleId(10)),
            member_role: Some(DiscordRoleId(11)),
            mod_approval_channel: Some(DiscordChannelId(20)),
            ..Default::default()
        };
        store.save_config(guild, &cfg).await.unwrap();
        assert_eq!(store.load_config(guild).await.unwrap(), cfg);
    }
}
