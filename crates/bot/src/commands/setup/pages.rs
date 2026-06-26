//! The per-page component layouts and the small button/select builders they share.
//!
//! Each `*_page` returns the [`CreateActionRow`]s for one feature page, read against the
//! current [`GuildConfig`]; [`page_components`] and [`page_embeds`] pick the right set for a
//! [`Page`]. The custom-id constants and [`Page`] come from the hub via `use super::*`.

use serenity::all::{
    ButtonStyle, ChannelId, ChannelType, CreateActionRow, CreateButton, CreateEmbed,
    CreateSelectMenu, CreateSelectMenuKind, RoleId,
};

use domain::{DiscordChannelId, DiscordRoleId};
use engine::store::GuildConfig;

use crate::render::setup::{dues_page_embed, landing_embed};

use super::*;

/// The Landing view's buttons: one row per pair of feature pages, all blurple.
pub(super) fn landing_page() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::Buttons(vec![
            CreateButton::new(NAV_VERIFICATION_ID)
                .label("Verification")
                .style(ButtonStyle::Primary),
            CreateButton::new(NAV_MEMBERSHIP_ID)
                .label("Membership & access")
                .style(ButtonStyle::Primary),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(NAV_DUES_ID)
                .label("Dues reminders")
                .style(ButtonStyle::Primary),
            CreateButton::new(NAV_MODERATION_ID)
                .label("Moderation")
                .style(ButtonStyle::Primary),
        ]),
    ]
}

/// Verification page: unverified role + the three verification channels, then a row of
/// blurple actions (edit/post the prompt) and grey Back. Five rows, at Discord's cap.
fn verification_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(UNVERIFIED_ROLE_ID, "Unverified role", cfg.unverified_role),
        channel_select(
            UNVERIFIED_CHAN_ID,
            "Unverified channel (the prompt is posted here)",
            cfg.unverified_channel,
        ),
        channel_select(
            MOD_CHAN_ID,
            "Mod-approval channel",
            cfg.mod_approval_channel,
        ),
        channel_select(
            VERIFY_LOG_CHAN_ID,
            "Verification-log channel",
            cfg.verification_log_channel,
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_PROMPT_ID)
                .label("Edit prompt message")
                .style(ButtonStyle::Primary),
            publish_button(
                POST_PROMPT_ID,
                cfg.unverified_prompt.is_some(),
                "Post prompt",
                "Update prompt",
            ),
            back_button(),
        ]),
    ]
}

/// Membership & access page: the four "who-is-what" role selects plus grey Back.
fn membership_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(MEMBER_ROLE_ID, "Member role", cfg.member_role),
        role_select(DUES_ROLE_ID, "Dues Expired role", cfg.dues_expired_role),
        role_select(
            DUES_EXPIRING_ROLE_ID,
            "Dues Expiring role",
            cfg.dues_expiring_role,
        ),
        role_select(
            OVERRIDE_ROLE_ID,
            "Manual Override role",
            cfg.manual_override_role,
        ),
        back_row(),
    ]
}

/// Dues reminders page: the thread-parent channel select; a row of four blurple per-type
/// message editors; a row of blurple "Edit channel message" + "Post channel message" + grey
/// "Set sign-up URL"; the Reminders toggle on its own row; grey Back. Five rows, at the cap.
/// The channel hosts the renewal + lapse threads, so it must be visible to both Member and
/// Dues Expired (see the help text).
fn dues_page(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        channel_select(
            DUES_CHAN_ID,
            "Dues-expired channel (visible to Member + Dues Expired)",
            cfg.dues_expired_channel,
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_MONTHLY_ID)
                .label("Monthly")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_YEARLY_ID)
                .label("Yearly")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_ONETIME_ID)
                .label("One-time")
                .style(ButtonStyle::Primary),
            CreateButton::new(EDIT_INCOME_ID)
                .label("Income-based")
                .style(ButtonStyle::Primary),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(EDIT_BANNER_ID)
                .label("Edit channel message")
                .style(ButtonStyle::Primary),
            publish_button(
                POST_BANNER_ID,
                cfg.dues_banner.is_some(),
                "Post channel message",
                "Update message",
            ),
            CreateButton::new(DUES_URL_BUTTON_ID)
                .label("Set sign-up URL")
                .style(ButtonStyle::Secondary),
        ]),
        CreateActionRow::Buttons(vec![toggle_button(
            REMINDERS_TOGGLE_ID,
            "Reminders",
            cfg.reminders_enabled,
        )]),
        back_row(),
    ]
}

