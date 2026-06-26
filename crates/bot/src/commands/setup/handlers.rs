//! The work a non-modal interaction triggers: the two feature toggles, the publish/update
//! posting actions, and applying one role/channel selection.
//!
//! Each handler clones the live [`GuildConfig`], mutates one field, persists the whole row,
//! swaps the live handle, and audits the change - then leaves the redraw to the hub's
//! [`redraw`]. Shared vocabulary (the custom-id constants, [`Page`], [`ack`], [`redraw`],
//! [`resolve_body`]) comes from the hub via `use super::*`.

use serenity::all::{ChannelId, ComponentInteraction, ComponentInteractionDataKind, MessageId};

use domain::{DiscordChannelId, DiscordMessageId, DiscordRoleId};
use engine::audit::AuditLog;
use engine::backends::util::DiscordUserId;
use engine::reminders::MessageKind;
use engine::store::{ConfigStore, GuildConfig, MessageRef};

use crate::render::reminders::{banner_edit, banner_message};
use crate::render::self_verify::{verify_prompt, verify_prompt_edit};

use super::*;

// ====================================================================
// Toggle handlers
// ====================================================================

/// Flip the Automatic Membership Checks (scheduled scan) toggle, persist, audit, and
/// redraw the Moderation page. Warns if the scan is enabled with no mod-approval channel
/// for the safety alert.
pub(super) async fn toggle_scan(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    if !ack(ctx, interaction).await {
        return;
    }
    let data = ctx.data();
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let was_enabled = cfg.scan_enabled;
    cfg.scan_enabled = !was_enabled;
    let no_mod_channel = cfg.mod_approval_channel.is_none();
    let note = if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        let now_enabled = cfg.scan_enabled;
        data.guild_config.store(std::sync::Arc::new(cfg));
        // Attribute the toggle like every other config change (see `apply_selection`):
        // enabling a background job that can demote roles is a mod action to log.
        if let Err(e) = data
            .auditor
            .record(
                invoker,
                invoker,
                "config.set_scan",
                serde_json::json!({
                    "field": "automatic membership checks",
                    "old": was_enabled,
                    "new": now_enabled
                }),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to audit config change");
        }
        // Turning the checks on with no mod-approval channel leaves the mass-demote
        // tripwire alert nowhere to post; warn rather than enable it silently.
        if now_enabled && no_mod_channel {
            Some(
                "Automatic membership checks on. Note: no mod-approval channel is set, so the \
                 safety alert can't be posted if a check is paused - set one under Verification."
                    .to_owned(),
            )
        } else {
            None
        }
    };
    redraw(ctx, interaction, accent, Page::Moderation, note.as_deref()).await;
}

/// Flip the dues-reminders toggle, persist, audit, and redraw the Dues reminders page.
/// On a flip to off, sweep the Dues Expiring marker off every currently-marked member so
/// none is stranded with it. Warns if enabled with no dues-expired channel.
pub(super) async fn toggle_reminders(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    if !ack(ctx, interaction).await {
        return;
    }
    let data = ctx.data();
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let was_enabled = cfg.reminders_enabled;
    cfg.reminders_enabled = !was_enabled;
    let note = if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        let now_enabled = cfg.reminders_enabled;
        data.guild_config.store(std::sync::Arc::new(cfg));
        if let Err(e) = data
            .auditor
            .record(
                invoker,
                invoker,
                "config.set_reminders",
                serde_json::json!({
                    "field": "dues reminders",
                    "old": was_enabled,
                    "new": now_enabled
                }),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to audit config change");
        }
        if now_enabled {
            // Turning reminders on with no dues-expired channel set means threads have
            // nowhere to post; warn rather than enable it silently.
            if data.guild_config.load().dues_expired_channel.is_none() {
                Some(
                    "Dues reminders on. Note: no dues-expired channel is set - set one on this \
                     page so reminder threads have a home."
                        .to_owned(),
                )
            } else {
                None
            }
        } else {
            // Turning reminders off: clear the Dues Expiring marker from every member that
            // still holds it, so it does not linger once the sweep stops reconciling it.
            cleanup_markers_on_off(ctx).await;
            Some(
                "Dues reminders off. Cleared the Dues Expiring marker from any held members."
                    .to_owned(),
            )
        }
    };
    redraw(ctx, interaction, accent, Page::Dues, note.as_deref()).await;
}

