//! The `axum` app: one read-only `GET /users` route over the served roster.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use backends::solidarity_tech::fixtures::users_page;
use serde::Deserialize;
use serde_json::Value;

use crate::persona::Persona;
use crate::roster::MockKey;

/// The `GET /users` query parameters the mock honors. `_limit`/`_offset` page the
/// roster; `email` filters it (the manual verify path sends `?email=`). `user_list_ids`
/// is intentionally not a field: the mock has exactly one list, so that filter is
/// ignored (serde skips unknown query params) and any list query returns the whole
/// roster.
#[derive(Deserialize)]
struct UsersQuery {
    #[serde(rename = "_limit")]
    limit: Option<u32>,
    #[serde(rename = "_offset")]
    offset: Option<u32>,
    /// Solidarity Tech's `find_members` email filter; absent matches the whole roster.
    email: Option<String>,
}

/// Build the router over the parsed persona map. The roster JSON is rebuilt per
/// request (see [`list_users`]) so date-relative personas stay current on a
/// long-lived server. Unmatched paths get axum's default `404`.
pub fn router(personas: Arc<Vec<(MockKey, Persona)>>) -> Router {
    Router::new()
        .route("/users", get(list_users))
        .with_state(personas)
}

async fn list_users(
    State(personas): State<Arc<Vec<(MockKey, Persona)>>>,
    Query(q): Query<UsersQuery>,
) -> Json<Value> {
    // Rebuild against today's date each request, so date-relative personas (e.g.
    // amber's renewal window) don't go stale as the server ages.
    let roster = crate::roster::records(&personas, chrono::Local::now().date_naive());
    // Apply the email filter the live `/users?email=` would, so `find_by_email` reads
    // the 0-1 matching rows rather than the whole roster.
    let roster = match &q.email {
        Some(wanted) => filter_by_email(&roster, wanted),
        None => roster,
    };
    let limit = q.limit.unwrap_or(100) as usize;
    let offset = q.offset.unwrap_or(0) as usize;
    let (page, total) = paginate(&roster, limit, offset);
    Json(users_page(page, total, limit as u32, offset as u32))
}

/// The roster records whose top-level `email` equals `wanted`, case-insensitively -
/// the mock's stand-in for Solidarity Tech's server-side `?email=` filter. A record
/// with a `null` email (the malformed persona) never matches.
fn filter_by_email(roster: &[Value], wanted: &str) -> Vec<Value> {
    roster
        .iter()
        .filter(|r| {
            r["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(wanted))
        })
        .cloned()
        .collect()
}

/// One page of the roster: the `offset..offset+limit` slice plus the full count,
/// matching the live client's `_offset`/`_limit` paging.
fn paginate(roster: &[Value], limit: usize, offset: usize) -> (Vec<Value>, usize) {
    let page = roster.iter().skip(offset).take(limit).cloned().collect();
    (page, roster.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roster(n: usize) -> Vec<Value> {
        (0..n).map(|i| json!({ "id": i })).collect()
    }

    #[test]
    fn first_page_returns_the_window_and_total() {
        let r = roster(250);
        let (page, total) = paginate(&r, 100, 0);
        assert_eq!(total, 250);
        assert_eq!(page.len(), 100);
        assert_eq!(page[0]["id"], 0);
    }

    #[test]
    fn second_page_offsets() {
        let r = roster(250);
        let (page, _) = paginate(&r, 100, 100);
        assert_eq!(page[0]["id"], 100);
        assert_eq!(page.len(), 100);
    }

    #[test]
    fn offset_past_the_end_is_empty() {
        let r = roster(3);
        let (page, total) = paginate(&r, 100, 100);
        assert!(page.is_empty());
        assert_eq!(total, 3);
    }

    #[test]
    fn email_filter_matches_case_insensitively_and_excludes_the_rest() {
        let roster = vec![
            json!({ "id": 1, "email": "Found@Persona.test" }),
            json!({ "id": 2, "email": "other@persona.test" }),
            json!({ "id": 3, "email": null }),
        ];
        let hit = filter_by_email(&roster, "found@persona.test");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0]["id"], 1);
        assert!(filter_by_email(&roster, "nobody@persona.test").is_empty());
    }
}
