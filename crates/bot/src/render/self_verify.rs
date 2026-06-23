//! Pure builders for self-service verification: the standing prompt posted in the
//! unverified channel, the email modal it opens, and the moderator-log embed for a
//! successful grant. No gateway - serialized in unit tests to lock their shape.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateInputText, CreateMessage,
    CreateModal, InputTextStyle, UserId,
};

use domain::Role;

/// The standing prompt button: pressing it opens [`self_verify_modal`]. A constant
/// id so the button keeps working across restarts with no stored message state.
pub const PROMPT_BUTTON_ID: &str = "selfverify_open";
/// The modal's own custom id, matched in the interaction handler.
pub const SUBMIT_MODAL_ID: &str = "selfverify_submit";
pub const EMAIL_FIELD_ID: &str = "selfverify_email";
pub const FIRST_FIELD_ID: &str = "selfverify_first";
pub const LAST_FIELD_ID: &str = "selfverify_last";

/// The message posted into the unverified channel: a short explainer embed and the
/// button that opens the verification form.
pub fn verify_prompt(accent: u32) -> CreateMessage {
    let embed = CreateEmbed::new()
        .title("Verify your membership")
        .description(
            "Already a dues-paying member? Press the button below and enter the \
             email on your membership to get verified.",
        )
        .color(accent);
    CreateMessage::new()
        .embed(embed)
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(PROMPT_BUTTON_ID)
                .label("Verify me")
                .style(ButtonStyle::Primary),
        ])])
}

/// The verification form: membership email, first name, last initial. The name
/// fields are a moderator-facing double-check, not a gate (see `name_check`).
pub fn self_verify_modal() -> CreateModal {
    CreateModal::new(SUBMIT_MODAL_ID, "Verify your membership").components(vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Membership email", EMAIL_FIELD_ID)
                .placeholder("name@example.com")
                .required(true)
                .min_length(1)
                .max_length(254),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "First name", FIRST_FIELD_ID)
                .required(true)
                .min_length(1)
                .max_length(100),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Last initial", LAST_FIELD_ID)
                .required(true)
                .min_length(1)
                .max_length(4),
        ),
    ])
}

/// The fields of a successful self-verification's moderator-log embed: who verified, the
/// name they typed and the record's name (shown side by side to eyeball), the granted
/// role, and the matched email.
pub struct VerifyLog<'a> {
    pub member: UserId,
    pub handle: &'a str,
    pub submitted_name: &'a str,
    pub record_name: Option<&'a str>,
    pub role: Role,
    pub email: &'a str,
    /// The submitted name failed `name_check` - draw the eyeball warning.
    pub name_mismatch: bool,
}

/// The moderator-log embed for a successful self-verification: who verified, the
/// granted role, the email that matched, and - when the submitted name does not
/// line up with the record - a visible warning to eyeball.
pub fn log_embed(log: &VerifyLog<'_>, accent: u32) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title("Self-service verification")
        .description(format!(
            "<@{}> ({}) verified as {}.",
            log.member.get(),
            log.handle,
            log.role.as_str()
        ))
        .field("Submitted name", log.submitted_name, true)
        .field(
            "Record name",
            log.record_name.unwrap_or("(none on file)"),
            true,
        )
        .field("Granted role", log.role.as_str(), true)
        .field("Email", log.email, false)
        .color(accent);
    if log.name_mismatch {
        embed = embed.field(
            "\u{26a0} Name mismatch",
            "The submitted name does not match the record name - double-check this is the right person.",
            false,
        );
    }
    embed
}

/// The moderator-log embed for a self-verification that matched a record carrying no
/// usable standing: the member is real, but no role could be granted, so a moderator may
/// want to hand-approve them. Mirrors [`log_embed`] but names no role.
pub fn malformed_log_embed(
    member: UserId,
    handle: &str,
    submitted_name: &str,
    email: &str,
    accent: u32,
) -> CreateEmbed {
    CreateEmbed::new()
        .title("Self-service verification needs review")
        .description(format!(
            "<@{}> ({handle}) matched a membership record with no usable standing, so no \
             role was granted. Consider verifying them by hand.",
            member.get(),
        ))
        .field("Submitted name", submitted_name, true)
        .field("Email", email, false)
        .color(accent)
}
