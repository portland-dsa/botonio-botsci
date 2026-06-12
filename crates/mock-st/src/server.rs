//! The `axum` app: one read-only `GET /users` route over the served roster.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use backends::solidarity_tech::fixtures::users_page;
use serde::Deserialize;
use serde_json::Value;

/// The `GET /users` paging parameters. `user_list_ids` is intentionally not a
/// field: the mock has exactly one list, so the filter is ignored (serde skips
/// unknown query params) and any list query returns the whole roster.
#[derive(Deserialize)]
struct UsersQuery {
    #[serde(rename = "_limit")]
    limit: Option<u32>,
    #[serde(rename = "_offset")]
    offset: Option<u32>,
}

/// Build the router over an already-built roster. Unmatched paths get axum's
/// default `404`.
pub fn router(roster: Arc<Vec<Value>>) -> Router {
    Router::new()
        .route("/users", get(list_users))
        .with_state(roster)
}

async fn list_users(
    State(roster): State<Arc<Vec<Value>>>,
    Query(q): Query<UsersQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(100) as usize;
    let offset = q.offset.unwrap_or(0) as usize;
    let (page, total) = paginate(&roster, limit, offset);
    Json(users_page(page, total, limit as u32, offset as u32))
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
}