/// Sweep the Dues Expiring marker off every currently-marked member after reminders are
/// disabled. Builds the role writer from the saved config; if the managed roles are not
/// fully configured there is nothing to write, so it is a no-op.
async fn cleanup_markers_on_off(ctx: &Context<'_>) {
    let data = ctx.data();
    let Some(discord) = data.role_writer() else {
        tracing::debug!("reminders off: managed roles not configured; no markers to clear");
        return;
    };
    crate::reminders::cleanup_expiring_markers(
        &data.store,
        &discord,
        data.config.guild(),
        data.config.scan_pace,
    )
    .await;
}

// ====================================================================
// SSO toggle (Moderation page)
// ====================================================================

/// The SSO enable/disable toggle. Enabling flips immediately, like the other toggles.
/// Disabling is the lockout-risk direction - it removes the admin-panel login path - so
/// instead of flipping it renders the confirm view into the panel; the disable itself
/// happens in [`confirm_disable_sso`] once the moderator confirms.
pub(super) async fn toggle_sso(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    if ctx.data().guild_config.load().sso_enabled {
        // Disabling: show the confirm view in place. A direct message update (like `nav`),
        // not an `ack` + `redraw`; the flip waits for the confirm button.
        let (embed, components) = super::pages::sso_disable_confirm(accent);
        let message = CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(components);
        if let Err(e) = interaction
            .create_response(
                ctx.serenity_context(),
                CreateInteractionResponse::UpdateMessage(message),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to show the SSO disable confirm; continuing");
        }
        return;
    }
    // Enabling: immediate, mirroring the other toggles.
    set_sso_enabled(ctx, interaction, accent, invoker, true, None).await;
}

/// Apply a confirmed SSO disable (the "Yes, disable SSO" button), then redraw the
/// Moderation page noting the change.
pub(super) async fn confirm_disable_sso(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
) {
    set_sso_enabled(
        ctx,
        interaction,
        accent,
        invoker,
        false,
        Some("SSO disabled."),
    )
    .await;
}

/// Set `sso_enabled` to `new`, persist, audit, and redraw the Moderation page. Shared by
/// the immediate enable path and the confirmed-disable path; mirrors the other toggles.
async fn set_sso_enabled(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    invoker: DiscordUserId,
    new: bool,
    success_note: Option<&str>,
) {
    if !ack(ctx, interaction).await {
        return;
    }
    let data = ctx.data();
    let guild = data.config.guild();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let was_enabled = cfg.sso_enabled;
    cfg.sso_enabled = new;
    let note = if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        Some("Something went wrong saving that - please try again.".to_owned())
    } else {
        data.guild_config.store(std::sync::Arc::new(cfg));
        if let Err(e) = data
            .auditor
            .record(
                invoker,
                invoker,
                "config.set_sso",
                serde_json::json!({ "field": "sso", "old": was_enabled, "new": new }),
            )
            .await
        {
            tracing::warn!(error = %e, "setup: failed to audit config change");
        }
        success_note.map(str::to_owned)
    };
    redraw(ctx, interaction, accent, Page::Moderation, note.as_deref()).await;
}

// ====================================================================
// Posting actions
// ====================================================================

/// Which standing message a posted reference belongs to. Selects the [`GuildConfig`] slot
/// [`save_message_ref`] writes.
#[derive(Clone, Copy)]
enum PostedMessage {
    Prompt,
    Banner,
}

