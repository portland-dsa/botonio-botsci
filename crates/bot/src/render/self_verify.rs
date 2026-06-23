//! Pure builders for self-service verification: the standing prompt posted in the
//! unverified channel, the email modal it opens, and the moderator-log embed for a
//! successful grant. No gateway - serialized in unit tests to lock their shape.

// allow(dead_code): these builders have no in-binary caller of their own - the
// self-service interaction handler that uses them is wired in the bot's gateway layer.
#![allow(dead_code)]

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

/// The moderator-log embed for a successful self-verification: who verified, the
/// granted role, the email that matched, and - when the submitted name does not
/// line up with the record - a visible warning to eyeball.
pub fn log_embed(
    member: UserId,
    handle: &str,
    record_name: Option<&str>,
    role: Role,
    email: &str,
    name_warning: bool,
    accent: u32,
) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title("Self-service verification")
        .description(format!(
            "<@{}> ({handle}) verified as {}.",
            member.get(),
            role.as_str()
        ))
        .field("Record name", record_name.unwrap_or("(none on file)"), true)
        .field("Granted role", role.as_str(), true)
        .field("Email", email, false)
        .color(accent);
    if name_warning {
        embed = embed.field(
            "\u{26a0} Name mismatch",
            "The submitted name does not match the record name - double-check this is the right person.",
            false,
        );
    }
    embed
}
