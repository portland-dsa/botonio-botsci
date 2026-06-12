//! The reusable member-record store: a flat [`MemberRecord`] and the
//! [`MemberStore`] trait the card resolver reads through.
//!
//! The current implementation ([`InMemoryStore`]) holds the roster in RAM, swept
//! from a Solidarity Tech user list. [`MemberRecord`] is deliberately flat and
//! built from persistence-friendly primitives so a future implementation can back the same
//! [`MemberStore`] trait with a `diesel` table without changing any caller.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::{MembershipStatus, MigsStatus, Role};

use crate::backends::solidarity_tech::{
    DuesStatus, MembershipType, SolidarityTechClient, SolidarityTechMember,
};
use crate::paging::drain_pages;
use crate::seam::NoProgress;
use crate::util::{DiscordHandle, DiscordUserId, Email};

/// A member projected to the flat shape the card and the future cache share.
/// Every field is a persistence-friendly primitive (`String`,
/// `Option<NaiveDate>`, small text-mapped enums) so a future implementation maps it to one
/// `diesel` table with no nesting.
#[derive(Debug, Clone)]
pub struct MemberRecord {
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

/// An immutable lookup index keyed by Discord snowflake id. There is deliberately
/// no handle-keyed map: a card is resolved by immutable id only (see
/// [`crate::card::resolve`]), never by a mutable, recyclable username.
#[derive(Default)]
pub struct Index {
    by_id: HashMap<u64, MemberRecord>,
}

impl Index {
    /// Build from a Solidarity Tech sweep.
    pub fn build(st: Vec<SolidarityTechMember>) -> Self {
        let mut idx = Index::default();
        for m in st {
            idx.insert(MemberRecord::from(m));
        }
        idx
    }

    fn insert(&mut self, rec: MemberRecord) {
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
}

/// Reverse lookup from a Discord id to a [`MemberRecord`]. Async from the start
/// so a later `diesel`-backed implementation drops in without a signature change.
#[async_trait]
pub trait MemberStore: Send + Sync {
    /// Look up a member by their Discord user snowflake.
    async fn by_discord_id(&self, id: DiscordUserId) -> Option<MemberRecord>;
}

/// The in-memory [`MemberStore`]: a snapshot [`Index`] behind a
/// `RwLock<Arc<Index>>`. Reads clone out the `Arc` and never block a concurrent
/// rebuild; the write lock is held only for the pointer swap itself.
pub struct InMemoryStore {
    index: RwLock<Arc<Index>>,
}

impl InMemoryStore {
    /// Construct a store from an already-built [`Index`].
    pub fn new(index: Index) -> Self {
        Self {
            index: RwLock::new(Arc::new(index)),
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
    async fn by_discord_id(&self, id: DiscordUserId) -> Option<MemberRecord> {
        self.snapshot().by_id(id)
    }
}

/// Build the member index from a Solidarity Tech user list (pre-filtered to
/// Discord-linked members) via [`drain_pages`] with a no-op progress reporter.
pub async fn build_index(st: &impl SolidarityTechClient, list_id: &str) -> crate::Result<Index> {
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
    Ok(Index::build(st_members))
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
    async fn build_index_fetches_the_discord_list() {
        let mut st_client = MockSolidarityTechClient::new();
        st_client
            .expect_members_in_list_page()
            .returning(|_, _| ready_ok(st_page(vec![st("zoop", 42, "zoop")])));
        let idx = build_index(&st_client, "1234").await.unwrap();
        assert!(idx.by_id(DiscordUserId(42)).is_some());
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
        assert!(store.by_discord_id(DiscordUserId(42)).await.is_some());
        // Swap in an index that no longer contains 42.
        store.swap(Index::build(vec![st("rose", 99, "rose")]));
        assert!(store.by_discord_id(DiscordUserId(42)).await.is_none());
        assert!(store.by_discord_id(DiscordUserId(99)).await.is_some());
    }
}
