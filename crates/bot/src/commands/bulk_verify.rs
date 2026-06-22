//! `/bulk-verify` - sweep the server roster into a managed state in one pass, then
//! walk a moderator through the members that did not match. Moderators only; every
//! per-member write runs through the same audited engine path single /verify uses.
//! Resumable: progress is persisted, so a Discord interaction timeout loses nothing.

use std::time::Duration;

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateInteractionResponse, EditInteractionResponse,
};

use chrono::Utc;

use domain::{DiscordGuildId, Role};
use engine::backends::discord::{DiscordClient, DiscordError};
use engine::backends::util::{DiscordHandle, DiscordUserId};
use engine::bulk::{self, miss_still_pending};
use engine::store::{BulkMiss, BulkScope, BulkSession, BulkSessionStore, BulkStatus, MissState};
use engine::verify::{DataStore, Member, ResyncOutcome, Target};

use crate::commands::verify::{StepOutcome, verify_step};
use crate::data::{Context, Error};
use crate::moderator::invoker_is_moderator;
use crate::render::bulk_verify as embeds;

/// A conservative pause between per-member writes during apply, well under Discord's
/// role-write limits and Solidarity Tech's ~2 req/s self-heal cadence. Fixed, not
/// configured (no rate-limit knob, per the design).
const APPLY_PACING: Duration = Duration::from_millis(500);

/// How often the apply progress message is edited (Discord caps message edits).
const PROGRESS_EVERY: usize = 5;

/// Button ids for the resume prompt.
const RESUME_BUTTON_ID: &str = "bulk_resume";
const START_OVER_BUTTON_ID: &str = "bulk_start_over";
const CANCEL_BUTTON_ID: &str = "bulk_cancel";

/// Button ids for the preview confirm.
const APPLY_BUTTON_ID: &str = "bulk_apply";
const PREVIEW_CANCEL_BUTTON_ID: &str = "bulk_preview_cancel";

/// Wizard control button ids passed as `extra_buttons` to `verify_step`.
const SKIP_BUTTON_ID: &str = "bulk_skip";
const STOP_BUTTON_ID: &str = "bulk_stop";

/// The email-lookup button id, mirrored from verify so the pre-step render matches
/// what `verify_step` expects to find on the message it owns.
const LOOKUP_BUTTON_ID: &str = "verify_lookup_email";

#[derive(Debug, poise::ChoiceParameter)]
pub enum ScopeChoice {
    #[name = "Unmanaged members only (default)"]
    Unmanaged,
    #[name = "The whole server (full resync)"]
    WholeGuild,
}

impl From<ScopeChoice> for BulkScope {
    fn from(c: ScopeChoice) -> BulkScope {
        match c {
            ScopeChoice::Unmanaged => BulkScope::UnmanagedOnly,
            ScopeChoice::WholeGuild => BulkScope::WholeGuild,
        }
    }
}