/// Publish the verification prompt: edit the standing message in place if one was posted
/// before, otherwise post a fresh one to the configured unverified channel and remember it.
/// Any edit failure (most often the message was deleted) falls back to a fresh post. Returns
/// a short note for the moderator.
pub(super) async fn post_prompt(ctx: &Context<'_>, accent: u32) -> Option<String> {
    let data = ctx.data();
    let body = resolve_body(ctx, MessageKind::Unverified).await;
    let (existing, configured_channel) = {
        let cfg = data.guild_config.load();
        (cfg.unverified_prompt, cfg.unverified_channel)
    };

    if let Some(r) = existing {
        match ChannelId::new(r.channel.0)
            .edit_message(
                ctx.serenity_context().http.clone(),
                MessageId::new(r.message.0),
                verify_prompt_edit(&body, accent),
            )
            .await
        {
            Ok(_) => return Some("Updated the verification prompt in place.".to_owned()),
            Err(e) => tracing::warn!(
                error = %e,
                "setup: couldn't edit the existing verification prompt; posting a fresh one"
            ),
        }
    }

    let Some(channel) = configured_channel else {
        return Some(
            "Set an unverified channel first (on this page), then post the prompt.".to_owned(),
        );
    };
    match ChannelId::new(channel.0)
        .send_message(
            ctx.serenity_context().http.clone(),
            verify_prompt(&body, accent),
        )
        .await
    {
        Ok(msg) => {
            save_message_ref(
                ctx,
                PostedMessage::Prompt,
                channel,
                DiscordMessageId(msg.id.get()),
            )
            .await;
            Some("Posted the verification prompt to the unverified channel.".to_owned())
        }
        Err(e) => {
            tracing::warn!(error = %e, "setup: failed to post the verification prompt");
            Some(
                "Couldn't post the prompt there - check my permissions on that channel.".to_owned(),
            )
        }
    }
}

/// Publish the dues-expiring banner: the dues counterpart to [`post_prompt`], editing the
/// standing banner in place when one exists and otherwise posting a fresh one to the
/// configured dues-expired channel. Returns a short note for the moderator.
pub(super) async fn post_banner(ctx: &Context<'_>, accent: u32) -> Option<String> {
    let data = ctx.data();
    let body = resolve_body(ctx, MessageKind::DuesBanner).await;
    let (existing, configured_channel, signup_url) = {
        let cfg = data.guild_config.load();
        (
            cfg.dues_banner,
            cfg.dues_expired_channel,
            cfg.dues_signup_url.clone(),
        )
    };

    if let Some(r) = existing {
        match ChannelId::new(r.channel.0)
            .edit_message(
                ctx.serenity_context().http.clone(),
                MessageId::new(r.message.0),
                banner_edit(&body, signup_url.as_deref(), accent),
            )
            .await
        {
            Ok(_) => return Some("Updated the dues-expiring message in place.".to_owned()),
            Err(e) => tracing::warn!(
                error = %e,
                "setup: couldn't edit the existing dues channel message; posting a fresh one"
            ),
        }
    }

    let Some(channel) = configured_channel else {
        return Some(
            "Set a dues-expired channel first (on this page), then post the channel message."
                .to_owned(),
        );
    };
    match ChannelId::new(channel.0)
        .send_message(
            ctx.serenity_context().http.clone(),
            banner_message(&body, signup_url.as_deref(), accent),
        )
        .await
    {
        Ok(msg) => {
            save_message_ref(
                ctx,
                PostedMessage::Banner,
                channel,
                DiscordMessageId(msg.id.get()),
            )
            .await;
            Some("Posted the dues-expiring message to the dues-expired channel.".to_owned())
        }
        Err(e) => {
            tracing::warn!(error = %e, "setup: failed to post the dues channel message");
            Some(
                "Couldn't post the message there - check my permissions on that channel."
                    .to_owned(),
            )
        }
    }
}

