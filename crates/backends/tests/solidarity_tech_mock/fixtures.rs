//! Thin wrappers over the crate's shared `/users` builders, so the contract
//! suite and the standalone mock serve byte-identical shapes.

pub(crate) fn user_json(
    id: u64,
    email: &str,
    custom_props: serde_json::Value,
) -> serde_json::Value {
    backends::solidarity_tech::fixtures::user_json(id, Some(email), custom_props)
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
    backends::solidarity_tech::fixtures::users_page(users, total, 100, 0)
}