/// Sweep the server roster and walk through unmatched members. Moderators only.
///
/// Hidden from non-moderators via `default_member_permissions`; the in-code
/// moderator check is the real gate.
#[poise::command(slash_command, default_member_permissions = "ADMINISTRATOR")]
pub async fn bulk_verify(
    ctx: Context<'_>,
    #[description = "Which members to sweep"] scope: Option<ScopeChoice>,
) -> Result<(), Error> {
    if !invoker_is_moderator(&ctx).await {
        tracing::warn!("non-moderator attempted a bulk verify");
        ctx.send(
            poise::CreateReply::default()
                .content("That command is for moderators only.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let plain = |content: &str| {
        poise::CreateReply::default()
            .content(content.to_owned())
            .ephemeral(true)
    };

    let data = ctx.data();
    let Some(discord) = data.role_writer() else {
        tracing::warn!("bulk-verify attempted before the managed roles were configured");
        ctx.send(plain(
            "Roles are not configured yet - a server manager needs to run /setup first.",
        ))
        .await?;
        return Ok(());
    };

    let invoker = DiscordUserId(ctx.author().id.get());
    let guild_id = DiscordGuildId(ctx.guild_id().unwrap().get());
    let scope = scope
        .map(BulkScope::from)
        .unwrap_or(BulkScope::UnmanagedOnly);
    let sctx = ctx.serenity_context();

    let embed_reply = |embed, components: Vec<CreateActionRow>| {
        poise::CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .components(components)
    };

    // --- Resume check ---
    let existing = data.store.load_session(guild_id).await?;
    // On "Start over" the resume prompt's message is reused as the sweep host, so the
    // whole flow stays in a single ephemeral message.
    let mut reused_handle: Option<poise::ReplyHandle<'_>> = None;
    if let Some(session) = existing.filter(|s| s.status == BulkStatus::InProgress) {
        if bulk::is_session_stale(session.updated_at, Utc::now()) {
            // Stale: abandon and fall through to a fresh sweep.
            data.store.abandon_session(guild_id).await?;
            tracing::info!(
                "abandoned stale bulk-verify session for guild {}",
                guild_id.0
            );
        } else {
            // Active session: offer Resume / Start over / Cancel.
            let counts = data.store.counts(guild_id).await?;
            let resume_handle = ctx
                .send(embed_reply(
                    embeds::resume_embed(session.scope, session.started_by, counts),
                    vec![CreateActionRow::Buttons(vec![
                        CreateButton::new(RESUME_BUTTON_ID)
                            .label("Resume")
                            .style(ButtonStyle::Primary),
                        CreateButton::new(START_OVER_BUTTON_ID)
                            .label("Start over")
                            .style(ButtonStyle::Secondary),
                        CreateButton::new(CANCEL_BUTTON_ID)
                            .label("Cancel")
                            .style(ButtonStyle::Secondary),
                    ])],
                ))
                .await?;
            // Collect the press in its own scope so the message borrow ends before the
            // handle may be moved into `reused_handle`.
            let press = {
                let message = resume_handle.message().await?;
                match message
                    .await_component_interaction(sctx)
                    .author_id(ctx.author().id)
                    .timeout(Duration::from_secs(180))
                    .await
                {
                    Some(press) => press,
                    // Timed out - leave the session as is and exit silently.
                    None => return Ok(()),
                }
            };
            press
                .create_response(sctx, CreateInteractionResponse::Acknowledge)
                .await?;

            match press.data.custom_id.as_str() {
                CANCEL_BUTTON_ID => {
                    press
                        .edit_response(
                            sctx,
                            EditInteractionResponse::new()
                                .content("Cancelled.")
                                .components(vec![]),
                        )
                        .await?;
                    return Ok(());
                }
                START_OVER_BUTTON_ID => {
                    // Discard the prior session now, so a cancelled or empty re-sweep
                    // cannot leave it dangling for the next /bulk-verify to offer as
                    // resumable. The fresh sweep below builds and stores a new one.
                    data.store.abandon_session(guild_id).await?;
                    press
                        .edit_response(
                            sctx,
                            EditInteractionResponse::new()
                                .embed(embeds::progress_embed(0, 0))
                                .components(vec![]),
                        )
                        .await?;
                    reused_handle = Some(resume_handle);
                }
                _ => {
                    // RESUME_BUTTON_ID: jump straight to the wizard.
                    press
                        .edit_response(
                            sctx,
                            EditInteractionResponse::new()
                                .embed(embeds::progress_embed(0, 0))
                                .components(vec![]),
                        )
                        .await?;
                    run_wizard(ctx, &resume_handle, &discord, invoker, guild_id, vec![]).await?;
                    return Ok(());
                }
            }
        }
    }

    // --- Sweep ---
    let sweep_handle = match reused_handle {
        Some(handle) => handle,
        None => {
            ctx.send(embed_reply(embeds::progress_embed(0, 0), vec![]))
                .await?
        }
    };
    let sweep_msg = sweep_handle.message().await?;

    let members = match bulk::enumerate(&discord, scope).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "bulk-verify roster enumerate failed");
            sweep_handle
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content("Failed to fetch the member list. Please try again in a moment.")
                        .ephemeral(true),
                )
                .await?;
            return Ok(());
        }
    };
    let total = members.len();
    let tally = bulk::preview(&*data.store, &members).await?;

    // --- Preview/confirm ---
    sweep_handle
        .edit(
            ctx,
            embed_reply(
                embeds::preview_embed(scope, &tally),
                vec![CreateActionRow::Buttons(vec![
                    CreateButton::new(APPLY_BUTTON_ID)
                        .label("Apply and continue")
                        .style(ButtonStyle::Primary),
                    CreateButton::new(PREVIEW_CANCEL_BUTTON_ID)
                        .label("Cancel")
                        .style(ButtonStyle::Secondary),
                ])],
            ),
        )
        .await?;

    let Some(confirm) = sweep_msg
        .await_component_interaction(sctx)
        .author_id(ctx.author().id)
        .timeout(Duration::from_secs(300))
        .await
    else {
        return Ok(());
    };
    confirm
        .create_response(sctx, CreateInteractionResponse::Acknowledge)
        .await?;

    if confirm.data.custom_id == PREVIEW_CANCEL_BUTTON_ID {
        confirm
            .edit_response(
                sctx,
                EditInteractionResponse::new()
                    .content("Cancelled.")
                    .components(vec![]),
            )
            .await?;
        return Ok(());
    }

    // Apply confirmed: show progress, then drive the audited verify path per member.
    confirm
        .edit_response(
            sctx,
            EditInteractionResponse::new()
                .embed(embeds::progress_embed(0, total))
                .components(vec![]),
        )
        .await?;

    // --- Apply ---
    let now = Utc::now();
    let mut queue: Vec<BulkMiss> = Vec::new();
    let mut role_tally: Vec<(Role, usize)> = Role::ALL.into_iter().map(|r| (r, 0)).collect();

    for (i, m) in members.iter().enumerate() {
        let store = DataStore::new(
            &*data.solidarity_tech,
            &discord,
            &*data.store,
            &*data.auditor,
        );
        let outcome = Member::new(
            &store,
            Target {
                id: m.id,
                handle: m.handle.clone(),
            },
        )
        .resync(invoker, &m.held)
        .await;

        // Pace only the members we actually wrote to: a role change, or a miss that was not
        // already Unverified. Untouched (already-correct) members fly past with no sleep.
        let wrote = match outcome {
            Ok(ResyncOutcome::Changed(role)) => {
                if let Some(entry) = role_tally.iter_mut().find(|(r, _)| *r == role) {
                    entry.1 += 1;
                }
                true
            }
            Ok(ResyncOutcome::Unchanged(_)) => false,
            Ok(ResyncOutcome::Miss) => {
                queue.push(BulkMiss {
                    discord_user_id: m.id,
                    handle: Some(m.handle.clone()),
                    position: queue.len() as i32,
                    state: MissState::Pending,
                });
                !bulk::already_in_role(&m.held, Role::Unverified)
            }
            Ok(ResyncOutcome::Conflict) => {
                // Conflicts are audited by resync and left for individual /verify; not queued.
                tracing::debug!(user = %m.id, "bulk-verify: conflict, leaving for /verify");
                false
            }
            Err(e) => {
                // One member's error never aborts the run.
                tracing::warn!(user = %m.id, error = %e, "bulk-verify: apply error, continuing");
                false
            }
        };

        if i % PROGRESS_EVERY == 0 {
            let _ = sweep_handle
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .embed(embeds::progress_embed(i + 1, total))
                        .ephemeral(true),
                )
                .await;
        }
        if wrote {
            tokio::time::sleep(APPLY_PACING).await;
        }
    }

    let session = BulkSession {
        guild: guild_id,
        scope,
        status: BulkStatus::InProgress,
        started_by: invoker,
        created_at: now,
        updated_at: now,
    };
    data.store.start_session(&session, &queue).await?;

    // --- Wizard loop ---
    run_wizard(ctx, &sweep_handle, &discord, invoker, guild_id, role_tally).await?;

    Ok(())
}

