//! Step definitions for `resilience.feature`: rate-limit backoff/retry, request
//! pacing, and surfacing 4xx/5xx responses as a Solidarity Tech error.

use cucumber::{given, then, when};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use backends::solidarity_tech::{SolidarityTechClient, SolidarityTechError};

use crate::fixtures::users_list;
use crate::{EspioWorld, Outcome};

#[given("Solidarity Tech returns 429 once and then succeeds")]
async fn rate_limited_then_ok(world: &mut EspioWorld) {
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(429).append_header("Retry-After", "1"))
        .up_to_n_times(1)
        .mount(&world.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![])))
        .mount(&world.server)
        .await;
}

#[given(expr = "Solidarity Tech responds with status {int}")]
async fn responds_with_status(world: &mut EspioWorld, status: u16) {
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(status).set_body_string("error"))
        .mount(&world.server)
        .await;
}

#[when("Espio makes a Solidarity Tech request")]
async fn makes_a_request(world: &mut EspioWorld) {
    let email = "x@y.com".parse().unwrap();
    world.last = Some(Outcome::Members(world.client.find_by_email(&email).await));
}

#[when("Espio makes five sequential Solidarity Tech requests")]
async fn five_sequential(world: &mut EspioWorld) {
    Mock::given(method("GET"))
        .and(path("/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_list(vec![])))
        .mount(&world.server)
        .await;

    let email: backends::util::Email = "x@y.com".parse().unwrap();
    let start = std::time::Instant::now();
    for _ in 0..5 {
        world.client.find_by_email(&email).await.unwrap();
    }
    world.elapsed = Some(start.elapsed());
}

#[then("the Solidarity Tech request is retried and ultimately succeeds")]
async fn retried_succeeds(world: &mut EspioWorld) {
    assert!(
        world.members().is_ok(),
        "expected success after retry, got {:?}",
        world.members()
    );
    let reqs = world.server.received_requests().await.unwrap();
    assert!(
        reqs.len() >= 2,
        "expected at least 2 requests (429 + success), got {}",
        reqs.len()
    );
}

#[then("the Solidarity Tech requests are spaced out under the rate limit")]
async fn spaced_out(world: &mut EspioWorld) {
    // 5 calls each pay the leading pacing sleep; allow generous slack for slow CI.
    let elapsed = world.elapsed.expect("no elapsed time recorded");
    assert!(
        elapsed.as_millis() >= 1500,
        "expected >=1500ms for 5 paced calls, got {}ms",
        elapsed.as_millis()
    );
}

#[then("the request fails with a Solidarity Tech error")]
async fn fails_with_error(world: &mut EspioWorld) {
    let err = world
        .members()
        .as_ref()
        .expect_err("lookup unexpectedly succeeded");
    assert!(
        matches!(err, SolidarityTechError::Status { .. }),
        "expected a Status error, got {err:?}"
    );
}
