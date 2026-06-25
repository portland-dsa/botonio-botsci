//! The `/setup` panel embed: the current guild configuration, each unset value
//! shown as "not set" rather than a blank, mentioning roles and channels so a
//! moderator can read them at a glance.

use engine::store::GuildConfig;
use serenity::all::CreateEmbed;

/// One role line's value: a role mention, or "not set".
fn role_line(id: Option<domain::DiscordRoleId>) -> String {
    id.map(|r| format!("<@&{}>", r.0))
        .unwrap_or_else(|| "_not set_".to_owned())
}

/// One channel line's value: a channel mention, or "not set".
fn channel_line(id: Option<domain::DiscordChannelId>) -> String {
    id.map(|c| format!("<#{}>", c.0))
        .unwrap_or_else(|| "_not set_".to_owned())
}

/// Build the current-config embed.
pub fn config_embed(cfg: &GuildConfig, accent: u32) -> CreateEmbed {
    CreateEmbed::new()
        .title("Bot configuration")
        .colour(accent)
        .field("Moderator role", role_line(cfg.moderator_role), false)
        .field("Member role", role_line(cfg.member_role), false)
        .field("Dues-expired role", role_line(cfg.dues_expired_role), false)
        .field("Unverified role", role_line(cfg.unverified_role), false)
        .field(
            "Manual Override role",
            role_line(cfg.manual_override_role),
            false,
        )
        .field(
            "Mod-approval channel",
            channel_line(cfg.mod_approval_channel),
            false,
        )
        .field(
            "Unverified channel",
            channel_line(cfg.unverified_channel),
            false,
        )
        .field(
            "Dues-expired channel",
            channel_line(cfg.dues_expired_channel),
            false,
        )
        .field(
            "Verification-log channel",
            channel_line(cfg.verification_log_channel),
            false,
        )
        .field(
            "Dues-reminder channel",
            channel_line(cfg.dues_reminder_channel),
            false,
        )
        .field(
            "Dues sign-up URL",
            cfg.dues_signup_url
                .as_deref()
                .unwrap_or("_not set_")
                .to_owned(),
            false,
        )
        .field(
            "Dues reminders",
            if cfg.reminders_enabled { "On" } else { "Off" },
            false,
        )
        .field(
            "Scheduled scan",
            if cfg.scan_enabled { "On" } else { "Off" },
            false,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::DiscordRoleId;

    #[test]
    fn unset_role_reads_not_set() {
        assert_eq!(role_line(None), "_not set_");
        assert_eq!(role_line(Some(DiscordRoleId(42))), "<@&42>");
    }
}
