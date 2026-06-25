//! Pure builders for the dues-reminder thread messages: the per-milestone message with
//! its deadline line and renew/help/snooze/opt-out buttons, and the built-in default
//! template bodies a moderator can override.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateMessage, UserId,
};

use engine::reminders::{Milestone, ReminderTemplateKind};

pub const DUES_SNOOZE_ID: &str = "dues_snooze";
pub const DUES_OPTOUT_ID: &str = "dues_optout";
pub const DUES_HELP_ID: &str = "dues_help";

/// Whether a stored dues sign-up URL is a usable web link. Discord rejects a link button whose
/// URL lacks an http(s) scheme, so such a value must never reach `CreateButton::new_link` - it
/// would fail the whole message send. A deliberately lenient scheme check, not full validation.
pub fn is_http_signup_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The built-in body for a template kind, used when a moderator has not set one.
pub fn default_template(kind: ReminderTemplateKind) -> &'static str {
    match kind {
        ReminderTemplateKind::Monthly => {
            "Your monthly dues are coming up for renewal. \
            Keeping them current keeps your access and supports the chapter."
        }
        ReminderTemplateKind::Yearly => {
            "Your yearly membership is up for renewal soon. \
            Renew to keep your standing and access."
        }
        ReminderTemplateKind::OneTime => {
            "Your membership is set to lapse soon. Renew to \
            keep your access."
        }
        ReminderTemplateKind::IncomeBased => {
            "Your income-based membership is up for \
            renewal soon. Renew to keep your standing and access."
        }
        ReminderTemplateKind::Expired => {
            "Your membership has lapsed, so your access has \
            been limited. Renew any time to restore it - and use the button below if you \
            need a hand."
        }
    }
}

/// The bot-owned urgency line, from the true remaining days.
pub fn deadline_line(days_until: i64) -> String {
    match days_until {
        d if d < 0 => "Your membership has lapsed.".to_owned(),
        0 => "Your membership lapses today.".to_owned(),
        1 => "Your membership lapses tomorrow.".to_owned(),
        d => format!("Your membership lapses in {d} days."),
    }
}

/// Embed for one reminder milestone. Combined with `reminder_buttons` to build a full message.
pub fn reminder_embed(body: &str, days_until: i64, accent: u32) -> CreateEmbed {
    CreateEmbed::new()
        .title("Dues renewal")
        .description(format!("{}\n\n{body}", deadline_line(days_until)))
        .color(accent)
}

/// Button row for one reminder milestone. `signup_url` adds a Renew link button when set.
/// `disabled` greys out the interactive (ask-an-admin / snooze / opt-out) buttons so a
/// preview can show the layout without their live handlers firing; the Renew link has no
/// handler, so it stays active.
pub fn reminder_buttons(
    milestone: Milestone,
    signup_url: Option<&str>,
    member: UserId,
    disabled: bool,
) -> Vec<CreateButton> {
    let mut buttons = Vec::new();
    // Only attach the Renew link when the stored URL is a usable http(s) link: a malformed value
    // would make Discord reject the whole message, so a bad stored URL degrades to a missing
    // button rather than a failed send.
    if let Some(url) = signup_url.filter(|u| is_http_signup_url(u)) {
        buttons.push(CreateButton::new_link(url).label("Renew"));
    }
    buttons.push(
        CreateButton::new(format!("{DUES_HELP_ID}:{}", member.get()))
            .label("Ask an admin for help")
            .style(ButtonStyle::Secondary)
            .disabled(disabled),
    );
    if milestone != Milestone::Expired {
        buttons.push(
            CreateButton::new(format!("{DUES_SNOOZE_ID}:{}", member.get()))
                .label("Not this cycle")
                .style(ButtonStyle::Secondary)
                .disabled(disabled),
        );
        buttons.push(
            CreateButton::new(format!("{DUES_OPTOUT_ID}:{}", member.get()))
                .label("Stop dues reminders")
                .style(ButtonStyle::Danger)
                .disabled(disabled),
        );
    }
    buttons
}

/// The thread message for one milestone. `signup_url` adds a Renew link button when set.
pub fn reminder_message(
    body: &str,
    days_until: i64,
    milestone: Milestone,
    signup_url: Option<&str>,
    member: UserId,
    accent: u32,
) -> CreateMessage {
    CreateMessage::new()
        .content(format!("<@{}>", member.get()))
        .embed(reminder_embed(body, days_until, accent))
        .components(vec![CreateActionRow::Buttons(reminder_buttons(
            milestone, signup_url, member, false,
        ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_line_negative() {
        assert_eq!(deadline_line(-5), "Your membership has lapsed.");
    }

    #[test]
    fn deadline_line_zero() {
        assert_eq!(deadline_line(0), "Your membership lapses today.");
    }

    #[test]
    fn deadline_line_one() {
        assert_eq!(deadline_line(1), "Your membership lapses tomorrow.");
    }

    #[test]
    fn deadline_line_n() {
        assert_eq!(deadline_line(14), "Your membership lapses in 14 days.");
    }

    #[test]
    fn default_template_expired_non_empty() {
        let body = default_template(ReminderTemplateKind::Expired);
        assert!(!body.is_empty());
    }
}