/// Remember where a standing message was just posted, so the next publish edits it in place
/// and its button reads "Update". Persists the reference into the guild config and swaps the
/// live handle. A failed save is logged, not surfaced: the message is already posted, and a
/// missing reference only means the next publish posts a fresh copy.
async fn save_message_ref(
    ctx: &Context<'_>,
    which: PostedMessage,
    channel: DiscordChannelId,
    message: DiscordMessageId,
) {
    let data = ctx.data();
    let mut cfg = GuildConfig::clone(&data.guild_config.load());
    let slot = match which {
        PostedMessage::Prompt => &mut cfg.unverified_prompt,
        PostedMessage::Banner => &mut cfg.dues_banner,
    };
    *slot = Some(MessageRef { channel, message });
    if let Err(e) = data.store.save_config(data.config.guild(), &cfg).await {
        tracing::warn!(
            error = %e,
            "setup: posted the message but couldn't save its reference; the next publish posts fresh"
        );
        return;
    }
    data.guild_config.store(std::sync::Arc::new(cfg));
}

// ====================================================================
// Selection apply
// ====================================================================

/// Apply one role/channel selection: set the field, validate, persist, swap the live
/// handle, audit. Returns a short note for the moderator (a confirmation, or a rejection
/// reason), or `None` if the interaction was not a setting select we handle.
pub(super) async fn apply_selection(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    invoker: DiscordUserId,
) -> Option<String> {
    let data = ctx.data();
    let id = interaction.data.custom_id.as_str();
    let mut cfg: GuildConfig = GuildConfig::clone(&data.guild_config.load());

    let (label, action, old, new): (&str, &str, Option<u64>, u64) = match &interaction.data.kind {
        ComponentInteractionDataKind::RoleSelect { values } => {
            let rid = values.first()?.get();
            let (label, old) = set_role_field(&mut cfg, id, DiscordRoleId(rid))?;
            (label, "config.set_role", old, rid)
        }
        ComponentInteractionDataKind::ChannelSelect { values } => {
            let cid = values.first()?.get();
            let (label, old) = set_channel_field(&mut cfg, id, DiscordChannelId(cid))?;
            (label, "config.set_channel", old, cid)
        }
        _ => return None,
    };

    // Every configured role must stay distinct (see `roles_are_distinct`).
    if !roles_are_distinct(&cfg) {
        return Some(
            "That role is already assigned to another setting - pick a distinct role for each."
                .to_owned(),
        );
    }

    let guild = data.config.guild();
    if let Err(e) = data.store.save_config(guild, &cfg).await {
        tracing::error!(error = %e, "setup: failed to save guild config");
        return Some("Something went wrong saving that - please try again.".to_owned());
    }
    data.guild_config.store(std::sync::Arc::new(cfg));
    if let Err(e) = data
        .auditor
        .record(
            invoker,
            invoker,
            action,
            serde_json::json!({ "field": label, "old": old, "new": new }),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to audit config change");
    }
    Some(format!("Updated the {label}."))
}

/// Set the role field named by `id`, returning its display label and the previous id.
fn set_role_field(
    cfg: &mut GuildConfig,
    id: &str,
    rid: DiscordRoleId,
) -> Option<(&'static str, Option<u64>)> {
    let (label, slot): (&str, &mut Option<DiscordRoleId>) = match id {
        MOD_ROLE_ID => ("moderator role", &mut cfg.moderator_role),
        MEMBER_ROLE_ID => ("member role", &mut cfg.member_role),
        DUES_ROLE_ID => ("dues-expired role", &mut cfg.dues_expired_role),
        DUES_EXPIRING_ROLE_ID => ("dues-expiring role", &mut cfg.dues_expiring_role),
        UNVERIFIED_ROLE_ID => ("unverified role", &mut cfg.unverified_role),
        OVERRIDE_ROLE_ID => ("manual override role", &mut cfg.manual_override_role),
        _ => return None,
    };
    let old = slot.map(|r| r.0);
    *slot = Some(rid);
    Some((label, old))
}

