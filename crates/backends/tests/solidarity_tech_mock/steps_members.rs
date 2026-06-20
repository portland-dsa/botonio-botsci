//! Step definitions for `members.feature`: lookups, custom-property decoding,
//! pagination, and the identity write/clear merges. Also defines the shared
//! `a mocked Solidarity Tech API` background step both features use.

use chrono::NaiveDate;
use cucumber::{given, then, when};
use domain::MigsStatus;

use backends::solidarity_tech::{
    DuesStatus, MembershipType, SolidarityTechClient, SolidarityTechError, StClearFlags,
};
use backends::util::{DiscordHandle, DiscordUserId, Email};
use wiremock::matchers::{any, body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use crate::fixtures::{user_json, user_with_phone, users_list};
use crate::{EspioWorld, MEMBER_ID, Outcome, TOKEN};

// ==============================================================================
// GIVEN - mounting the mocked API
// ==============================================================================

#[given("a mocked Solidarity Tech API")]
async fn mocked_api(_world: &mut EspioWorld) {
    // The server is started in `EspioWorld::new`; this anchors the Background.
}

#[given(expr = "Solidarity Tech has one user with the email {string}")]
async fn one_user_by_email(world: &mut EspioWorld, email: String) {
    let user = user_json(1001, &email, serde_json::json!({}));
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", email.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given(expr = "Solidarity Tech has one user with the phone {string}")]
async fn one_user_by_phone(world: &mut EspioWorld, phone: String) {
    let user = user_with_phone(1002, &phone);
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("phone_number", phone.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has a user with a discord-handle and discord-user-id property")]
async fn user_with_props(world: &mut EspioWorld) {
    let user = user_json(
        4242,
        "espio@example.com",
        serde_json::json!({ "discord-handle": "espio", "discord-user-id": "987654321" }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "espio@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has a user with monthly and yearly dues-status properties")]
async fn user_with_dues_props(world: &mut EspioWorld) {
    // The property keys and values here are the real wire contract: the keys
    // match the serde renames on `StCustomProps`, and the values match the
    // `DuesStatusRaw` decode arms on the client.
    let user = user_json(
        7001,
        "dues@example.com",
        serde_json::json!({ "monthly-dues-status": "active", "yearly-dues-status": "past_due" }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dues@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has a user with an unrecognized dues-status value")]
async fn user_with_unknown_dues(world: &mut EspioWorld) {
    let user = user_json(
        7002,
        "dues@example.com",
        serde_json::json!({ "monthly-dues-status": "Surprise" }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dues@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given(
    "Solidarity Tech has a user with x-date, membership-type, and membership-status properties"
)]
async fn user_with_verification_props(world: &mut EspioWorld) {
    // Keys match the serde renames on `StCustomProps`; values match the client's
    // decode arms - a `YYYY-MM-DD` date, one of the four membership-type values,
    // and an exact "Membership Status" string.
    let user = user_json(
        7003,
        "dues@example.com",
        serde_json::json!({
            "x-date": "2026-12-31",
            "membership-type": "yearly",
            "membership-status": "Member in Good Standing",
        }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dues@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has a user with an unrecognized membership-type value")]
async fn user_with_unknown_type(world: &mut EspioWorld) {
    let user = user_json(
        7004,
        "dues@example.com",
        serde_json::json!({ "membership-type": "weekly" }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dues@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has a user with a retired membership-status value")]
async fn user_with_retired_status(world: &mut EspioWorld) {
    let user = user_json(
        7005,
        "dues@example.com",
        serde_json::json!({ "membership-status": "Constitutional Member" }),
    );
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dues@example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given(expr = "Solidarity Tech has a user matching the email {string} and the phone {string}")]
async fn user_by_email_and_phone(world: &mut EspioWorld, email: String, phone: String) {
    // The mock matches only when BOTH query params are present, so a client that
    // dropped one or split into two requests would miss it (and `expect(1)` fails).
    let user = user_json(1003, &email, serde_json::json!({}));
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", email.as_str()))
        .and(query_param("phone_number", phone.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![user])))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech returns both matches on one bounded page")]
async fn matches_on_one_page(world: &mut EspioWorld) {
    // `find_members` reads a single `_limit`-bounded page (it does not paginate);
    // the matcher proves Espio asked with that page size, and the body carries both
    // records so the lookup returns every match on that one page. `total_count`
    // equals the returned count, so no truncation warning fires.
    let users = vec![
        user_json(1, "dup@example.com", serde_json::json!({})),
        user_json(2, "dup@example.com", serde_json::json!({})),
    ];
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dup@example.com"))
        .and(query_param("_limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(users)))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech returns one match but reports that more exist")]
async fn truncated_page(world: &mut EspioWorld) {
    // One row on the page, but `meta.total_count` says five matches exist. The
    // client must surface exactly the page it read (and warn) - never silently
    // fetch a second page or fabricate the missing rows. `expect(1)` proves no
    // pagination follow-up request is made.
    let body = serde_json::json!({
        "data": [user_json(1, "dup@example.com", serde_json::json!({}))],
        "meta": { "total_count": 5, "limit": 100, "offset": 0 }
    });
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("email", "dup@example.com"))
        .and(query_param("_limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech has 150 users across two pages")]
async fn users_across_two_pages(world: &mut EspioWorld) {
    // 150 users at a 100-row page size means two pages, paged by `_offset`. The
    // two mocks match `_offset=0` / `_offset=100`, so the scenario fails if the
    // offset cursor regresses.
    let page1: Vec<_> = (0..100)
        .map(|i| user_json(i, &format!("u{i}@example.com"), serde_json::json!({})))
        .collect();
    let page2: Vec<_> = (100..150)
        .map(|i| user_json(i, &format!("u{i}@example.com"), serde_json::json!({})))
        .collect();

    let body1 = serde_json::json!({
        "data": page1,
        "meta": { "total_count": 150, "limit": 100, "offset": 0 }
    });
    let body2 = serde_json::json!({
        "data": page2,
        "meta": { "total_count": 150, "limit": 100, "offset": 100 }
    });

    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("_offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body1))
        .expect(1)
        .named("users page 1")
        .mount(&world.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("_offset", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body2))
        .expect(1)
        .named("users page 2")
        .mount(&world.server)
        .await;
}

#[given("Solidarity Tech defines several custom user properties")]
async fn defines_properties(world: &mut EspioWorld) {
    let body = serde_json::json!({
        "data": [
            {"id": 1, "name": "Discord Handle", "key": "discord-handle", "field_type": "single_line_text"},
            {"id": 2, "name": "Discord User ID", "key": "discord-user-id", "field_type": "single_line_text"}
        ],
        "meta": {"total_count": 2, "limit": 100, "offset": 0}
    });
    Mock::given(method("GET"))
        .and(path("/custom_user_properties"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&world.server)
        .await;
}

// ==============================================================================
// WHEN - Espio calls the client
// ==============================================================================

#[when("Espio makes any Solidarity Tech request")]
async fn makes_any_request(world: &mut EspioWorld) {
    let expected = format!("Bearer {TOKEN}");
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(header("authorization", expected.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![])))
        .expect(1)
        .mount(&world.server)
        .await;

    let email = "espio@example.com".parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when(expr = "Espio finds members by the email {string}")]
async fn finds_by_email(world: &mut EspioWorld, email: String) {
    let email = email.parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when(expr = "Espio finds members by the phone {string}")]
async fn finds_by_phone(world: &mut EspioWorld, phone: String) {
    let phone = phone.parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_phone(&phone).await));
}

#[when(expr = "Espio finds members by the email {string} and the phone {string}")]
async fn finds_by_email_and_phone(world: &mut EspioWorld, email: String, phone: String) {
    let email = email.parse().unwrap();
    let phone = phone.parse().unwrap();
    world.last = Some(Outcome::Members(
        world.client.find_members(Some(&email), Some(&phone)).await,
    ));
}

#[when("Espio finds members with neither an email nor a phone")]
async fn finds_with_neither(world: &mut EspioWorld) {
    // Guard against any leaked request: an empty-criteria lookup must short-
    // circuit before the wire.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&world.server)
        .await;

    world.last = Some(Outcome::Members(
        world.client.find_members(None, None).await,
    ));
}

#[when("Espio finds that member")]
async fn finds_that_member(world: &mut EspioWorld) {
    let email = "espio@example.com".parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when("Espio finds that dues member")]
async fn finds_that_dues_member(world: &mut EspioWorld) {
    let email = "dues@example.com".parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when("Espio finds the members")]
async fn finds_the_members(world: &mut EspioWorld) {
    let email = "dup@example.com".parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when("Espio stamps a Discord handle and id onto a member")]
async fn stamps_identity(world: &mut EspioWorld) {
    let expected_body = serde_json::json!({
        "custom_user_properties": { "discord-handle": "espio", "discord-user-id": "987654321" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    let handle: DiscordHandle = "espio".parse().unwrap();
    world.last = Some(Outcome::Write(
        world
            .client
            .set_discord_identity(MEMBER_ID, &handle, DiscordUserId(987654321))
            .await,
    ));
}

#[when("Espio clears a member's Discord handle and id")]
async fn clears_identity(world: &mut EspioWorld) {
    let expected_body = serde_json::json!({
        "custom_user_properties": { "discord-handle": "", "discord-user-id": "" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    world.last = Some(Outcome::Write(
        world
            .client
            .clear_discord_identity(
                MEMBER_ID,
                StClearFlags {
                    handle: true,
                    user_id: true,
                },
            )
            .await,
    ));
}

#[when("Espio clears only a member's Discord handle")]
async fn clears_handle_only(world: &mut EspioWorld) {
    // The merge writes only the handle key; the user-id property is left intact.
    let expected_body = serde_json::json!({
        "custom_user_properties": { "discord-handle": "" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    world.last = Some(Outcome::Write(
        world
            .client
            .clear_discord_identity(
                MEMBER_ID,
                StClearFlags {
                    handle: true,
                    user_id: false,
                },
            )
            .await,
    ));
}

#[when("Espio clears only a member's Discord user id")]
async fn clears_user_id_only(world: &mut EspioWorld) {
    let expected_body = serde_json::json!({
        "custom_user_properties": { "discord-user-id": "" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    world.last = Some(Outcome::Write(
        world
            .client
            .clear_discord_identity(
                MEMBER_ID,
                StClearFlags {
                    handle: false,
                    user_id: true,
                },
            )
            .await,
    ));
}

#[when("Espio clears neither Discord property")]
async fn clears_neither(world: &mut EspioWorld) {
    // With no flag set there is nothing to write, so no request may leave.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&world.server)
        .await;

    world.last = Some(Outcome::Write(
        world
            .client
            .clear_discord_identity(
                MEMBER_ID,
                StClearFlags {
                    handle: false,
                    user_id: false,
                },
            )
            .await,
    ));
}

#[when("Espio lists the custom user properties")]
async fn lists_properties(world: &mut EspioWorld) {
    world.last = Some(Outcome::Properties(
        world.client.list_custom_user_properties().await,
    ));
}

#[when("Espio lists all members")]
async fn lists_all_members(world: &mut EspioWorld) {
    let result = async {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = world.client.members_page(cursor.as_deref()).await?;
            out.extend(page.members);
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(out)
    }
    .await;
    world.last = Some(Outcome::Members(result));
}

#[when("Espio stamps only a Discord handle onto a member")]
async fn stamps_handle_only(world: &mut EspioWorld) {
    // A handle-only write: the merge sends just the discord-handle key, so the
    // existing discord-user-id property is left intact.
    let expected_body = serde_json::json!({
        "custom_user_properties": { "discord-handle": "espio" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    let handle: DiscordHandle = "espio".parse().unwrap();
    world.last = Some(Outcome::Write(
        world.client.set_discord_handle(MEMBER_ID, &handle).await,
    ));
}

#[when("Espio stamps an alternate email onto a member")]
async fn stamps_alternate_email(world: &mut EspioWorld) {
    // An alternate-email-only write: the merge sends just the alternate-email key,
    // leaving every other custom property intact.
    let expected_body = serde_json::json!({
        "custom_user_properties": { "alternate-email": "alt@example.com" }
    });
    Mock::given(method("PUT"))
        .and(path(format!("/users/{MEMBER_ID}")))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 4242})))
        .expect(1)
        .mount(&world.server)
        .await;

    let alternate_email: Email = "alt@example.com".parse().unwrap();
    world.last = Some(Outcome::Write(
        world
            .client
            .set_alternate_email(MEMBER_ID, &alternate_email)
            .await,
    ));
}

// ==============================================================================
// THEN - assert the outcome and verify the request shape
// ==============================================================================

#[then("the request carries the bearer authorization header")]
async fn carries_bearer(world: &mut EspioWorld) {
    // The mock in the `when` step matched only on `Bearer <token>`; verifying its
    // expectation proves the header was sent exactly.
    assert!(
        world.members().is_ok(),
        "request failed: {:?}",
        world.members()
    );
    world.server.verify().await;
}

#[then("that user is returned")]
async fn that_user_returned(world: &mut EspioWorld) {
    let members = world.members().as_ref().expect("lookup failed");
    assert_eq!(members.len(), 1, "expected exactly one match");
    world.server.verify().await;
}

#[then("the lookup fails because no identifier was given")]
async fn fails_no_identifier(world: &mut EspioWorld) {
    let err = world
        .members()
        .as_ref()
        .expect_err("lookup unexpectedly succeeded");
    assert!(
        matches!(err, SolidarityTechError::NoQueryCriteria),
        "expected NoQueryCriteria, got {err:?}"
    );
    world.server.verify().await;
}

#[then("the member carries the Discord handle and Discord user id from those properties")]
async fn member_carries_identity(world: &mut EspioWorld) {
    let members = world.members().as_ref().expect("lookup failed");
    let m = members.first().expect("no member returned");
    assert_eq!(m.discord_handle.as_ref().map(|h| h.as_str()), Some("espio"));
    assert_eq!(m.discord_user_id, Some(DiscordUserId(987654321)));
    world.server.verify().await;
}

#[then("the member carries the monthly and yearly dues status from those properties")]
async fn member_carries_dues(world: &mut EspioWorld) {
    let members = world.members().as_ref().expect("lookup failed");
    let m = members.first().expect("no member returned");
    assert_eq!(m.monthly_dues, Some(DuesStatus::Active));
    assert_eq!(m.yearly_dues, Some(DuesStatus::Overdue));
    world.server.verify().await;
}

#[then("the lookup fails because the dues status was unrecognized")]
async fn fails_unrecognized_dues(world: &mut EspioWorld) {
    let err = world
        .members()
        .as_ref()
        .expect_err("lookup unexpectedly succeeded");
    assert!(
        matches!(err, SolidarityTechError::UnknownDuesStatus(_)),
        "expected UnknownDuesStatus, got {err:?}"
    );
    world.server.verify().await;
}

#[then(
    "the member carries the expiry date, membership type, and membership standing from those properties"
)]
async fn member_carries_verification(world: &mut EspioWorld) {
    let members = world.members().as_ref().expect("lookup failed");
    let m = members.first().expect("no member returned");
    assert_eq!(m.xdate, NaiveDate::from_ymd_opt(2026, 12, 31));
    assert_eq!(m.membership_type, Some(MembershipType::Yearly));
    assert_eq!(
        m.membership_standing,
        Some(MigsStatus::MemberInGoodStanding)
    );
    world.server.verify().await;
}

#[then("the lookup fails because the membership type was unrecognized")]
async fn fails_unrecognized_type(world: &mut EspioWorld) {
    let err = world
        .members()
        .as_ref()
        .expect_err("lookup unexpectedly succeeded");
    assert!(
        matches!(err, SolidarityTechError::UnknownMembershipType(_)),
        "expected UnknownMembershipType, got {err:?}"
    );
    world.server.verify().await;
}

#[then("the lookup fails because the membership status could not be decoded")]
async fn fails_bad_standing(world: &mut EspioWorld) {
    let err = world
        .members()
        .as_ref()
        .expect_err("lookup unexpectedly succeeded");
    assert!(
        matches!(err, SolidarityTechError::BadMembershipStanding(_)),
        "expected BadMembershipStanding, got {err:?}"
    );
    world.server.verify().await;
}

#[then("both matches are returned")]
async fn both_matches_returned(world: &mut EspioWorld) {
    let members = world.members().as_ref().expect("lookup failed");
    assert_eq!(members.len(), 2, "expected both matches to be returned");
    world.server.verify().await;
}

#[then("only the page it read is returned and no further page is fetched")]
async fn only_page_read(world: &mut EspioWorld) {
    // The client surfaces exactly the one row on the page even though the API
    // reported five matches - it reads a single page and warns rather than
    // paginating. `expect(1)` on the mock (verified here) proves no second request.
    let members = world.members().as_ref().expect("lookup failed");
    assert_eq!(members.len(), 1, "expected only the single returned row");
    world.server.verify().await;
}

#[then("the Solidarity Tech update sets the discord-handle and discord-user-id properties")]
async fn update_sets_properties(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("set_discord_identity failed"),
        _ => panic!("last call was not a write"),
    };
    // The PUT body matcher in the `when` step pinned both keys; verify it ran.
    world.server.verify().await;
}

#[then(expr = "all {int} members are returned across both pages")]
async fn all_members_returned(world: &mut EspioWorld, count: usize) {
    let members = world.members().as_ref().expect("list_all_members failed");
    assert_eq!(members.len(), count, "expected {count} members");
    // Both page mocks declared `.expect(1)`.
    world.server.verify().await;
}

#[then("the Solidarity Tech update sets only the alternate-email property")]
async fn update_sets_alternate_email_only(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("set_alternate_email failed"),
        _ => panic!("last call was not a write"),
    };
    // The PUT body matcher (only the alternate-email key) + `.expect(1)` pinned it.
    world.server.verify().await;
}

#[then("the Solidarity Tech update sets only the discord-handle property")]
async fn update_sets_handle_only(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("set_discord_handle failed"),
        _ => panic!("last call was not a write"),
    };
    // The PUT body matcher (only the discord-handle key) + `.expect(1)` pinned it.
    world.server.verify().await;
}

#[then("the Solidarity Tech update blanks the discord-handle and discord-user-id properties")]
async fn update_blanks_properties(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("clear_discord_identity failed"),
        _ => panic!("last call was not a write"),
    };
    world.server.verify().await;
}

#[then("only the discord-handle property is blanked")]
async fn blanks_handle_only(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("clear_discord_identity failed"),
        _ => panic!("last call was not a write"),
    };
    // The PUT body matcher in the `when` step pinned a body with only the
    // discord-handle key; verifying its `expect(1)` proves the user-id was omitted.
    world.server.verify().await;
}

#[then("only the discord-user-id property is blanked")]
async fn blanks_user_id_only(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("clear_discord_identity failed"),
        _ => panic!("last call was not a write"),
    };
    world.server.verify().await;
}

#[then("no Solidarity Tech request is made")]
async fn no_request_made(world: &mut EspioWorld) {
    match world.last.as_ref().expect("no write was made") {
        Outcome::Write(r) => r.as_ref().expect("dry-run write errored"),
        _ => panic!("last call was not a write"),
    };
    // The `expect(0)` mock would panic on drop if any request had been sent;
    // verifying makes the zero-call expectation explicit.
    world.server.verify().await;
}

#[then("the defined properties are returned")]
async fn properties_returned(world: &mut EspioWorld) {
    let props = match world.last.as_ref().expect("no listing was made") {
        Outcome::Properties(r) => r.as_ref().expect("list_custom_user_properties failed"),
        _ => panic!("last call was not a property listing"),
    };
    assert_eq!(props.len(), 2);
    assert_eq!(props[0].name, "Discord Handle");
    assert_eq!(props[0].key, "discord-handle");
    assert_eq!(props[1].name, "Discord User ID");
    assert_eq!(props[1].key, "discord-user-id");
    world.server.verify().await;
}
