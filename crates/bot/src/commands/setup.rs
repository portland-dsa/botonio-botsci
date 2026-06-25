//! `/setup` - the Manage-Server-gated guild configuration panel. Bootstraps the
//! moderator role, so it gates on Discord's Manage Guild permission rather than the
//! bot's own (not-yet-configured) moderator role. The panel hosts two sections of
//! native role/channel select menus; a selection writes the whole config row, swaps
//! the live handle, and audits the change.

use std::time::Duration;

use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, ChannelType, ComponentInteraction,
    ComponentInteractionCollector, ComponentInteractionDataKind, CreateActionRow, CreateButton,
    CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateModal,
    CreateSelectMenu, CreateSelectMenuKind, EditInteractionResponse, InputTextStyle, Permissions,
    RoleId,
};
use serenity::futures::StreamExt as _;

use domain::{DiscordChannelId, DiscordRoleId};
use engine::audit::AuditLog;
use engine::backends::util::DiscordUserId;
use engine::store::{ConfigStore, GuildConfig};

use crate::data::{Context, Error};
use crate::render::setup::config_embed;

// Navigation buttons.
const SET_STATUS_ROLES_ID: &str = "setup_set_status_roles";
const SET_MODERATOR_ID: &str = "setup_set_moderator";
const SET_CHANNELS_ID: &str = "setup_set_channels";
const SET_DUES_ID: &str = "setup_set_dues";
const BACK_ID: &str = "setup_back";
const SCAN_TOGGLE_ID: &str = "setup_scan_toggle";
const REMINDERS_TOGGLE_ID: &str = "setup_reminders_toggle";
const DUES_URL_BUTTON_ID: &str = "setup_dues_url_button";

// Per-setting select-menu custom ids.
const MOD_ROLE_ID: &str = "setup_role_moderator";
const MEMBER_ROLE_ID: &str = "setup_role_member";
const DUES_ROLE_ID: &str = "setup_role_dues_expired";
const UNVERIFIED_ROLE_ID: &str = "setup_role_unverified";
const OVERRIDE_ROLE_ID: &str = "setup_role_manual_override";
const MOD_CHAN_ID: &str = "setup_chan_mod_approval";
const UNVERIFIED_CHAN_ID: &str = "setup_chan_unverified";
const DUES_CHAN_ID: &str = "setup_chan_dues_expired";
const VERIFY_LOG_CHAN_ID: &str = "setup_chan_verify_log";
const DUES_REMINDER_CHAN_ID: &str = "setup_chan_dues_reminder";
const POST_PROMPT_ID: &str = "setup_post_prompt";

// Dues sign-up URL modal.
const DUES_URL_MODAL_ID: &str = "setup_dues_url_modal";
const DUES_URL_FIELD_ID: &str = "setup_dues_url_field";
const DUES_URL_MODAL_TIMEOUT: Duration = Duration::from_secs(120);

const NAV_TIMEOUT: Duration = Duration::from_secs(180);

/// Whether the invoker actually holds Manage Guild. `default_member_permissions` only
/// hides the command in the client; this is the enforced gate (mirroring the moderator
/// commands, which treat the permission as a hint and check in code).
pub async fn invoker_can_configure(ctx: &Context<'_>) -> bool {
    match ctx.author_member().await {
        Some(member) => member
            .permissions
            .is_some_and(|p| p.contains(Permissions::MANAGE_GUILD)),
        None => false,
    }
}