/// Moderation page: the moderator role select, then a row with the Automatic Membership
/// Checks toggle and grey Back.
fn moderation_page(cfg: &GuildConfig, sso_deploy_enabled: bool) -> Vec<CreateActionRow> {
    let mut toggles = vec![toggle_button(
        SCAN_TOGGLE_ID,
        "Automatic Membership Checks",
        cfg.scan_enabled,
    )];
    // The SSO toggle shows only when the deploy enables SSO (BOT_SSO_ENABLED) - the other
    // half of the two-gate model. Without it the per-guild toggle would be inert, so it is
    // hidden rather than shown as a dead control.
    if sso_deploy_enabled {
        toggles.push(toggle_button(SSO_TOGGLE_ID, "SSO", cfg.sso_enabled));
    }
    toggles.push(back_button());
    vec![
        role_select(MOD_ROLE_ID, "Moderator role", cfg.moderator_role),
        CreateActionRow::Buttons(toggles),
    ]
}

/// The components for `page`, read against the current config.
pub(super) fn page_components(
    page: Page,
    cfg: &GuildConfig,
    sso_deploy_enabled: bool,
) -> Vec<CreateActionRow> {
    match page {
        Page::Landing => landing_page(),
        Page::Verification => verification_page(cfg),
        Page::Membership => membership_page(cfg),
        Page::Dues => dues_page(cfg),
        Page::Moderation => moderation_page(cfg, sso_deploy_enabled),
    }
}

/// The embed(s) for `page`: only the landing carries one (the readiness summary). The
/// feature pages show their values in their own selects, so they render with an empty
/// embed list - which also clears the landing's embed when navigating into a page.
pub(super) fn page_embeds(page: Page, cfg: &GuildConfig, accent: u32) -> Vec<CreateEmbed> {
    match page {
        Page::Landing => vec![landing_embed(cfg, accent)],
        Page::Dues => vec![dues_page_embed(accent)],
        _ => Vec::new(),
    }
}

// ====================================================================
// Shared component builders
// ====================================================================

fn back_button() -> CreateButton {
    CreateButton::new(BACK_ID)
        .label("Back")
        .style(ButtonStyle::Secondary)
}

fn back_row() -> CreateActionRow {
    CreateActionRow::Buttons(vec![back_button()])
}

/// A single-button toggle: grey (Secondary) when off, green (Success) when on, the label
/// suffixed with its current state. Pressing it flips the underlying setting.
fn toggle_button(id: &str, label: &str, enabled: bool) -> CreateButton {
    let (style, state) = if enabled {
        (ButtonStyle::Success, "ON")
    } else {
        (ButtonStyle::Secondary, "OFF")
    };
    CreateButton::new(id)
        .label(format!("{label}: {state}"))
        .style(style)
}

/// The blurple publish button for a standing message. Reads `post_label` until the message
/// has been posted once (a stored [`MessageRef`](engine::store::MessageRef) in the config),
/// then `update_label` - a press re-publishes by editing that message in place rather than
/// posting a duplicate.
fn publish_button(id: &str, posted: bool, post_label: &str, update_label: &str) -> CreateButton {
    CreateButton::new(id)
        .label(if posted { update_label } else { post_label })
        .style(ButtonStyle::Primary)
}

/// The disable-SSO confirmation rendered into the panel in place of the Moderation page:
/// the verbatim warning as an embed plus a red "Yes, disable SSO" and a grey "Cancel".
/// Disabling SSO removes the admin-panel login path, so it is gated behind this explicit
/// confirm; enabling needs none and never reaches here.
pub(super) fn sso_disable_confirm(accent: u32) -> (CreateEmbed, Vec<CreateActionRow>) {
    let embed = CreateEmbed::new()
        .title("Disable SSO?")
        .description(SSO_DISABLE_WARNING)
        .color(accent);
    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(SSO_DISABLE_CONFIRM_ID)
            .label("Yes, disable SSO")
            .style(ButtonStyle::Danger),
        CreateButton::new(SSO_DISABLE_CANCEL_ID)
            .label("Cancel")
            .style(ButtonStyle::Secondary),
    ]);
    (embed, vec![buttons])
}

fn role_select(id: &str, placeholder: &str, current: Option<DiscordRoleId>) -> CreateActionRow {
    let kind = CreateSelectMenuKind::Role {
        default_roles: current.map(|r| vec![RoleId::new(r.0)]),
    };
    CreateActionRow::SelectMenu(CreateSelectMenu::new(id, kind).placeholder(placeholder))
}

fn channel_select(
    id: &str,
    placeholder: &str,
    current: Option<DiscordChannelId>,
) -> CreateActionRow {
    let kind = CreateSelectMenuKind::Channel {
        channel_types: Some(vec![ChannelType::Text]),
        default_channels: current.map(|c| vec![ChannelId::new(c.0)]),
    };
    CreateActionRow::SelectMenu(CreateSelectMenu::new(id, kind).placeholder(placeholder))
}
