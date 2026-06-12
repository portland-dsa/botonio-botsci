//! `/help` - an ephemeral embed with a per-invoker topic select menu.

use std::time::Duration;

use serenity::all::{
    ComponentInteractionCollector, CreateActionRow, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, RoleId,
};
use serenity::futures::StreamExt as _;

use crate::data::{Context, Error};
use crate::render::help::{Topic, help_embed, topics_for};

const MENU_ID: &str = "help_topic";
const NAV_TIMEOUT: Duration = Duration::from_secs(180);

#[poise::command(slash_command)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let is_mod = invoker_is_moderator(&ctx).await;
    let accent = ctx.data().config.accent_color;
    let topics = topics_for(is_mod);
    let current = Topic::MyMembership;

    let reply = poise::CreateReply::default()
        .embed(help_embed(current, accent))
        .components(vec![menu_row(&topics, current)])
        .ephemeral(true);
    let handle = ctx.send(reply).await?;

    // Collect select interactions on this ephemeral message until the timeout.
    let msg = handle.message().await?;
    let mut stream = ComponentInteractionCollector::new(ctx.serenity_context())
        .message_id(msg.id)
        .author_id(ctx.author().id)
        .custom_ids(vec![MENU_ID.to_string()])
        .timeout(NAV_TIMEOUT)
        .stream();

    while let Some(interaction) = stream.next().await {
        // Re-derive the chosen topic AND re-check permission - never trust the id.
        let chosen = selected_topic(&interaction);
        let allowed = topics_for(invoker_is_moderator(&ctx).await);
        let topic = chosen
            .filter(|t| allowed.contains(t))
            .unwrap_or(Topic::MyMembership);

        // Log and keep navigating if one update fails: a transient 5xx or an
        // already-acknowledged interaction must not end the whole menu session.
        if let Err(e) = interaction
            .create_response(
                ctx.serenity_context(),
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(help_embed(topic, accent))
                        .components(vec![menu_row(&allowed, topic)]),
                ),
            )
            .await
        {
            tracing::warn!(error = %e, "help menu: failed to update message; continuing");
        }
    }
    Ok(())
}

fn menu_row(topics: &[Topic], current: Topic) -> CreateActionRow {
    let options = topics
        .iter()
        .map(|t| CreateSelectMenuOption::new(t.label(), t.id()).default_selection(*t == current))
        .collect();
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(MENU_ID, CreateSelectMenuKind::String { options })
            .placeholder("Pick a topic"),
    )
}

fn selected_topic(interaction: &serenity::all::ComponentInteraction) -> Option<Topic> {
    if let serenity::all::ComponentInteractionDataKind::StringSelect { values } =
        &interaction.data.kind
    {
        values.first().and_then(|v| Topic::from_id(v))
    } else {
        None
    }
}

/// Whether the invoker holds the configured moderator role.
async fn invoker_is_moderator(ctx: &Context<'_>) -> bool {
    let role_id = RoleId::new(ctx.data().config.moderator_role_id);
    match ctx.author_member().await {
        Some(member) => member.roles.contains(&role_id),
        None => false,
    }
}