/// Configure the bot's roles and channels. Server managers only.
#[poise::command(slash_command, default_member_permissions = "MANAGE_GUILD")]
pub async fn setup(ctx: Context<'_>) -> Result<(), Error> {
    if !invoker_can_configure(&ctx).await {
        ctx.send(
            poise::CreateReply::default()
                .content("That command is for server managers only.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let data = ctx.data();
    let invoker = DiscordUserId(ctx.author().id.get());
    let accent = data.config.accent_color;

    let handle = ctx
        .send(
            poise::CreateReply::default()
                .embed(config_embed(&data.guild_config.load(), accent))
                .components(panel_buttons())
                .ephemeral(true),
        )
        .await?;
    let msg = handle.message().await?;

    // One collector for every button and select on this ephemeral message, scoped to the
    // invoker. Each interaction either navigates between sections or applies one setting.
    let mut stream = ComponentInteractionCollector::new(ctx.serenity_context())
        .message_id(msg.id)
        .author_id(ctx.author().id)
        .timeout(NAV_TIMEOUT)
        .stream();

    while let Some(interaction) = stream.next().await {
        match interaction.data.custom_id.as_str() {
            SET_STATUS_ROLES_ID => {
                update_panel(
                    &ctx,
                    &interaction,
                    accent,
                    status_section(&data.guild_config.load()),
                    None,
                )
                .await;
            }
            SET_MODERATOR_ID => {
                update_panel(
                    &ctx,
                    &interaction,
                    accent,
                    moderator_section(&data.guild_config.load()),
                    None,
                )
                .await;
            }
            SET_CHANNELS_ID => {
                update_panel(
                    &ctx,
                    &interaction,
                    accent,
                    channel_section(&data.guild_config.load()),
                    None,
                )
                .await;
            }
            SET_DUES_ID => {
                update_panel(
                    &ctx,
                    &interaction,
                    accent,
                    dues_section(&data.guild_config.load()),
                    None,
                )
                .await;
            }
            BACK_ID => {
                update_panel(&ctx, &interaction, accent, panel_buttons(), None).await;
            }
            SCAN_TOGGLE_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
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
                                "field": "scheduled scan",
                                "old": was_enabled,
                                "new": now_enabled
                            }),
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "setup: failed to audit config change");
                    }
                    // Turning the scan on with no mod-approval channel leaves the mass-demote
                    // tripwire alert nowhere to post; warn rather than enable it silently.
                    if now_enabled && no_mod_channel {
                        Some(
                            "Scheduled scan on. Note: no mod-approval channel is set, so the \
                             safety alert can't be posted if a scan is paused - set one under \
                             Set channels."
                                .to_owned(),
                        )
                    } else {
                        None
                    }
                };
                edit_panel(&ctx, &interaction, accent, panel_buttons(), note.as_deref()).await;
            }
            REMINDERS_TOGGLE_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
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
                    // Turning reminders on with no dues-reminder channel set means threads
                    // have nowhere to post; warn rather than enable it silently.
                    if now_enabled && data.guild_config.load().dues_reminder_channel.is_none() {
                        Some(
                            "Dues reminders on. Note: no dues-reminder channel is set - set one \
                             under Dues reminders so reminder threads have a home."
                                .to_owned(),
                        )
                    } else {
                        None
                    }
                };
                edit_panel(&ctx, &interaction, accent, panel_buttons(), note.as_deref()).await;
            }
            DUES_URL_BUTTON_ID => {
                // Open a modal to collect the free-text URL, then await its submission.
                let url_modal = dues_url_modal(data.guild_config.load().dues_signup_url.as_deref());
                if let Err(e) = interaction
                    .create_response(
                        ctx.serenity_context(),
                        CreateInteractionResponse::Modal(url_modal),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "setup: failed to open dues URL modal");
                    continue;
                }
                // A dismissed modal sends no event; the timeout prevents a hang.
                let submit = match msg
                    .await_modal_interaction(ctx.serenity_context())
                    .author_id(ctx.author().id)
                    .custom_ids(vec![DUES_URL_MODAL_ID.to_owned()])
                    .timeout(DUES_URL_MODAL_TIMEOUT)
                    .await
                {
                    Some(s) => s,
                    None => continue,
                };
                if let Err(e) = submit
                    .create_response(
                        ctx.serenity_context(),
                        CreateInteractionResponse::Acknowledge,
                    )
                    .await
                {
                    tracing::warn!(error = %e, "setup: failed to acknowledge dues URL modal submit");
                    continue;
                }
                let raw_url = submit
                    .data
                    .components
                    .iter()
                    .flat_map(|row| &row.components)
                    .find_map(|c| match c {
                        ActionRowComponent::InputText(input)
                            if input.custom_id == DUES_URL_FIELD_ID =>
                        {
                            input.value.clone()
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let guild = data.config.guild();
                let mut cfg = GuildConfig::clone(&data.guild_config.load());
                let old_url = cfg.dues_signup_url.clone();
                let trimmed = raw_url.trim();
                let note = if !trimmed.is_empty()
                    && !crate::render::reminders::is_http_signup_url(trimmed)
                {
                    // A link button with a non-http(s) URL is rejected by Discord, which would
                    // fail every reminder send; refuse the value and keep the current one.
                    Some(
                        "That doesn't look like a web link - the dues sign-up URL must start with \
                         http:// or https://. Left it unchanged."
                            .to_owned(),
                    )
                } else {
                    let new_url = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_owned())
                    };
                    cfg.dues_signup_url = new_url.clone();
                    if let Err(e) = data.store.save_config(guild, &cfg).await {
                        tracing::error!(error = %e, "setup: failed to save guild config");
                        Some("Something went wrong saving that - please try again.".to_owned())
                    } else {
                        data.guild_config.store(std::sync::Arc::new(cfg));
                        if let Err(e) = data
                            .auditor
                            .record(
                                invoker,
                                invoker,
                                "config.set_dues_url",
                                serde_json::json!({
                                    "field": "dues sign-up URL",
                                    "old": old_url,
                                    "new": new_url
                                }),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "setup: failed to audit config change");
                        }
                        if old_url == new_url {
                            Some(
                                "That's already the dues sign-up URL - nothing changed.".to_owned(),
                            )
                        } else {
                            Some("Updated the dues sign-up URL.".to_owned())
                        }
                    }
                };
                let mut edit = EditInteractionResponse::new()
                    .embed(config_embed(&data.guild_config.load(), accent))
                    .components(panel_buttons());
                if let Some(ref n) = note {
                    edit = edit.content(n);
                }
                if let Err(e) = submit.edit_response(ctx.serenity_context(), edit).await {
                    tracing::warn!(error = %e, "setup: failed to update panel after dues URL change");
                }
            }
            POST_PROMPT_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = match data.guild_config.load().unverified_channel {
                    None => Some(
                        "Set an unverified channel first (under Set channels), then post the prompt."
                            .to_owned(),
                    ),
                    Some(ch) => {
                        let msg = crate::render::self_verify::verify_prompt(accent);
                        match serenity::all::ChannelId::new(ch.0)
                            .send_message(ctx.serenity_context().http.clone(), msg)
                            .await
                        {
                            Ok(_) => Some(
                                "Posted the verification prompt to the unverified channel."
                                    .to_owned(),
                            ),
                            Err(e) => {
                                tracing::warn!(error = %e, "setup: failed to post the verification prompt");
                                Some(
                                    "Couldn't post the prompt there - check my permissions on that channel."
                                        .to_owned(),
                                )
                            }
                        }
                    }
                };
                edit_panel(&ctx, &interaction, accent, panel_buttons(), note.as_deref()).await;
            }
            MEMBER_ROLE_ID | DUES_ROLE_ID | UNVERIFIED_ROLE_ID | OVERRIDE_ROLE_ID => {
                // Acknowledge first so the persist + audit below can't blow Discord's
                // 3-second response deadline; the panel is then edited in place.
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                edit_panel(
                    &ctx,
                    &interaction,
                    accent,
                    status_section(&data.guild_config.load()),
                    note.as_deref(),
                )
                .await;
            }
            MOD_ROLE_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                edit_panel(
                    &ctx,
                    &interaction,
                    accent,
                    moderator_section(&data.guild_config.load()),
                    note.as_deref(),
                )
                .await;
            }
            MOD_CHAN_ID | UNVERIFIED_CHAN_ID | DUES_CHAN_ID | VERIFY_LOG_CHAN_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                edit_panel(
                    &ctx,
                    &interaction,
                    accent,
                    channel_section(&data.guild_config.load()),
                    note.as_deref(),
                )
                .await;
            }
            DUES_REMINDER_CHAN_ID => {
                if !ack(&ctx, &interaction).await {
                    continue;
                }
                let note = apply_selection(&ctx, &interaction, invoker).await;
                edit_panel(
                    &ctx,
                    &interaction,
                    accent,
                    dues_section(&data.guild_config.load()),
                    note.as_deref(),
                )
                .await;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The summary view's buttons. Two rows: the first holds the five existing nav/action
/// buttons (at Discord's 5-per-row cap); the second holds the three dues-reminder
/// controls (channel nav, toggle, URL entry).
fn panel_buttons() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::Buttons(vec![
            CreateButton::new(SET_STATUS_ROLES_ID)
                .label("Status & override roles")
                .style(ButtonStyle::Primary),
            CreateButton::new(SET_MODERATOR_ID)
                .label("Moderator role")
                .style(ButtonStyle::Secondary),
            CreateButton::new(SET_CHANNELS_ID)
                .label("Set channels")
                .style(ButtonStyle::Secondary),
            CreateButton::new(SCAN_TOGGLE_ID)
                .label("Toggle scheduled scan")
                .style(ButtonStyle::Secondary),
            CreateButton::new(POST_PROMPT_ID)
                .label("Post verification prompt")
                .style(ButtonStyle::Secondary),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(SET_DUES_ID)
                .label("Dues-reminder channel")
                .style(ButtonStyle::Secondary),
            CreateButton::new(REMINDERS_TOGGLE_ID)
                .label("Toggle dues reminders")
                .style(ButtonStyle::Secondary),
            CreateButton::new(DUES_URL_BUTTON_ID)
                .label("Set dues sign-up URL")
                .style(ButtonStyle::Secondary),
        ]),
    ]
}

