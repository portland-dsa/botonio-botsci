//! JSON response-body builders for the Solidarity Tech mock scenarios.

pub(crate) fn user_json(
    id: u64,
    email: &str,
    custom_props: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "email": email,
        "phone_number": null,
        "custom_user_properties": custom_props,
    })
}

pub(crate) fn user_with_phone(id: u64, phone: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "email": "phone-user@example.com",
        "phone_number": phone,
        "custom_user_properties": {},
    })
}

pub(crate) fn users_list(users: Vec<serde_json::Value>) -> serde_json::Value {
    let total = users.len();
    serde_json::json!({
        "data": users,
        "meta": { "total_count": total, "limit": 100, "offset": 0 }
    })
}
