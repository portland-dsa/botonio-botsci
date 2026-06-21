//! Embeds for the manual-verify flow: the initial prompt that hosts the "Look up by
//! email" button, and the in-place edits for each outcome. One ephemeral message carries
//! all of them, so they share a builder and differ only by colour and body.

use serenity::all::{CreateEmbed, CreateEmbedAuthor};

use domain::Role;

use crate::render::card::{COLOR_AMBER, COLOR_GREEN, COLOR_RED};

/// A neutral action-prompt colour (Discord blurple) for the pre-lookup state.
const COLOR_BLURPLE: u32 = 0x58_65_f2;

/// The Manual Override accent (violet): approved by hand, not matched in Solidarity Tech.
const COLOR_VIOLET: u32 = 0x92_56_e0;

/// Which state of the manual-verify exchange an embed renders.
pub enum VerifyState {
    /// Automatic match missed; invite the moderator to look up by email.
    Prompt,
    /// Matched and assigned this standing role.
    Verified(Role),
    /// The email matched no record; offer a retry.
    NotFound,
    /// The email is on record for a different account.
    Conflict,
    /// The typed value was not a plausible email; offer a retry.
    InvalidEmail,
    /// The interaction sat idle past the collector timeout.
    Expired,
    /// Hand-approved past Solidarity Tech: granted `Member` plus the Manual Override marker.
    Overridden,
    /// An unexpected backend failure.
    Error,
}

/// Build the manual-verify embed for `state`, headed by the target's avatar, display
/// name, and handle. The colour echoes the membership card: green for a good-standing
/// `Member`, red for a lapsed `Dues Expired` or a conflict, amber for a recoverable
/// not-found / invalid input.
pub fn state_embed(
    display_name: &str,
    handle: &str,
    avatar_url: &str,
    state: &VerifyState,
) -> CreateEmbed {
    let (color, body) = match state {
        VerifyState::Prompt => (
            COLOR_BLURPLE,
            "I couldn't find them in our records by their Discord information. \
             Know their email? Look them up:"
                .to_string(),
        ),
        VerifyState::Verified(role) => {
            let color = match role {
                Role::Member => COLOR_GREEN,
                Role::DuesExpired => COLOR_RED,
                Role::Unverified => COLOR_AMBER,
            };
            (color, format!("\u{2705} Verified as {}.", role.as_str()))
        }
        VerifyState::NotFound => (
            COLOR_AMBER,
            "\u{2753} No Solidarity Tech record matches that email. Want to try a different one?"
                .to_string(),
        ),
        VerifyState::Conflict => (
            COLOR_RED,
            "\u{26a0}\u{fe0f} That email is on record for a different Discord account. \
             Nothing was changed - please check the records in Solidarity Tech manually."
                .to_string(),
        ),
        VerifyState::InvalidEmail => (
            COLOR_AMBER,
            "That doesn't look like an email address. Want to try again?".to_string(),
        ),
        VerifyState::Expired => (
            COLOR_BLURPLE,
            "This timed out. Run /verify again when you're ready.".to_string(),
        ),
        VerifyState::Overridden => (
            COLOR_VIOLET,
            "\u{2705} Hand approved, not matched with any Solidarity Tech member record.\n\n\
             Your approval has been logged, along with the time of approval for future review"
                .to_string(),
        ),
        VerifyState::Error => (
            COLOR_RED,
            "Something went wrong on my end - please try again in a moment.".to_string(),
        ),
    };
    CreateEmbed::new()
        .author(CreateEmbedAuthor::new(display_name).icon_url(avatar_url))
        .colour(color)
        .description(format!("`@{handle}`\n\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(state: VerifyState) -> serde_json::Value {
        serde_json::to_value(state_embed(
            "Rosy the Rascal",
            "rosytherascal",
            "http://a/x.png",
            &state,
        ))
        .unwrap()
    }

    fn color(v: &serde_json::Value) -> u64 {
        v["color"].as_u64().unwrap()
    }
    fn desc(v: &serde_json::Value) -> String {
        v["description"].as_str().unwrap().to_string()
    }

    #[test]
    fn verified_member_is_green() {
        let v = json(VerifyState::Verified(Role::Member));
        assert_eq!(color(&v), COLOR_GREEN as u64);
        assert!(desc(&v).contains("Verified as Member"));
    }

    #[test]
    fn verified_dues_expired_is_red() {
        let v = json(VerifyState::Verified(Role::DuesExpired));
        assert_eq!(color(&v), COLOR_RED as u64);
        assert!(desc(&v).contains("Verified as Dues Expired"));
    }

    #[test]
    fn not_found_is_amber_and_invites_retry() {
        let v = json(VerifyState::NotFound);
        assert_eq!(color(&v), COLOR_AMBER as u64);
        assert!(desc(&v).contains("try a different one"));
    }

    #[test]
    fn conflict_is_red_and_names_a_different_account() {
        let v = json(VerifyState::Conflict);
        assert_eq!(color(&v), COLOR_RED as u64);
        assert!(desc(&v).contains("different Discord account"));
    }

    #[test]
    fn every_state_carries_the_handle() {
        for state in [
            VerifyState::Prompt,
            VerifyState::Verified(Role::Member),
            VerifyState::NotFound,
            VerifyState::Conflict,
            VerifyState::InvalidEmail,
            VerifyState::Expired,
            VerifyState::Overridden,
            VerifyState::Error,
        ] {
            assert!(desc(&json(state)).contains("@rosytherascal"));
        }
    }

    #[test]
    fn overridden_is_violet_and_carries_the_copy() {
        let v = json(VerifyState::Overridden);
        assert_eq!(color(&v), COLOR_VIOLET as u64);
        assert!(desc(&v).contains("Hand approved"));
        assert!(desc(&v).contains("logged"));
    }
}
