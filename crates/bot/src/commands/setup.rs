//! `/setup` - the Manage-Server-gated guild configuration panel. Bootstraps the
//! moderator role, so it gates on Discord's Manage Guild permission rather than the
//! bot's own (not-yet-configured) moderator role.

use serenity::all::{ButtonStyle, CreateActionRow, CreateButton, Permissions};

use crate::data::{Context, Error};
use crate::render::setup::config_embed;

const SET_ROLES_ID: &str = "setup_set_roles";
const SET_CHANNELS_ID: &str = "setup_set_channels";

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
    let cfg = data.guild_config.load();
    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(SET_ROLES_ID)
            .label("Set roles")
            .style(ButtonStyle::Primary),
        CreateButton::new(SET_CHANNELS_ID)
            .label("Set channels")
            .style(ButtonStyle::Secondary),
    ]);
    ctx.send(
        poise::CreateReply::default()
            .embed(config_embed(&cfg, data.config.accent_color))
            .components(vec![buttons])
            .ephemeral(true),
    )
    .await?;
    Ok(())
}
