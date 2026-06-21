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

/// `"{verb} {display_name}"`, truncated to Discord's 45-character modal-title cap.
///
/// A name long enough to overflow is cut and marked with a trailing `...` so the title is
/// always accepted by the API rather than rejected for length.
fn modal_title(verb: &str, display_name: &str) -> String {
    let full = format!("{verb} {display_name}");
    if full.chars().count() <= TITLE_MAX {
        return full;
    }
    let kept: String = full.chars().take(TITLE_MAX - 3).collect();
    format!("{kept}...")
}

/// `"Verify {display_name}"`, truncated to Discord's modal-title cap.
pub fn verify_title(display_name: &str) -> String {
    modal_title("Verify", display_name)
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

/// The override-reason text-input id; read back off the submission.
pub const REASON_FIELD_ID: &str = "reason";

/// The override-reason modal's own custom id; the collector filters submissions by it.
pub const OVERRIDE_MODAL_ID: &str = "verify_override_modal";

/// The longest reason the modal accepts.
const REASON_MAX: u16 = 300;

/// The override-approval modal: a single optional paragraph "Reason" field, titled for the
/// member being approved. `custom_id` is normally [`OVERRIDE_MODAL_ID`].
pub fn override_modal(custom_id: &str, display_name: &str) -> CreateModal {
    CreateModal::new(custom_id, modal_title("Approve", display_name)).components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Paragraph,
                "Reason (optional)",
                REASON_FIELD_ID,
            )
            .placeholder("Why are you approving this member? (optional)")
            .required(false)
            .max_length(REASON_MAX),
        ),
    ])
}

/// Normalize a typed override reason: trim surrounding whitespace and treat an empty
/// string as no reason. The modal already caps the length.
pub fn parse_reason(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
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
    fn override_modal_is_a_single_optional_reason_field() {
        let v = json(override_modal(OVERRIDE_MODAL_ID, "Rosy the Rascal"));
        assert_eq!(v["title"], "Approve Rosy the Rascal");
        let rows = v["components"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let field = &rows[0]["components"][0];
        assert_eq!(field["label"], "Reason (optional)");
        assert_eq!(field["custom_id"], REASON_FIELD_ID);
        assert_eq!(field["style"], 2); // PARAGRAPH
        assert_eq!(field["required"], false);
        assert_eq!(field["max_length"], 300);
    }

    #[test]
    fn parse_reason_trims_and_optionalizes() {
        assert_eq!(
            parse_reason("  vouched in person "),
            Some("vouched in person".into())
        );
        assert_eq!(parse_reason("   "), None);
        assert_eq!(parse_reason(""), None);
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