/// Set the channel field named by `id`, returning its display label and the previous id.
fn set_channel_field(
    cfg: &mut GuildConfig,
    id: &str,
    cid: DiscordChannelId,
) -> Option<(&'static str, Option<u64>)> {
    let (label, slot): (&str, &mut Option<DiscordChannelId>) = match id {
        MOD_CHAN_ID => ("mod-approval channel", &mut cfg.mod_approval_channel),
        UNVERIFIED_CHAN_ID => ("unverified channel", &mut cfg.unverified_channel),
        DUES_CHAN_ID => ("dues-expired channel", &mut cfg.dues_expired_channel),
        VERIFY_LOG_CHAN_ID => (
            "verification-log channel",
            &mut cfg.verification_log_channel,
        ),
        _ => return None,
    };
    let old = slot.map(|c| c.0);
    *slot = Some(cid);
    Some((label, old))
}

/// Every configured role must be distinct - the moderator role included. Assigning one
/// Discord role to two settings is always a configuration mistake, and letting a managed
/// status role double as the moderator role would hand moderator access to anyone the bot
/// verifies as a member.
fn roles_are_distinct(cfg: &GuildConfig) -> bool {
    let mut seen = std::collections::HashSet::new();
    [
        cfg.moderator_role,
        cfg.member_role,
        cfg.dues_expired_role,
        cfg.dues_expiring_role,
        cfg.unverified_role,
        cfg.manual_override_role,
    ]
    .into_iter()
    .flatten()
    .all(|r| seen.insert(r.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_roles() {
        let mut cfg = GuildConfig {
            member_role: Some(DiscordRoleId(5)),
            dues_expired_role: Some(DiscordRoleId(5)),
            ..Default::default()
        };
        assert!(!roles_are_distinct(&cfg));
        cfg.dues_expired_role = Some(DiscordRoleId(6));
        cfg.unverified_role = Some(DiscordRoleId(7));
        assert!(roles_are_distinct(&cfg));
        // A managed status role must not double as the moderator role.
        cfg.moderator_role = Some(DiscordRoleId(5));
        assert!(!roles_are_distinct(&cfg));
    }

    #[test]
    fn dues_expiring_role_must_be_distinct() {
        let cfg = GuildConfig {
            member_role: Some(DiscordRoleId(5)),
            dues_expired_role: Some(DiscordRoleId(6)),
            unverified_role: Some(DiscordRoleId(7)),
            dues_expiring_role: Some(DiscordRoleId(5)), // clashes with Member
            ..Default::default()
        };
        assert!(!roles_are_distinct(&cfg));
    }

    #[test]
    fn set_role_field_reports_old_and_updates() {
        let mut cfg = GuildConfig::default();
        let (label, old) = set_role_field(&mut cfg, MEMBER_ROLE_ID, DiscordRoleId(9)).unwrap();
        assert_eq!(label, "member role");
        assert_eq!(old, None);
        assert_eq!(cfg.member_role, Some(DiscordRoleId(9)));
        let (_, old2) = set_role_field(&mut cfg, MEMBER_ROLE_ID, DiscordRoleId(10)).unwrap();
        assert_eq!(old2, Some(9));
    }

    #[test]
    fn set_role_field_handles_the_dues_expiring_role() {
        let mut cfg = GuildConfig::default();
        let (label, old) =
            set_role_field(&mut cfg, DUES_EXPIRING_ROLE_ID, DiscordRoleId(12)).unwrap();
        assert_eq!(label, "dues-expiring role");
        assert_eq!(old, None);
        assert_eq!(cfg.dues_expiring_role, Some(DiscordRoleId(12)));
    }

    #[test]
    fn set_channel_field_dues_expired() {
        let mut cfg = GuildConfig::default();
        let (label, old) = set_channel_field(&mut cfg, DUES_CHAN_ID, DiscordChannelId(20)).unwrap();
        assert_eq!(label, "dues-expired channel");
        assert_eq!(old, None);
        assert_eq!(cfg.dues_expired_channel, Some(DiscordChannelId(20)));
    }
}
