//! Pure builders for the dues-reminder thread messages: the per-milestone message with
//! its deadline line and renew/help/opt-out buttons, and the built-in default
//! template bodies a moderator can override.

use chrono::{Datelike, NaiveDate};
use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateMessage, UserId,
};

use engine::reminders::{MessageKind, Milestone};

pub const DUES_OPTOUT_ID: &str = "dues_optout";
pub const DUES_HELP_ID: &str = "dues_help";
pub const DUES_BANNER_HELP_ID: &str = "dues_banner_help";

/// Whether a stored dues sign-up URL is a usable web link. Discord rejects a link button whose
/// URL lacks an http(s) scheme, so such a value must never reach `CreateButton::new_link` - it
/// would fail the whole message send. A deliberately lenient scheme check, not full validation.
pub fn is_http_signup_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The bot-owned title for a milestone notice. Renewal shows the absolute expiry date;
/// Lapse is fixed. Date is formatted portably to avoid the glibc-only `%-d`.
pub fn notice_title(milestone: Milestone, xdate: NaiveDate) -> String {
    match milestone {
        Milestone::Renewal => format!(
            "Your membership expires on {} {}, {}!",
            xdate.format("%B"),
            xdate.day(),
            xdate.year()
        ),
        Milestone::Lapse => "Your membership has lapsed!".to_owned(),
    }
}

/// The built-in body for a message kind, used when a moderator has not set one.
pub fn default_body(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Monthly => {
            "Your monthly dues are coming up for renewal. \
            Keeping them current keeps your access and supports the chapter."
        }
        MessageKind::Yearly => {
            "Your yearly membership is up for renewal soon. \
            Renew to keep your standing and access."
        }
        MessageKind::OneTime => {
            "Your membership is set to lapse soon. Renew to \
            keep your access."
        }
        MessageKind::IncomeBased => {
            "Your income-based membership is up for \
            renewal soon. Renew to keep your standing and access."
        }
        MessageKind::Unverified => {
            "Already a dues-paying member? Press the button below and enter the \
             email on your membership to get verified."
        }
        MessageKind::DuesBanner => {
            "Your membership is expiring soon. Renew now to keep your access and \
             support the chapter."
        }
    }
}

/// Button row for one reminder milestone. `signup_url` adds a grey Renew link button when set.
/// Help is blurple (Primary). Stop dues reminders (Danger) appears only on Renewal - the
/// lapse notice ignores opt-out.
pub fn reminder_buttons(
    milestone: Milestone,
    signup_url: Option<&str>,
    member: UserId,
) -> Vec<CreateButton> {
    let mut buttons = Vec::new();
    // Only attach the Renew link when the stored URL is a usable http(s) link: a malformed value
    // would make Discord reject the whole message, so a bad stored URL degrades to a missing
    // button rather than a failed send. Link buttons are always grey - this is Discord's design.
    if let Some(url) = signup_url.filter(|u| is_http_signup_url(u)) {
        buttons.push(CreateButton::new_link(url).label("Renew"));
    }
    buttons.push(
        CreateButton::new(format!("{DUES_HELP_ID}:{}", member.get()))
            .label("Get help")
            .style(ButtonStyle::Primary),
    );
    if milestone != Milestone::Lapse {
        buttons.push(
            CreateButton::new(format!("{DUES_OPTOUT_ID}:{}", member.get()))
                .label("Stop dues reminders")
                .style(ButtonStyle::Danger),
        );
    }
    buttons
}

/// The thread message for one milestone. `signup_url` adds a grey Renew link button when set.
pub fn reminder_message(
    body: &str,
    milestone: Milestone,
    xdate: NaiveDate,
    signup_url: Option<&str>,
    member: UserId,
    accent: u32,
) -> CreateMessage {
    let embed = CreateEmbed::new()
        .title(notice_title(milestone, xdate))
        .description(body)
        .color(accent);
    CreateMessage::new()
        .content(format!("<@{}>", member.get()))
        .embed(embed)
        .components(vec![CreateActionRow::Buttons(reminder_buttons(
            milestone, signup_url, member,
        ))])
}

/// The channel banner message for members inside the pre-lapse window.
/// Embed titled "Dues Expiring!", described with `body`. Buttons: grey Renew link +
/// blurple Get help (`DUES_BANNER_HELP_ID`).
pub fn banner_message(body: &str, signup_url: Option<&str>, accent: u32) -> CreateMessage {
    let embed = CreateEmbed::new()
        .title("Dues Expiring!")
        .description(body)
        .color(accent);
    let mut buttons = Vec::new();
    if let Some(url) = signup_url.filter(|u| is_http_signup_url(u)) {
        buttons.push(CreateButton::new_link(url).label("Renew"));
    }
    buttons.push(
        CreateButton::new(DUES_BANNER_HELP_ID)
            .label("Get help")
            .style(ButtonStyle::Primary),
    );
    CreateMessage::new()
        .embed(embed)
        .components(vec![CreateActionRow::Buttons(buttons)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_title_shows_absolute_date() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 9).unwrap();
        assert_eq!(
            notice_title(Milestone::Renewal, d),
            "Your membership expires on July 9, 2026!"
        );
    }

    #[test]
    fn lapse_title_is_static() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 9).unwrap();
        assert_eq!(
            notice_title(Milestone::Lapse, d),
            "Your membership has lapsed!"
        );
    }

    #[test]
    fn default_body_present_for_every_kind() {
        for k in [
            MessageKind::Monthly,
            MessageKind::Yearly,
            MessageKind::OneTime,
            MessageKind::IncomeBased,
            MessageKind::Unverified,
            MessageKind::DuesBanner,
        ] {
            assert!(!default_body(k).is_empty());
        }
    }
}
