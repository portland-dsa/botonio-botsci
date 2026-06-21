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
use engine::store::{IdentityWrite, InMemoryStore, Index, MemberRecord, MemberStore, RosterWrite};
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
