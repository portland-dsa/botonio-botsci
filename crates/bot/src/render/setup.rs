//! The `/setup` landing embed: a by-feature readiness summary ("Bot setup") that tells
//! a moderator at a glance which features are configured and which still need attention.
//!
//! Why only the landing carries an embed: each feature page shows its current values
//! inside its own select menus (the selects pre-select the configured role/channel), so
//! a flat field dump repeated on every page would be noise. The landing is the one place
//! a whole-config overview earns its space.

use engine::store::GuildConfig;
use serenity::all::CreateEmbed;

/// A feature's readiness marker: configured (green check) or needs attention (warning).
/// Emoji, not text, so the state reads as colour at a glance the way the design intends.
fn mark(ready: bool) -> &'static str {
    if ready {
        "\u{2705}" // white check mark on green
    } else {
        "\u{26a0}\u{fe0f}" // warning sign (amber)
    }
}

/// The per-feature status summary shown on the landing, one line per feature. Kept pure
/// (and separate from [`landing_embed`]) so the readiness rules are unit-testable without
/// reaching into a [`CreateEmbed`]. Dues reminders reports its on/off state explicitly,
/// since a fully-configured feature can still be switched off.
fn summary(cfg: &GuildConfig) -> String {
    let verification = cfg.unverified_role.is_some() && cfg.unverified_channel.is_some();
    let membership = cfg.member_role.is_some() && cfg.dues_expired_role.is_some();
    let moderation = cfg.moderator_role.is_some();

    let dues = if !cfg.reminders_enabled {
        format!("{} Dues reminders - off", mark(false))
    } else if cfg.dues_expired_channel.is_none() {
        format!("{} Dues reminders - needs a channel", mark(false))
    } else {
        format!("{} Dues reminders", mark(true))
    };

    // Two features per line, mirroring the landing's 2-per-row nav buttons:
    //   Verification        Membership & access
    //   Dues reminders      Moderation
    const GAP: &str = "\u{2003}\u{2003}"; // em spaces - a wide gap Discord preserves
    let (v, m, md) = (mark(verification), mark(membership), mark(moderation));
    format!("{v} Verification{GAP}{m} Membership & access\n{dues}{GAP}{md} Moderation")
}

/// Build the landing summary embed - the title plus the [`summary`] readiness lines.
/// The feature pages render with no embed; their selects carry the current values.
pub fn landing_embed(cfg: &GuildConfig, accent: u32) -> CreateEmbed {
    CreateEmbed::new()
        .title("Bot setup")
        .colour(accent)
        .description(summary(cfg))
}

/// The Dues reminders page's caption embed - the one feature page that carries an embed.
/// The four per-type editor buttons need it: Discord cannot label a button row, so their
/// purpose (editing the member-facing reminder text) would otherwise be implicit.
pub fn dues_page_embed(accent: u32) -> CreateEmbed {
    CreateEmbed::new()
        .title("Dues reminders")
        .colour(accent)
        .description(
            "Use the Monthly, Yearly, One-time, and Income-based buttons to edit the \
             reminder a member of that type sees.",
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DiscordChannelId, DiscordRoleId};

    #[test]
    fn empty_config_flags_every_feature() {
        // A fresh guild has nothing set and reminders off: every feature reads
        // needs-attention, and dues spells out the off state.
        let s = summary(&GuildConfig::default());
        assert!(s.starts_with(&format!("{} Verification", mark(false))));
        assert!(s.contains(&format!("{} Membership & access", mark(false))));
        assert!(s.contains("Dues reminders - off"));
        assert!(s.contains(&format!("{} Moderation", mark(false))));
    }

    #[test]
    fn configured_features_read_ready() {
        let cfg = GuildConfig {
            unverified_role: Some(DiscordRoleId(1)),
            unverified_channel: Some(DiscordChannelId(2)),
            member_role: Some(DiscordRoleId(3)),
            dues_expired_role: Some(DiscordRoleId(4)),
            moderator_role: Some(DiscordRoleId(5)),
            dues_expired_channel: Some(DiscordChannelId(6)),
            reminders_enabled: true,
            ..Default::default()
        };
        let s = summary(&cfg);
        assert!(s.contains(&format!("{} Verification", mark(true))));
        assert!(s.contains(&format!("{} Membership & access", mark(true))));
        assert!(s.contains(&format!("{} Dues reminders", mark(true))));
        assert!(!s.contains("Dues reminders - off"));
        assert!(s.contains(&format!("{} Moderation", mark(true))));
    }

    #[test]
    fn reminders_on_without_channel_needs_attention() {
        let cfg = GuildConfig {
            reminders_enabled: true,
            dues_expired_channel: None,
            ..Default::default()
        };
        assert!(summary(&cfg).contains("Dues reminders - needs a channel"));
    }
}