/// The membership-facing roles: the three status roles plus the additive Manual Override
/// marker, four selects and a back button (five rows, at Discord's cap).
fn status_section(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(MEMBER_ROLE_ID, "Member role", cfg.member_role),
        role_select(DUES_ROLE_ID, "Dues-expired role", cfg.dues_expired_role),
        role_select(UNVERIFIED_ROLE_ID, "Unverified role", cfg.unverified_role),
        role_select(
            OVERRIDE_ROLE_ID,
            "Manual Override role",
            cfg.manual_override_role,
        ),
        back_row(),
    ]
}

/// The moderator role, on its own page.
fn moderator_section(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        role_select(MOD_ROLE_ID, "Moderator role", cfg.moderator_role),
        back_row(),
    ]
}

/// The three channel select menus plus a back button.
fn channel_section(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        channel_select(
            MOD_CHAN_ID,
            "Mod-approval channel",
            cfg.mod_approval_channel,
        ),
        channel_select(
            UNVERIFIED_CHAN_ID,
            "Unverified channel",
            cfg.unverified_channel,
        ),
        channel_select(
            DUES_CHAN_ID,
            "Dues-expired channel",
            cfg.dues_expired_channel,
        ),
        channel_select(
            VERIFY_LOG_CHAN_ID,
            "Verification-log channel",
            cfg.verification_log_channel,
        ),
        back_row(),
    ]
}

