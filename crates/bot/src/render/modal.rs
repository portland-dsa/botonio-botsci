//! The manual-verify email modal, its dynamic title, and the email parse the bot runs
//! on the typed value before it ever reaches Solidarity Tech. Pure builders, so the
//! render test below can lock the modal's shape by serializing it, with no gateway.

use serenity::all::{CreateActionRow, CreateInputText, CreateModal, InputTextStyle};

use engine::util::Email;

/// The text-input component id inside the modal; read back off the submission.
pub const EMAIL_FIELD_ID: &str = "email";

/// The modal's own custom id; the collector filters submissions by it.
pub const EMAIL_MODAL_ID: &str = "verify_email_modal";

/// Discord's hard cap on a modal title, in characters.
const TITLE_MAX: usize = 45;

/// `"Verify {display_name}"`, truncated to Discord's 45-character modal-title cap.
///
/// A name long enough to overflow is cut and marked with a trailing `...` so the title is
/// always accepted by the API rather than rejected for length.
pub fn verify_title(display_name: &str) -> String {
    let full = format!("Verify {display_name}");
    if full.chars().count() <= TITLE_MAX {
        return full;
    }
    let kept: String = full.chars().take(TITLE_MAX - 3).collect();
    format!("{kept}...")
}

/// The manual-verify modal: a single required **Member email** field, titled for the
/// member being verified. `custom_id` is normally [`EMAIL_MODAL_ID`].
pub fn email_modal(custom_id: &str, display_name: &str) -> CreateModal {
    CreateModal::new(custom_id, verify_title(display_name)).components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Member email", EMAIL_FIELD_ID)
                .placeholder("name@example.com")
                .required(true)
                .min_length(1)
                .max_length(254),
        ),
    ])
}

/// Minimal sanity-check on a typed email before it is sent to Solidarity Tech: trimmed,
/// no whitespace, at most 254 characters, one `@` with a non-empty local part and a
/// dotted domain. Catches obvious typos so the bot can prompt a retry without a wasted API
/// read; full validity is Solidarity Tech's to confirm by finding (or not) a record.
pub fn parse_email(raw: &str) -> Option<Email> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 254 || s.chars().any(char::is_whitespace) {
        return None;
    }
    let (local, domain) = s.split_once('@')?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') || domain.contains('@') {
        return None;
    }
    Some(Email(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(m: CreateModal) -> serde_json::Value {
        serde_json::to_value(&m).unwrap()
    }

    #[test]
    fn modal_is_a_single_required_email_field() {
        let v = json(email_modal(EMAIL_MODAL_ID, "Rosy the Rascal"));
        assert_eq!(v["title"], "Verify Rosy the Rascal");
        let rows = v["components"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let field = &rows[0]["components"][0];
        assert_eq!(field["label"], "Member email");
        assert_eq!(field["custom_id"], EMAIL_FIELD_ID);
        assert_eq!(field["style"], 1); // SHORT
        assert_eq!(field["required"], true);
        assert_eq!(field["max_length"], 254);
    }

    #[test]
    fn title_is_truncated_to_the_cap() {
        let long = "Rosy the Rascal ".repeat(10);
        let title = verify_title(&long);
        assert_eq!(title.chars().count(), 45);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn parse_email_accepts_a_plain_address() {
        assert_eq!(
            parse_email("  rosy@example.com "),
            Some(Email("rosy@example.com".into()))
        );
    }

    #[test]
    fn parse_email_rejects_obvious_garbage() {
        assert!(parse_email("").is_none());
        assert!(parse_email("not-an-email").is_none());
        assert!(parse_email("@nolocal.com").is_none());
        assert!(parse_email("no@domain").is_none());
        assert!(parse_email("has space@x.com").is_none());
        assert!(parse_email("a@@b.com").is_none());
    }
}
