//! The reusable member-record store: the flat [`MemberRecord`] and the store traits the
//! engine reads and writes through, plus the in-memory implementation behind them.
//!
//! This hub owns the concrete [`InMemoryStore`] (its fields, constructor, and index-swap
//! helpers) and re-exports every type and trait so callers keep using `engine::store::X`. The
//! vocabulary and the per-trait [`InMemoryStore`] impls are split by concern across the leaf
//! modules - the leaves reach this hub's private store fields and helpers, which a parent
//! module could not reach into a child's private types in the other direction:
//!
//! - [`member`] - the roster: [`MemberRecord`], [`Index`], the dedup rule, the read/write/
//!   repair traits, and the Solidarity Tech sweep.
//! - [`config`] - guild-level config ([`GuildConfig`], [`MessageRef`]) and channel snapshots.
//! - [`moderation`] - the manual override, the grace override, and the bulk-verify session.
//! - [`reminders`] - the dues-reminder cycle state, opt-out, and editable message bodies.
//!
//! [`MemberRecord`] is deliberately flat and built from persistence-friendly primitives so a
//! Postgres-backed store can implement the same traits over a sqlx-mapped table.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::channels::snapshot::ChannelSnapshot;

mod config;
mod member;
mod moderation;
mod reminders;

pub use config::*;
pub use member::*;
pub use moderation::*;
pub use reminders::*;

/// The in-memory [`MemberStore`]: a snapshot [`Index`] behind a
/// `RwLock<Arc<Index>>`. Reads clone out the `Arc` and never block a concurrent
/// rebuild; the write lock is held only for the pointer swap itself.
///
/// The fields are private to this hub but reachable from the leaf modules (its descendants),
/// where each trait impl lives.
pub struct InMemoryStore {
    index: RwLock<Arc<Index>>,
    config: RwLock<GuildConfig>,
    /// Hand-approval stamps: subject Discord id to its [`OverrideRecord`]. The
    /// in-memory analogue of the `manual_override` table, insert-once just like it.
    overrides: RwLock<HashMap<u64, OverrideRecord>>,
    /// The single per-guild bulk session + its queue (in-memory analogue of the
    /// bulk_verify_session/miss tables). `BTreeMap<position, BulkQueueEntry>` keeps the
    /// queue ordered; the option is the at-most-one session.
    bulk: RwLock<Option<(BulkSession, std::collections::BTreeMap<i32, BulkQueueEntry>)>>,
    /// Channel-permission snapshots, in insertion order. Newest is last. The
    /// in-memory analogue of a future snapshots table.
    snapshots: RwLock<Vec<ChannelSnapshot>>,
    /// Moderator grace stamps: Discord id to its [`GraceOverride`].
    grace: RwLock<HashMap<u64, GraceOverride>>,
    /// Per-member reminder cycle state: Discord id to its [`ReminderCycleState`].
    reminder_state: RwLock<HashMap<u64, ReminderCycleState>>,
    /// Permanent opt-outs: Discord id to the [`OptOutSource`] that set it.
    opt_out: RwLock<HashMap<u64, OptOutSource>>,
    /// Per-template-kind bodies: token string to body text.
    templates: RwLock<HashMap<String, String>>,
}