fn back_row() -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(BACK_ID)
            .label("Back to summary")
            .style(ButtonStyle::Secondary),
    ])
}

/// The dues-reminder channel on its own page. The channel-section is at Discord's
/// 5-row cap, so this control gets a dedicated section. Help text notes that the
/// channel must be visible to both `Member` and `Dues Expired` so a member's reminder
/// thread survives their lapse demotion.
fn dues_section(cfg: &GuildConfig) -> Vec<CreateActionRow> {
    vec![
        channel_select(
            DUES_REMINDER_CHAN_ID,
            "Dues-reminder channel (visible to Member + Dues Expired)",
            cfg.dues_reminder_channel,
        ),
        back_row(),
    ]
}

/// The modal used by the "Set dues sign-up URL" button. Pre-fills the current value
/// so the moderator can see and edit it rather than re-typing from scratch.
fn dues_url_modal(current: Option<&str>) -> CreateModal {
    let input = CreateInputText::new(InputTextStyle::Short, "Dues sign-up URL", DUES_URL_FIELD_ID)
        .placeholder("https://example.org/dues")
        .required(false)
        .max_length(2000);
    let input = match current {
        Some(url) => input.value(url),
        None => input,
    };
    CreateModal::new(DUES_URL_MODAL_ID, "Set dues sign-up URL")
        .components(vec![CreateActionRow::InputText(input)])
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

/// Re-render the panel in place: the current-config embed, the given components, and an
/// optional one-line note (a confirmation or a rejection reason). Logs and continues on a
/// failed update so one transient error never ends the session.
async fn update_panel(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    components: Vec<CreateActionRow>,
    note: Option<&str>,
) {
    let embed = config_embed(&ctx.data().guild_config.load(), accent);
    let mut message = CreateInteractionResponseMessage::new()
        .embed(embed)
        .components(components);
    if let Some(note) = note {
        message = message.content(note);
    }
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::UpdateMessage(message),
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to update the panel; continuing");
    }
}

