//! Pure builders for self-service verification: the standing prompt posted in the
//! unverified channel, the email modal it opens, and the moderator-log embed for a
//! successful grant. No gateway - serialized in unit tests to lock their shape.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateInputText, CreateMessage,
    CreateModal, EditMessage, InputTextStyle, UserId,
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

/// The label of the embed field that carries the submitted email on a review request,
/// read back when a moderator presses "Re-verify now".
pub const EMAIL_FIELD_LABEL: &str = "Email";
/// The label of the embed field that carries the submitted name on a review request.
pub const SUBMITTED_NAME_FIELD_LABEL: &str = "Submitted name";

/// The review-request button custom-id kinds. Each carries the target member id as a
/// `kind:id` suffix so the handler works across restarts with no stored state; the
/// override button's modal additionally carries the review message id (`kind:id:msg`).
pub const REVIEW_REVERIFY_ID: &str = "selfverify_review_reverify";
pub const REVIEW_OVERRIDE_ID: &str = "selfverify_review_override";
pub const REVIEW_REJECT_ID: &str = "selfverify_review_reject";
pub const REVIEW_MODAL_ID: &str = "selfverify_review_modal";

/// The embed and components shared by the fresh-post [`verify_prompt`] and the in-place
/// [`verify_prompt_edit`], so the two cannot drift. Title is bot-owned; the description is
/// the stored or default body so moderators can customise the text. The button id is a
/// constant, so re-supplying it on an edit keeps the standing button working.
fn verify_prompt_parts(body: &str, accent: u32) -> (CreateEmbed, Vec<CreateActionRow>) {
    let embed = CreateEmbed::new()
        .title("Verify your membership")
        .description(body)
        .color(accent);
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(PROMPT_BUTTON_ID)
            .label("Verify me")
            .style(ButtonStyle::Primary),
    ])];
    (embed, components)
}

/// The message posted into the unverified channel: an explainer embed (body supplied by
/// the caller) and the button that opens the verification form.
pub fn verify_prompt(body: &str, accent: u32) -> CreateMessage {
    let (embed, components) = verify_prompt_parts(body, accent);
    CreateMessage::new().embed(embed).components(components)
}

/// The in-place edit of an already-posted verification prompt: the same embed and button as
/// [`verify_prompt`], shaped as an [`EditMessage`] so a re-publish updates the standing
/// message rather than posting a duplicate.
pub fn verify_prompt_edit(body: &str, accent: u32) -> EditMessage {
    let (embed, components) = verify_prompt_parts(body, accent);
    EditMessage::new().embed(embed).components(components)
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

/// Why a self-verification could not be granted automatically - shown to moderators on
/// the review request so they can decide how to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewReason {
    /// No Solidarity Tech record carried the submitted email.
    NotFound,
    /// The email is already linked to a different account.
    Conflict,
    /// A record matched but carries no usable standing.
    Malformed,
}

impl ReviewReason {
    /// The moderator-facing explanation of why the auto-grant did not happen.
    fn summary(self) -> &'static str {
        match self {
            ReviewReason::NotFound => {
                "No membership record matched the email they entered. If you add them to \
                 Solidarity Tech you can press Re-verify, or hand-approve with Manual override."
            }
            ReviewReason::Conflict => {
                "That email is already linked to a different account - confirm this is really \
                 them before approving."
            }
            ReviewReason::Malformed => {
                "A record matched but carries no usable standing. Fix it in Solidarity Tech \
                 and press Re-verify, or hand-approve with Manual override."
            }
        }
    }
}

/// The moderator-approval request posted when a self-verification could not be granted
/// automatically: who tried, the name and email they submitted, why it could not be
/// granted, and the buttons to re-check, hand-approve, or dismiss. The email rides in a
/// labelled field so the Re-verify handler can read it back with no stored state.
pub fn review_request(
    member: UserId,
    handle: &str,
    submitted_name: &str,
    email: &str,
    reason: ReviewReason,
    accent: u32,
) -> CreateMessage {
    let embed = CreateEmbed::new()
        .title("Self-service verification needs a moderator")
        .description(format!(
            "<@{}> ({handle}) tried to verify themselves but could not be matched automatically.",
            member.get(),
        ))
        .field(SUBMITTED_NAME_FIELD_LABEL, submitted_name, true)
        .field(EMAIL_FIELD_LABEL, email, true)
        .field("Why", reason.summary(), false)
        .color(accent);
    CreateMessage::new()
        .embed(embed)
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("{REVIEW_REVERIFY_ID}:{}", member.get()))
                .label("Re-verify now")
                .style(ButtonStyle::Primary),
            CreateButton::new(format!("{REVIEW_OVERRIDE_ID}:{}", member.get()))
                .label("Manual override")
                .style(ButtonStyle::Secondary),
            CreateButton::new(format!("{REVIEW_REJECT_ID}:{}", member.get()))
                .label("Reject")
                .style(ButtonStyle::Danger),
        ])])
}

/// The verification-log entry for a grant a moderator made by acting on a review request
/// (a re-verify or a manual override). Mirrors [`log_embed`] for the self-service path but
/// records who approved it and how, and carries no name double-check.
pub fn manual_log_embed(
    member: UserId,
    handle: &str,
    approver: UserId,
    how: &str,
    role: Role,
    accent: u32,
) -> CreateEmbed {
    CreateEmbed::new()
        .title("Self-service verification - approved by a moderator")
        .description(format!(
            "<@{}> ({handle}) was verified by <@{}> ({how}).",
            member.get(),
            approver.get(),
        ))
        .field("Granted role", role.as_str(), true)
        .color(accent)
}