/// Walk through each pending miss, letting the moderator verify, skip, or stop.
/// Persists each decision immediately so a timeout loses nothing. When no pending
/// misses remain, completes the session and shows the summary.
async fn run_wizard(
    ctx: Context<'_>,
    handle: &poise::ReplyHandle<'_>,
    discord: &engine::backends::discord::DiscordHttp,
    invoker: DiscordUserId,
    guild_id: DiscordGuildId,
    role_tally: Vec<(Role, usize)>,
) -> Result<(), Error> {
    let data = ctx.data();
    let sctx = ctx.serenity_context();

    // The host message id is stable across handle edits; capture it once for the
    // collector verify_step drives. Every visible update goes through `handle.edit`
    // (the interaction token) - the only way to edit an ephemeral message. A plain
    // Message::edit silently fails on ephemerals, which would leave the wizard invisible.
    let message = handle.message().await?.into_owned();
    let reply = |embed, components: Vec<CreateActionRow>| {
        poise::CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .components(components)
    };

    // Capture total queue depth once at wizard entry for the position display.
    let counts_at_entry = data.store.counts(guild_id).await?;
    let total_queue = counts_at_entry.pending + counts_at_entry.verified + counts_at_entry.skipped;

    let skip_btn = CreateButton::new(SKIP_BUTTON_ID)
        .label("Skip")
        .style(ButtonStyle::Secondary);
    let stop_btn = CreateButton::new(STOP_BUTTON_ID)
        .label("Stop")
        .style(ButtonStyle::Secondary);

    let mut queue_exhausted = false;
    loop {
        let Some(miss) = data.store.next_pending(guild_id).await? else {
            // Queue exhausted: signal completion after the loop.
            queue_exhausted = true;
            break;
        };

        // Liveness: has the member left or been verified by another path?
        let liveness = discord.member_roles(miss.discord_user_id).await;
        match liveness {
            Err(e) if is_not_found(&e) => {
                tracing::debug!(user = %miss.discord_user_id, "bulk-verify: member left guild");
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Skipped)
                    .await?;
                continue;
            }
            Err(e) => {
                // Transient error (5xx, rate-limit, network blip): leave the miss
                // Pending so it is retried on the next resume.
                tracing::warn!(user = %miss.discord_user_id, %e, "bulk-verify: transient liveness error, pausing");
                break;
            }
            Ok(roles) if !miss_still_pending(true, &roles.held) => {
                tracing::debug!(user = %miss.discord_user_id, "bulk-verify: member already in managed role");
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Skipped)
                    .await?;
                continue;
            }
            Ok(_) => {}
        }

        // Fetch the User object verify_step needs for display and identity.
        let member = match sctx
            .http
            .get_member(
                serenity::all::GuildId::new(guild_id.0),
                serenity::all::UserId::new(miss.discord_user_id.0),
            )
            .await
            .map_err(DiscordError::from)
        {
            Ok(m) => m,
            Err(ref e) if is_not_found(e) => {
                // Left between the liveness check and now.
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Skipped)
                    .await?;
                continue;
            }
            Err(e) => {
                // Transient error: leave Pending and pause.
                tracing::warn!(user = %miss.discord_user_id, %e, "bulk-verify: transient fetch error, pausing");
                break;
            }
        };
        let target = member.user;
        let display_name = target
            .global_name
            .clone()
            .unwrap_or_else(|| target.name.clone());
        let target_handle = DiscordHandle(target.name.clone());
        let avatar = target.face();
        let position = (miss.position as usize) + 1;

        // Render the initial wizard state onto the host message before calling
        // verify_step, mirroring manual_verify_flow: the caller sets the initial embed
        // and buttons, then hands the message to verify_step which drives the collector
        // from there. The edit goes through the interaction token (handle.edit), never
        // Message::edit, because the host message is ephemeral.
        handle
            .edit(
                ctx,
                reply(
                    embeds::wizard_embed(
                        &display_name,
                        &target.name,
                        &avatar,
                        position,
                        total_queue,
                    ),
                    vec![CreateActionRow::Buttons(vec![
                        CreateButton::new(LOOKUP_BUTTON_ID)
                            .label("Look up by email")
                            .style(ButtonStyle::Primary),
                        skip_btn.clone(),
                        stop_btn.clone(),
                    ])],
                ),
            )
            .await?;

        let (outcome, extra_press) = verify_step(
            ctx,
            &message,
            discord,
            &target,
            invoker,
            miss.discord_user_id,
            target_handle,
            &[skip_btn.clone(), stop_btn.clone()],
        )
        .await?;

        match outcome {
            StepOutcome::Verified(_) | StepOutcome::Overridden => {
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Verified)
                    .await?;
            }
            StepOutcome::NotFoundExhausted => {
                // The moderator pressed one of the extra buttons (Skip or Stop).
                if let Some(press) = extra_press {
                    if press.data.custom_id == STOP_BUTTON_ID {
                        // Stop: session stays in_progress and is resumable.
                        let _ = press
                            .create_response(sctx, CreateInteractionResponse::Acknowledge)
                            .await;
                        return Ok(());
                    }
                    // Skip (or any other extra button): acknowledge and mark skipped.
                    let _ = press
                        .create_response(sctx, CreateInteractionResponse::Acknowledge)
                        .await;
                }
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Skipped)
                    .await?;
            }
            StepOutcome::Expired => {
                // Interaction idle window closed - leave in_progress, resumable.
                return Ok(());
            }
            StepOutcome::Conflict | StepOutcome::Errored => {
                // Conflict is left for /verify; a backend error keeps the wizard moving.
                data.store
                    .mark_miss(guild_id, miss.discord_user_id, MissState::Skipped)
                    .await?;
            }
        }
    }

    if !queue_exhausted {
        // A transient error interrupted the wizard. The session stays InProgress
        // so the moderator can resume with /bulk-verify.
        return Ok(());
    }

    // Queue exhausted: mark complete and show summary.
    data.store.complete_session(guild_id).await?;
    let counts = data.store.counts(guild_id).await?;
    let _ = handle
        .edit(
            ctx,
            reply(embeds::summary_embed(&role_tally, counts), vec![]),
        )
        .await;

    Ok(())
}

/// Whether a [`DiscordError`] represents a 404 (member not found / left the guild).
fn is_not_found(e: &DiscordError) -> bool {
    let DiscordError::Serenity(inner) = e else {
        return false;
    };
    matches!(
        inner.as_ref(),
        serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(resp))
        if resp.status_code == serenity::http::StatusCode::NOT_FOUND
    )
}