/// Acknowledge a component interaction with a deferred message update, so the database
/// write and audit that follow cannot blow Discord's 3-second response deadline (the panel
/// is then edited in place by [`edit_panel`]). Returns whether the acknowledgement landed;
/// on failure the caller skips this interaction rather than acting without feedback.
async fn ack(ctx: &Context<'_>, interaction: &ComponentInteraction) -> bool {
    if let Err(e) = interaction
        .create_response(
            ctx.serenity_context(),
            CreateInteractionResponse::Acknowledge,
        )
        .await
    {
        tracing::warn!(error = %e, "setup: failed to acknowledge the interaction; skipping");
        return false;
    }
    true
}

/// Re-render the panel after a deferred [`ack`], editing the original message in place. The
/// apply path's counterpart to [`update_panel`], which it cannot use because the interaction
/// is already acknowledged. Logs and continues on a failed edit.
async fn edit_panel(
    ctx: &Context<'_>,
    interaction: &ComponentInteraction,
    accent: u32,
    components: Vec<CreateActionRow>,
    note: Option<&str>,
) {
    let embed = config_embed(&ctx.data().guild_config.load(), accent);
    let mut edit = EditInteractionResponse::new()
        .embed(embed)
        .components(components);
    if let Some(note) = note {
        edit = edit.content(note);
    }
    if let Err(e) = interaction
        .edit_response(ctx.serenity_context(), edit)
        .await
    {
        tracing::warn!(error = %e, "setup: failed to update the panel; continuing");
    }
}

/// Apply one role/channel selection: set the field, validate, persist, swap the live
/// handle, audit. Returns a short note for the moderator (a confirmation, or a rejection
/// reason), or `None` if the interaction was not a setting select we handle.
async fn apply_selection(
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
        DUES_REMINDER_CHAN_ID => ("dues-reminder channel", &mut cfg.dues_reminder_channel),
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
    fn set_role_field_handles_the_override_role() {
        let mut cfg = GuildConfig::default();
        let (label, old) = set_role_field(&mut cfg, OVERRIDE_ROLE_ID, DiscordRoleId(12)).unwrap();
        assert_eq!(label, "manual override role");
        assert_eq!(old, None);
        assert_eq!(cfg.manual_override_role, Some(DiscordRoleId(12)));
    }

    #[test]
    fn override_role_must_be_distinct() {
        let cfg = GuildConfig {
            member_role: Some(DiscordRoleId(5)),
            dues_expired_role: Some(DiscordRoleId(6)),
            unverified_role: Some(DiscordRoleId(7)),
            manual_override_role: Some(DiscordRoleId(5)), // clashes with Member
            ..Default::default()
        };
        assert!(!roles_are_distinct(&cfg));
    }

    #[test]
    fn set_channel_field_dues_reminder() {
        let mut cfg = GuildConfig::default();
        let (label, old) =
            set_channel_field(&mut cfg, DUES_REMINDER_CHAN_ID, DiscordChannelId(20)).unwrap();
        assert_eq!(label, "dues-reminder channel");
        assert_eq!(old, None);
        assert_eq!(cfg.dues_reminder_channel, Some(DiscordChannelId(20)));
        let (_, old2) =
            set_channel_field(&mut cfg, DUES_REMINDER_CHAN_ID, DiscordChannelId(21)).unwrap();
        assert_eq!(old2, Some(20));
    }
}