impl InMemoryStore {
    /// Construct a store from an already-built [`Index`].
    pub fn new(index: Index) -> Self {
        Self {
            index: RwLock::new(Arc::new(index)),
            config: RwLock::new(GuildConfig::default()),
            overrides: RwLock::new(HashMap::new()),
            bulk: RwLock::new(None),
            snapshots: RwLock::new(Vec::new()),
            grace: RwLock::new(HashMap::new()),
            reminder_state: RwLock::new(HashMap::new()),
            opt_out: RwLock::new(HashMap::new()),
            templates: RwLock::new(HashMap::new()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::solidarity_tech::FakeSolidarityTech;
    use crate::backends::solidarity_tech::SolidarityTechMember;
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};
    use chrono::{NaiveDate, Utc};
    use domain::{MembershipStatus, MigsStatus, Role};

    #[tokio::test]
    async fn sweep_roster_fetches_the_discord_list() {
        let st_client = FakeSolidarityTech::new().with_members(vec![st("zoop", 42, "zoop")]);
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
        assert_eq!(Role::try_from(r.membership()), Ok(Role::Member));
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

    fn base_st() -> SolidarityTechMember {
        SolidarityTechMember {
            id: StUserId("base".into()),
            email: Email("base@test.com".into()),
            ..Default::default()
        }
    }

    #[test]
    fn membership_is_malformed_when_standing_absent() {
        let st = SolidarityTechMember {
            membership_standing: None,
            ..base_st()
        };
        assert_eq!(
            MemberRecord::from(st).membership(),
            MembershipStatus::Malformed
        );
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

    #[tokio::test]
    async fn get_override_round_trips_stamp() {
        let store = InMemoryStore::new(Index::default());
        assert!(
            store
                .get_override(DiscordUserId(7))
                .await
                .unwrap()
                .is_none()
        );
        store
            .stamp_override(DiscordUserId(7), DiscordUserId(99), None)
            .await
            .unwrap();
        let got = store.get_override(DiscordUserId(7)).await.unwrap().unwrap();
        assert_eq!(got.approved_by, DiscordUserId(99));
    }

    #[tokio::test]
    async fn stamp_override_records_and_preserves_the_note() {
        let store = InMemoryStore::new(Index::default());
        store
            .stamp_override(
                DiscordUserId(7),
                DiscordUserId(99),
                Some("vouched in person".into()),
            )
            .await
            .unwrap();
        // Insert-once preserves the first note even if a later stamp carries another.
        store
            .stamp_override(
                DiscordUserId(7),
                DiscordUserId(1),
                Some("a later note".into()),
            )
            .await
            .unwrap();
        let got = store.get_override(DiscordUserId(7)).await.unwrap().unwrap();
        assert_eq!(got.approved_by, DiscordUserId(99));
        assert_eq!(got.note.as_deref(), Some("vouched in person"));
    }

    #[test]
    fn bulk_enum_tokens_round_trip() {
        for s in [BulkScope::UnmanagedOnly, BulkScope::WholeGuild] {
            assert_eq!(BulkScope::from_token(s.as_token()), Some(s));
        }
        for s in [
            BulkStatus::InProgress,
            BulkStatus::Complete,
            BulkStatus::Abandoned,
        ] {
            assert_eq!(BulkStatus::from_token(s.as_token()), Some(s));
        }
        for s in [MissState::Pending, MissState::Verified, MissState::Skipped] {
            assert_eq!(MissState::from_token(s.as_token()), Some(s));
        }
        assert_eq!(BulkScope::from_token("nonsense"), None);
        assert_eq!(BulkStatus::from_token("nonsense"), None);
        assert_eq!(MissState::from_token("nonsense"), None);
    }

    // Helpers for snapshot tests.
    fn empty_snapshot(guild: u64, saved_at: chrono::DateTime<Utc>) -> ChannelSnapshot {
        ChannelSnapshot {
            format_version: crate::channels::snapshot::SNAPSHOT_FORMAT_VERSION,
            guild_id: domain::DiscordGuildId(guild),
            saved_at,
            channels: vec![],
        }
    }

    #[tokio::test]
    async fn snapshot_save_latest_and_list() {
        use chrono::TimeZone;
        let store = InMemoryStore::new(Index::default_for_test());
        let guild = domain::DiscordGuildId(100);
        let other = domain::DiscordGuildId(999);

        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

        // Nothing saved yet.
        assert!(store.latest_snapshot(guild).await.unwrap().is_none());
        assert!(store.list_snapshots(guild).await.unwrap().is_empty());

        let s1 = empty_snapshot(guild.0, t1);
        let s2 = empty_snapshot(guild.0, t2);
        let s_other = empty_snapshot(other.0, t1);

        store.save_snapshot(&s1).await.unwrap();
        store.save_snapshot(&s_other).await.unwrap(); // different guild - must not affect guild
        store.save_snapshot(&s2).await.unwrap();

        // latest returns the most recently saved for this guild.
        assert_eq!(
            store.latest_snapshot(guild).await.unwrap(),
            Some(s2.clone())
        );

        // list returns newest first.
        let metas = store.list_snapshots(guild).await.unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].saved_at, t2);
        assert_eq!(metas[1].saved_at, t1);

        // other guild has its own snapshot, not leaking into guild.
        assert_eq!(store.latest_snapshot(other).await.unwrap(), Some(s_other));
        assert_eq!(store.list_snapshots(other).await.unwrap().len(), 1);
    }
}
