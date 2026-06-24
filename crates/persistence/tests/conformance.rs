//! [`PgStore`] and [`InMemoryStore`] must answer `by_discord_id` and `by_handle`
//! identically.
//!
//! The in-memory store hands back the exact [`MemberRecord`] it was given; the
//! Postgres store encodes each record to text tokens, writes it, reads it back, and
//! decodes it. Asserting the two agree therefore proves the cache's
//! encode/store/decode round-trip is lossless for every field - a token typo or a
//! column mix-up would make a populated record disagree.
//!
//! Gated behind the `live-db` feature because it needs a loopback Postgres; run it
//! from a shell that can bind loopback (see `deploy/test-infra/`). A plain
//! `cargo test -p persistence` compiles this file to nothing and needs no database.
#![cfg(feature = "live-db")]

use chrono::NaiveDate;

use domain::MigsStatus;
use engine::backends::solidarity_tech::{DuesStatus, MembershipType};
use engine::channels::ChannelSnapshot;
use engine::store::{
    BulkQueueEntry, BulkQueueKind, BulkScope, BulkSession, BulkSessionStore, BulkStatus,
    ChannelSnapshotStore, IdentityWrite, InMemoryStore, Index, MemberRecord, MemberStore,
    MissState, OverrideLog, RosterWrite,
};
use engine::util::{DiscordHandle, DiscordUserId, Email, StUserId};
use persistence::PgStore;

/// A date, unwrapped - the literals here are all valid.
fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date literal")
}

