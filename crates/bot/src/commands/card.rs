//! `/membership-card` (slash, self), "Membership Card" (user context-menu), and
//! `/lookup` (moderator slash). The self-only slash keeps its own path; the menu
//! and `/lookup` route through a shared core.

use chrono::Utc;
use serenity::all::User;

use engine::backends::util::DiscordUserId;
use engine::card::{self, CardError, PresentMember};

use crate::data::{Context, Error};
use crate::lookup::{self, LookupOutcome};
use crate::moderator::invoker_is_moderator;
use crate::render;

/// Slash form: always the invoker's own card.
#[poise::command(slash_command, rename = "membership-card")]
pub async fn membership_card(ctx: Context<'_>) -> Result<(), Error> {
    let author = ctx.author();
    show_card(ctx, author).await
}

/// Right-click form. Self for everyone; any member for moderators.
#[poise::command(context_menu_command = "Membership Card")]
pub async fn membership_card_menu(ctx: Context<'_>, target: User) -> Result<(), Error> {
    show_for_target(ctx, &target).await
}

/// Look up another member's membership card. Moderators only.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn lookup(
    ctx: Context<'_>,
    #[description = "The member to look up"] target: User,
) -> Result<(), Error> {
    show_for_target(ctx, &target).await
}

/// Resolve a privileged-or-self lookup of `target` through the shared core and
/// render the outcome ephemerally.
async fn show_for_target(ctx: Context<'_>, target: &User) -> Result<(), Error> {
    let invoker = DiscordUserId(ctx.author().id.get());
    let target_id = DiscordUserId(target.id.get());
    let is_mod = invoker_is_moderator(&ctx).await;
    let data = ctx.data();
    // store/auditor are generic params, so they need an explicit `&*` to reach the
    // concrete type the trait is implemented for; rate_limiter is a concrete
    // `&RateLimiter` where deref coercion applies (and clippy prefers the bare `&`).
    let outcome = lookup::lookup(
        &*data.store,
        &*data.auditor,
        &data.rate_limiter,
        invoker,
        target_id,
        is_mod,
    )
    .await;
    render_outcome(ctx, target, outcome).await
}

/// Turn a [`LookupOutcome`] into the ephemeral reply the member or moderator sees.
async fn render_outcome(
    ctx: Context<'_>,
    target: &User,
    outcome: LookupOutcome,
) -> Result<(), Error> {
    let reply = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };
    match outcome {
        LookupOutcome::Card(rec) | LookupOutcome::SelfCard(Some(rec)) => {
            let display_name = target.name.clone();
            let embed =
                render::card::membership_card(&rec, &display_name, None, Utc::now().date_naive());
            ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
                .await?;
        }
        LookupOutcome::SelfCard(None) => {
            ctx.send(reply(
                "I couldn't find a membership record for you. \
                 If you think this is wrong, ask a moderator.",
            ))
            .await?;
        }
        LookupOutcome::NotFound => {
            ctx.send(reply("No membership record found for that member."))
                .await?;
        }
        LookupOutcome::NotModerator => {
            tracing::warn!("non-moderator attempted a member lookup");
            ctx.send(reply("That command is for moderators only."))
                .await?;
        }
        LookupOutcome::RateLimited => {
            tracing::warn!("moderator lookup rate limit hit");
            ctx.send(reply(
                "You're looking up members too quickly - give it a moment.",
            ))
            .await?;
        }
        LookupOutcome::StoreError(e) => {
            tracing::error!(error = %e, "member lookup failed");
            ctx.send(reply(
                "Something went wrong on my end - please try again in a moment.",
            ))
            .await?;
        }
    }
    Ok(())
}

async fn show_card(ctx: Context<'_>, user: &User) -> Result<(), Error> {
    let subject = PresentMember {
        id: DiscordUserId(user.id.get()),
    };
    let rec = match card::resolve(&*ctx.data().store, &subject).await {
        Ok(rec) => rec,
        Err(CardError::NoRecord) => {
            // Expected outcome (the member has no linked record), so log at debug rather
            // than error - but leave a trace so a systemic miss spike stays diagnosable.
            // No identifiers: a count of these is enough to spot an outage.
            tracing::debug!("no membership record found for card lookup");
            ctx.send(
                poise::CreateReply::default()
                    .content(
                        "I couldn't find a membership record for you. \
                         If you think this is wrong, ask a moderator.",
                    )
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        // The live database-error path: the store is Postgres-backed (`PgStore`), so any
        // connection or query failure lands here. Log the detail and give the member a
        // generic, PII-free reply rather than surfacing the error. (The in-memory store
        // used in tests is `Infallible`, so this arm only fires against the real store.)
        Err(CardError::Store(e)) => {
            tracing::error!(error = %e, "membership card store lookup failed");
            ctx.send(
                poise::CreateReply::default()
                    .content("Something went wrong on my end - please try again in a moment.")
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
    };

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
