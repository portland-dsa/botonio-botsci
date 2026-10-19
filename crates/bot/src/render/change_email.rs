//! The email-change reply: one ephemeral embed reporting how moving a member onto the record
//! under a new email went. It has no buttons, so unlike the verify embeds no state offers a
//! retry - a moderator runs the command again.

use serenity::all::CreateEmbed;

use engine::verify::RelinkOutcome;

use crate::render::card::{COLOR_AMBER, COLOR_RED};
use crate::render::verify::{COLOR_BLURPLE, member_embed, role_color, standing_note};

/// What the email-change reply reports: the engine's outcome, or a failure before or during it.
pub enum ChangeEmailState {
    /// The change ran to an outcome.
    Done(RelinkOutcome),
    /// The typed value was not a plausible email, so nothing was looked up.
    InvalidEmail,
    /// An unexpected backend failure.
    Error,
}

/// Build the email-change embed for `state`, headed like the verify embeds.
pub fn change_email_embed(
    display_name: &str,
    handle: &str,
    avatar_url: &str,
    state: &ChangeEmailState,
) -> CreateEmbed {
    let (color, body) = match state {
        ChangeEmailState::Done(RelinkOutcome::Relinked { role, standing }) => (
            role_color(*role),
            format!(
                "\u{2705} Linked to their record under the new email and set to {}.{}",
                role.as_str(),
                standing_note(*standing)
            ),
        ),
        ChangeEmailState::Done(RelinkOutcome::Malformed) => (
            COLOR_AMBER,
            "\u{26a0}\u{fe0f} Linked to their record under the new email, but it has no \
             membership status, so I couldn't set a role. Run /verify on them to hand-approve."
                .to_owned(),
        ),
        ChangeEmailState::Done(RelinkOutcome::AlreadyLinked) => (
            COLOR_BLURPLE,
            "They're already linked to the record under that email - nothing to change.".to_owned(),
        ),
        ChangeEmailState::Done(RelinkOutcome::Conflict) => (
            COLOR_RED,
            "\u{26a0}\u{fe0f} That email is on record for a different Discord account. \
             Nothing was changed - please check the records in Solidarity Tech manually."
                .to_owned(),
        ),
        ChangeEmailState::Done(RelinkOutcome::NotFound) => (
            COLOR_AMBER,
            "\u{2753} No Solidarity Tech record matches that email. Nothing was changed."
                .to_owned(),
        ),
        ChangeEmailState::InvalidEmail => (
            COLOR_AMBER,
            "That doesn't look like an email address. Nothing was changed.".to_owned(),
        ),
        ChangeEmailState::Error => (
            COLOR_RED,
            "Something went wrong on my end - please try again in a moment.".to_owned(),
        ),
    };
    member_embed(display_name, handle, avatar_url, color, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Role;

    #[test]
    fn each_state_has_its_colour() {
        let relinked = |role| {
            ChangeEmailState::Done(RelinkOutcome::Relinked {
                role,
                standing: None,
            })
        };
        for (state, color) in [
            (relinked(Role::Member), role_color(Role::Member)),
            (relinked(Role::DuesExpired), role_color(Role::DuesExpired)),
            (
                ChangeEmailState::Done(RelinkOutcome::Malformed),
                COLOR_AMBER,
            ),
            (
                ChangeEmailState::Done(RelinkOutcome::AlreadyLinked),
                COLOR_BLURPLE,
            ),
            (ChangeEmailState::Done(RelinkOutcome::Conflict), COLOR_RED),
            (ChangeEmailState::Done(RelinkOutcome::NotFound), COLOR_AMBER),
            (ChangeEmailState::InvalidEmail, COLOR_AMBER),
            (ChangeEmailState::Error, COLOR_RED),
        ] {
            let v =
                serde_json::to_value(change_email_embed("Amy", "amy", "http://a/x.png", &state))
                    .unwrap();
            assert_eq!(v["color"].as_u64(), Some(u64::from(color)));
            assert!(v["description"].as_str().unwrap().contains("@amy"));
        }
    }
}
