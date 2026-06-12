//! `/membership-card` (slash, self) and "Membership Card" (user context-menu).
//! Both funnel through one core that is self-only for now.

use chrono::Utc;
use serenity::all::User;

use engine::backends::util::DiscordUserId;
use engine::card::{self, PresentMember};

use crate::data::{Context, Error};
use crate::render;

/// Slash form: always the invoker's own card.
#[poise::command(slash_command, rename = "membership-card")]
pub async fn membership_card(ctx: Context<'_>) -> Result<(), Error> {
    let author = ctx.author();
    show_card(ctx, author).await
}

/// Right-click form. Currently permits only the self case.
#[poise::command(context_menu_command = "Membership Card")]
pub async fn membership_card_menu(ctx: Context<'_>, target: User) -> Result<(), Error> {
    if target.id != ctx.author().id {
        ctx.send(
            poise::CreateReply::default()
                .content("You can only view your own membership card.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }
    show_card(ctx, &target).await
}

async fn show_card(ctx: Context<'_>, user: &User) -> Result<(), Error> {
    let subject = PresentMember {
        id: DiscordUserId(user.id.get()),
    };
    let rec = card::resolve(&*ctx.data().store, &subject).await?;

    // Nickname/display name and pronouns come from the interaction's member, not the
    // record. (Pronouns: read if serenity exposes them; otherwise always None.)
    let display_name = ctx
        .author_member()
        .await
        .map(|m| m.display_name().to_string())
        .unwrap_or_else(|| user.name.clone());
    let pronouns: Option<String> = None; // not wired yet

    let embed = render::card::membership_card(
        &rec,
        &display_name,
        pronouns.as_deref(),
        Utc::now().date_naive(),
    );
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}
