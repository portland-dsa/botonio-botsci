//! [`PgStore`] and [`InMemoryStore`] must answer `by_discord_id` identically.
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
use engine::store::{InMemoryStore, Index, MemberRecord, MemberStore, RosterWrite};
use engine::util::{DiscordHandle, DiscordUserId, Email};
use persistence::PgStore;

/// A date, unwrapped - the literals here are all valid.
fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date literal")
}

/// Three records covering the cases both stores must treat alike: a minimal record,
/// a fully-populated one (every optional field set, to exercise each token), and one
/// with no Discord id (which both stores must drop, since it cannot be looked up).
fn fixture() -> Vec<MemberRecord> {
    vec![
        MemberRecord {
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
        MemberRecord {
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
        // A second record carrying id 42. Both stores must keep the FIRST id-42 record
        // (a@b.test) and drop this one, never error on the duplicate. Placed last so
        // "first" is unambiguous.
        MemberRecord {
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
async fn all_unlinked_roster_preserves_the_cache(pool: sqlx::PgPool) {
    // A non-empty roster whose records all lack a Discord id dedups to zero storable rows.
    // Both stores must treat that like an empty roster - a no-op keeping the last good
    // cache - never a wipe. This is the input that would diverge the two stores if either
    // empty-check were placed before the dedup: it would wipe Postgres but not the in-memory
    // index, and the per-id comparison below would then catch the regression.
    let unlinked = || {
        vec![
            MemberRecord {
                discord_user_id: None,
                discord_handle: Some(DiscordHandle("ghost-a".into())),
                email: Email("ghost-a@b.test".into()),
                full_name: None,
                standing: None,
                join_date: None,
                expires: None,
                membership_type: None,
                monthly_dues: None,
                yearly_dues: None,
            },
            MemberRecord {
                discord_user_id: None,
                discord_handle: Some(DiscordHandle("ghost-b".into())),
                email: Email("ghost-b@b.test".into()),
                full_name: None,
                standing: None,
                join_date: None,
                expires: None,
                membership_type: None,
                monthly_dues: None,
                yearly_dues: None,
            },
        ]
    };

    let pg = PgStore::new(pool);
    pg.replace_roster(fixture()).await.unwrap();
    pg.replace_roster(unlinked()).await.unwrap();

    let mem = InMemoryStore::new(Index::default());
    mem.replace_roster(fixture()).await.unwrap();
    mem.replace_roster(unlinked()).await.unwrap();

    for id in [42u64, 99, 100] {
        let from_pg = pg.by_discord_id(DiscordUserId(id)).await.unwrap();
        let from_mem = mem.by_discord_id(DiscordUserId(id)).await.unwrap();
        assert_eq!(
            from_pg, from_mem,
            "stores disagreed after an all-unlinked roster on id {id}"
        );
        assert!(
            from_pg.is_some(),
            "id {id} must survive an all-unlinked roster"
        );
    }
}