/// Seven records covering the cases both stores must treat alike: a minimal record,
/// a fully-populated one (every optional field set, to exercise each token), records
/// covering every remaining enum variant, a handle-only member (no Discord id, now
/// retained and findable by handle), and a duplicate-id impostor that both stores
/// drop via the Discord-id first-wins guard.
fn fixture() -> Vec<MemberRecord> {
    vec![
        MemberRecord {
            st_user_id: StUserId("st-a".into()),
            discord_user_id: Some(DiscordUserId(42)),
            discord_handle: None,
            email: Email("a@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        },
        MemberRecord {
            st_user_id: StUserId("st-99".into()),
            discord_user_id: Some(DiscordUserId(99)),
            discord_handle: Some(DiscordHandle("zoopgoop".into())),
            email: Email("zoopgoop@b.test".into()),
            full_name: Some("Zoop Goop".into()),
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: Some(date(2021, 3, 15)),
            expires: Some(date(2026, 12, 31)),
            membership_type: Some(MembershipType::Monthly),
            monthly_dues: Some(DuesStatus::Active),
            yearly_dues: Some(DuesStatus::Cancelled),
        },
        // Records 100-102 cover the enum variants record 99 does not, so the round-trip is
        // exercised for *every* MembershipType, DuesStatus, and MigsStatus token - a drift
        // in any one (e.g. a yearly/one-time/income-based or never/overdue token) makes the
        // pg/mem comparison disagree instead of slipping through silently.
        MemberRecord {
            st_user_id: StUserId("st-100".into()),
            discord_user_id: Some(DiscordUserId(100)),
            discord_handle: Some(DiscordHandle("yearly".into())),
            email: Email("yearly@b.test".into()),
            full_name: Some("Yearly Member".into()),
            standing: Some(MigsStatus::Lapsed),
            join_date: Some(date(2020, 1, 1)),
            expires: Some(date(2025, 1, 1)),
            membership_type: Some(MembershipType::Yearly),
            monthly_dues: Some(DuesStatus::Never),
            yearly_dues: Some(DuesStatus::Overdue),
        },
        MemberRecord {
            st_user_id: StUserId("st-101".into()),
            discord_user_id: Some(DiscordUserId(101)),
            discord_handle: Some(DiscordHandle("onetime".into())),
            email: Email("onetime@b.test".into()),
            full_name: Some("One Time".into()),
            standing: Some(MigsStatus::MemberInGoodStanding),
            join_date: None,
            expires: None,
            membership_type: Some(MembershipType::OneTime),
            monthly_dues: Some(DuesStatus::Overdue),
            yearly_dues: Some(DuesStatus::Never),
        },
        MemberRecord {
            st_user_id: StUserId("st-102".into()),
            discord_user_id: Some(DiscordUserId(102)),
            discord_handle: None,
            email: Email("income@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: Some(MembershipType::IncomeBased),
            monthly_dues: None,
            yearly_dues: None,
        },
        // A handle-only member: no Discord id yet. Both stores retain this record now;
        // it is findable by handle, which is exactly the repair path the verify backfill
        // uses to link an id later.
        MemberRecord {
            st_user_id: StUserId("st-ghost".into()),
            discord_user_id: None,
            discord_handle: Some(DiscordHandle("ghost".into())),
            email: Email("ghost@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        },
        // A second record carrying id 42 but a different st_user_id. Both stores must keep
        // the FIRST id-42 record (a@b.test / st-a) and drop this one via the Discord-id
        // first-wins guard - not via st-id dedup, since the st ids differ. Placed last so
        // "first" is unambiguous.
        MemberRecord {
            st_user_id: StUserId("st-42-impostor".into()),
            discord_user_id: Some(DiscordUserId(42)),
            discord_handle: Some(DiscordHandle("impostor".into())),
            email: Email("dupe-42@b.test".into()),
            full_name: Some("Dupe".into()),
            standing: Some(MigsStatus::Lapsed),
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        },
    ]
}

#[sqlx::test(migrations = "./migrations")]
async fn pg_and_memory_agree(pool: sqlx::PgPool) {
    let pg = PgStore::new(pool);
    pg.replace_roster(fixture()).await.unwrap();

    let mem = InMemoryStore::new(Index::default());
    mem.replace_roster(fixture()).await.unwrap();

    // 42: minimal hit (also a duplicated id in the fixture); 99-102: hits covering every
    // enum token; 1234: a miss (neither has it).
    for id in [42u64, 99, 100, 101, 102, 1234] {
        let from_pg = pg.by_discord_id(DiscordUserId(id)).await.unwrap();
        let from_mem = mem.by_discord_id(DiscordUserId(id)).await.unwrap();
        assert_eq!(from_pg, from_mem, "stores disagreed on id {id}");
    }

    // First-wins on the duplicated id 42: both stores keep the first record (a@b.test)
    // and drop the later impostor, rather than erroring on the primary-key clash.
    let pg_42 = pg.by_discord_id(DiscordUserId(42)).await.unwrap();
    assert_eq!(
        pg_42.map(|r| r.email.0),
        Some("a@b.test".to_owned()),
        "duplicate id 42 must resolve to the first record, not the impostor"
    );

    // Handle-read agreement: both stores must answer by_handle identically.
    for handle in ["zoopgoop", "yearly", "ghost", "impostor", "nobody"] {
        let h = DiscordHandle(handle.into());
        let from_pg = pg.by_handle(&h).await.unwrap();
        let from_mem = mem.by_handle(&h).await.unwrap();
        assert_eq!(from_pg, from_mem, "stores disagreed on handle {handle}");
    }
    // The handle-only "ghost" is retained and findable by handle, with no Discord id.
    let ghost = pg.by_handle(&DiscordHandle("ghost".into())).await.unwrap();
    assert_eq!(ghost.map(|r| r.discord_user_id), Some(None));
    // The dropped impostor (its Discord id 42 was already taken) is unreachable by handle.
    assert!(
        pg.by_handle(&DiscordHandle("impostor".into()))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn empty_roster_preserves_the_cache(pool: sqlx::PgPool) {
    let pg = PgStore::new(pool);
    pg.replace_roster(fixture()).await.unwrap();
    // An empty roster is a no-op that must not wipe the populated cache.
    pg.replace_roster(vec![]).await.unwrap();
    assert!(
        pg.by_discord_id(DiscordUserId(99)).await.unwrap().is_some(),
        "empty replace_roster must preserve the existing cache"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn is_populated_reflects_contents(pool: sqlx::PgPool) {
    let pg = PgStore::new(pool);
    assert!(
        !pg.is_populated().await.unwrap(),
        "a freshly migrated cache is empty"
    );
    pg.replace_roster(fixture()).await.unwrap();
    assert!(
        pg.is_populated().await.unwrap(),
        "the cache is populated after a load"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn ping_answers(pool: sqlx::PgPool) {
    PgStore::new(pool).ping().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn handle_only_members_are_stored_and_found_by_handle(pool: sqlx::PgPool) {
    let handle_only = || {
        vec![MemberRecord {
            st_user_id: StUserId("st-rosy".into()),
            discord_user_id: None,
            discord_handle: Some(DiscordHandle("rosy".into())),
            email: Email("rosy@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }]
    };
    let pg = PgStore::new(pool);
    pg.replace_roster(handle_only()).await.unwrap();

    let mem = InMemoryStore::new(Index::default());
    mem.replace_roster(handle_only()).await.unwrap();

    let from_pg = pg.by_handle(&DiscordHandle("rosy".into())).await.unwrap();
    let from_mem = mem.by_handle(&DiscordHandle("rosy".into())).await.unwrap();
    assert_eq!(from_pg, from_mem);
    assert!(
        from_pg.is_some(),
        "a handle-only member is stored, not dropped"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn link_identity_backfills_a_discord_id(pool: sqlx::PgPool) {
    let seed = vec![MemberRecord {
        st_user_id: StUserId("st-rosy".into()),
        discord_user_id: None,
        discord_handle: Some(DiscordHandle("rosy".into())),
        email: Email("rosy@b.test".into()),
        full_name: None,
        standing: None,
        join_date: None,
        expires: None,
        membership_type: None,
        monthly_dues: None,
        yearly_dues: None,
    }];
    let pg = PgStore::new(pool);
    pg.replace_roster(seed).await.unwrap();

    pg.link_identity(
        &StUserId("st-rosy".into()),
        DiscordUserId(77),
        &DiscordHandle("rosy".into()),
    )
    .await
    .unwrap();

    let found = pg
        .by_discord_id(DiscordUserId(77))
        .await
        .unwrap()
        .expect("the member is findable by the backfilled id");
    assert_eq!(found.discord_handle, Some(DiscordHandle("rosy".into())));
}

#[sqlx::test(migrations = "./migrations")]
async fn guild_config_round_trips(pool: sqlx::PgPool) {
    use domain::{DiscordChannelId, DiscordGuildId, DiscordRoleId};
    use engine::store::{ConfigStore, GuildConfig};

    let store = PgStore::new(pool);
    let guild = DiscordGuildId(123);

    // No row yet -> the default (all unset).
    assert_eq!(
        store.load_config(guild).await.unwrap(),
        GuildConfig::default()
    );

    // A fully-populated config round-trips losslessly.
    let full = GuildConfig {
        moderator_role: Some(DiscordRoleId(1)),
        member_role: Some(DiscordRoleId(2)),
        dues_expired_role: Some(DiscordRoleId(3)),
        unverified_role: Some(DiscordRoleId(4)),
        manual_override_role: Some(DiscordRoleId(8)),
        mod_approval_channel: Some(DiscordChannelId(5)),
        unverified_channel: Some(DiscordChannelId(6)),
        dues_expired_channel: Some(DiscordChannelId(7)),
        verification_log_channel: Some(DiscordChannelId(10)),
        scan_enabled: true,
    };
    store.save_config(guild, &full).await.unwrap();
    assert_eq!(store.load_config(guild).await.unwrap(), full);

    // The upsert replaces the row wholesale: a later partial config wins, the
    // previously-set fields it omits going back to unset.
    let partial = GuildConfig {
        moderator_role: Some(DiscordRoleId(9)),
        ..Default::default()
    };
    store.save_config(guild, &partial).await.unwrap();
    assert_eq!(store.load_config(guild).await.unwrap(), partial);
}

#[sqlx::test(migrations = "./migrations")]
async fn override_stamp_is_insert_once_and_deletable(pool: sqlx::PgPool) {
    let store = PgStore::new(pool.clone());
    let subject = DiscordUserId(4242);

    store
        .stamp_override(subject, DiscordUserId(1), None)
        .await
        .unwrap();
    // Insert-once: a second stamp with a different approver neither overwrites nor errors.
    store
        .stamp_override(subject, DiscordUserId(2), None)
        .await
        .unwrap();
    let approver = sqlx::query_scalar!(
        "SELECT approved_by FROM manual_override WHERE discord_user_id = $1",
        subject.0 as i64
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(approver, 1, "the first approver is preserved on a re-stamp");

    // The typed read returns the preserved first approver.
    let got = store
        .get_override(subject)
        .await
        .unwrap()
        .expect("a stamp exists");
    assert_eq!(got.approved_by, DiscordUserId(1));

    // delete_override needs DELETE, which the test role holds; production withholds it.
    store.delete_override(subject).await.unwrap();
    let remaining = sqlx::query_scalar!(
        "SELECT count(*) FROM manual_override WHERE discord_user_id = $1",
        subject.0 as i64
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, Some(0), "delete_override removes the stamp");

    // After delete, the typed read misses.
    assert!(store.get_override(subject).await.unwrap().is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn override_note_is_stored_and_preserved(pool: sqlx::PgPool) {
    let store = PgStore::new(pool);
    let subject = DiscordUserId(7777);

    // A stamp with no note reads back None.
    store
        .stamp_override(subject, DiscordUserId(1), None)
        .await
        .unwrap();
    assert_eq!(
        store.get_override(subject).await.unwrap().unwrap().note,
        None
    );
    store.delete_override(subject).await.unwrap();

    // A stamp with a note reads it back; insert-once preserves the first note even when a
    // later stamp carries a different one.
    store
        .stamp_override(
            subject,
            DiscordUserId(1),
            Some("vouched at the branch meeting".into()),
        )
        .await
        .unwrap();
    store
        .stamp_override(subject, DiscordUserId(2), Some("a later note".into()))
        .await
        .unwrap();
    let got = store.get_override(subject).await.unwrap().unwrap();
    assert_eq!(got.approved_by, DiscordUserId(1));
    assert_eq!(got.note.as_deref(), Some("vouched at the branch meeting"));
}

#[sqlx::test(migrations = "./migrations")]
async fn unlink_clears_the_cached_identity(pool: sqlx::PgPool) {
    let store = PgStore::new(pool);
    store
        .replace_roster(vec![MemberRecord {
            st_user_id: StUserId("st-link".into()),
            discord_user_id: Some(DiscordUserId(7)),
            discord_handle: Some(DiscordHandle("linked".into())),
            email: Email("linked@b.test".into()),
            full_name: None,
            standing: None,
            join_date: None,
            expires: None,
            membership_type: None,
            monthly_dues: None,
            yearly_dues: None,
        }])
        .await
        .unwrap();
    assert!(
        store
            .by_discord_id(DiscordUserId(7))
            .await
            .unwrap()
            .is_some(),
        "the linked member is present before the unlink"
    );

    store.unlink_by_discord_id(DiscordUserId(7)).await.unwrap();

    assert!(
        store
            .by_discord_id(DiscordUserId(7))
            .await
            .unwrap()
            .is_none(),
        "the id column is cleared, so the member is no longer found by id"
    );
    assert!(
        store
            .by_handle(&DiscordHandle("linked".into()))
            .await
            .unwrap()
            .is_none(),
        "the handle column is cleared too, so the member is no longer found by handle"
    );
}

fn session(guild: u64) -> BulkSession {
    BulkSession {
        guild: domain::DiscordGuildId(guild),
        scope: BulkScope::UnmanagedOnly,
        status: BulkStatus::InProgress,
        started_by: DiscordUserId(1),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn miss(id: u64, pos: i32) -> BulkQueueEntry {
    BulkQueueEntry {
        discord_user_id: DiscordUserId(id),
        handle: Some(DiscordHandle(format!("u{id}"))),
        position: pos,
        state: MissState::Pending,
        kind: BulkQueueKind::Miss,
    }
}

fn malformed(id: u64, pos: i32) -> BulkQueueEntry {
    BulkQueueEntry {
        kind: BulkQueueKind::Malformed,
        ..miss(id, pos)
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn bulk_session_round_trips_and_resumes(pool: sqlx::PgPool) {
    let pg = PgStore::new(pool);
    let g = domain::DiscordGuildId(7);
    // The first entry is a malformed-record kind, so the round-trip also proves the
    // `kind` column persists 'malformed' and decodes back (and the CHECK accepts it).
    pg.start_session(&session(7), &[malformed(10, 0), miss(11, 1)])
        .await
        .unwrap();

    // Resume picks the lowest-position pending member, kind intact.
    let next = pg.next_pending(g).await.unwrap().unwrap();
    assert_eq!(next.discord_user_id, DiscordUserId(10));
    assert_eq!(next.kind, BulkQueueKind::Malformed);

    // Marking it verified advances the queue and the counts.
    pg.mark_miss(g, DiscordUserId(10), MissState::Verified)
        .await
        .unwrap();
    let next = pg.next_pending(g).await.unwrap().unwrap();
    assert_eq!(next.discord_user_id, DiscordUserId(11));
    let counts = pg.counts(g).await.unwrap();
    assert_eq!((counts.pending, counts.verified), (1, 1));

    // Skip the last, complete, and confirm the queue is exhausted.
    pg.mark_miss(g, DiscordUserId(11), MissState::Skipped)
        .await
        .unwrap();
    assert!(pg.next_pending(g).await.unwrap().is_none());
    pg.complete_session(g).await.unwrap();
    assert_eq!(
        pg.load_session(g).await.unwrap().unwrap().status,
        BulkStatus::Complete
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn start_over_replaces_the_queue(pool: sqlx::PgPool) {
    let pg = PgStore::new(pool);
    let g = domain::DiscordGuildId(7);
    pg.start_session(&session(7), &[miss(10, 0), miss(11, 1)])
        .await
        .unwrap();
    // A fresh start wholesale-replaces the prior queue (CASCADE clears the old rows).
    pg.start_session(&session(7), &[miss(20, 0)]).await.unwrap();
    let next = pg.next_pending(g).await.unwrap().unwrap();
    assert_eq!(next.discord_user_id, DiscordUserId(20));
    assert_eq!(pg.counts(g).await.unwrap().pending, 1);
}

/// Build a minimal snapshot with the given number of placeholder channels for the
/// given guild and timestamp.
fn snapshot(
    guild: u64,
    saved_at: chrono::DateTime<chrono::Utc>,
    channel_count: usize,
) -> ChannelSnapshot {
    use engine::backends::discord::ChannelKind;
    use engine::channels::SavedChannel;
    // format_version 1 is the current stable layout; matches SNAPSHOT_FORMAT_VERSION.
    ChannelSnapshot {
        format_version: 1,
        guild_id: domain::DiscordGuildId(guild),
        saved_at,
        channels: (0..channel_count)
            .map(|i| SavedChannel {
                id: domain::DiscordChannelId(i as u64 + 1),
                name: format!("chan-{i}"),
                kind: ChannelKind::Text,
                parent_id: None,
                overwrites: vec![],
            })
            .collect(),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn channel_snapshot_save_latest_and_list(pool: sqlx::PgPool) {
    use chrono::TimeZone;

    let pg = PgStore::new(pool);
    let guild = domain::DiscordGuildId(500);
    let other = domain::DiscordGuildId(501);

    // Nothing saved yet.
    assert!(pg.latest_snapshot(guild).await.unwrap().is_none());
    assert!(pg.list_snapshots(guild).await.unwrap().is_empty());

    let t1 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let t2 = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();

    let s1 = snapshot(guild.0, t1, 3); // 3 channels, older
    let s2 = snapshot(guild.0, t2, 5); // 5 channels, newer
    let s_other = snapshot(other.0, t1, 2); // different guild - must not bleed into guild

    pg.save_snapshot(&s1).await.unwrap();
    pg.save_snapshot(&s_other).await.unwrap();
    pg.save_snapshot(&s2).await.unwrap();

    // latest_snapshot returns the newest for this guild (s2, not s1 or s_other).
    let latest = pg
        .latest_snapshot(guild)
        .await
        .unwrap()
        .expect("a snapshot exists");
    assert_eq!(
        latest, s2,
        "latest_snapshot must return the newest saved snapshot"
    );

    // list_snapshots returns both entries for guild, newest first.
    let metas = pg.list_snapshots(guild).await.unwrap();
    assert_eq!(
        metas.len(),
        2,
        "list_snapshots must return one entry per saved snapshot"
    );
    assert_eq!(metas[0].saved_at, t2, "first entry is the newer snapshot");
    assert_eq!(metas[0].channel_count, 5);
    assert_eq!(metas[1].saved_at, t1, "second entry is the older snapshot");
    assert_eq!(metas[1].channel_count, 3);

    // The other guild is isolated: its one snapshot is present there but not here.
    let other_latest = pg.latest_snapshot(other).await.unwrap();
    assert_eq!(other_latest, Some(s_other));
    assert_eq!(pg.list_snapshots(other).await.unwrap().len(), 1);
}
