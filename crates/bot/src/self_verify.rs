//! Self-service verification: the standing-button + modal flow that lets a member
//! verify themselves against Solidarity Tech with no moderator approval. This file
//! holds the pure name double-check and the gateway interaction handler.

use std::time::Instant;

use serenity::all::{
    ChannelId, ComponentInteraction, Context, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, EditInteractionResponse,
    EditMessage, GuildId, Interaction, Member, Message, MessageId, ModalInteraction, RoleId,
    UserId,
};

use domain::Role;
use engine::backends::util::{DiscordHandle, DiscordUserId, Email};
use engine::verify::{DataStore, EmailGrant, Member as VerifyMember, Target};

use crate::data::{Data, Error};
use crate::render::modal::{REASON_FIELD_ID, override_modal, parse_email, parse_reason};
use crate::render::self_verify::{
    EMAIL_FIELD_ID, EMAIL_FIELD_LABEL, FIRST_FIELD_ID, LAST_FIELD_ID, PROMPT_BUTTON_ID,
    REVIEW_MODAL_ID, REVIEW_OVERRIDE_ID, REVIEW_REJECT_ID, REVIEW_REVERIFY_ID, ReviewReason,
    SUBMIT_MODAL_ID, VerifyLog, log_embed, manual_log_embed, review_request, self_verify_modal,
};

/// Whether the name a member typed lines up with their Solidarity Tech record.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// The submitted name lines up with the record.
    Match,
    /// It does not line up - the log post is flagged, but the grant still stands.
    Mismatch,
    /// The record has no name to compare against; not a mismatch.
    Unchecked,
}

/// Compare a submitted first name + last initial against the record's full name.
///
/// Deliberately blunt - no fuzzy or nickname matching - but lenient enough not to cry
/// wolf. Comparison is case- and accent-insensitive (so `Jose` and `jose` line up, as do
/// their accented forms). When the record name has a distinct first and last token, the
/// submitted first name and last initial must both match. When it carries only a single
/// token (a first name or a last name; the composed `full_name` cannot say which), the
/// submission matches if either the first name or the last initial lines up, so a real
/// member is not flagged merely because the record lacks the other half. A record with no
/// usable name is [`Unchecked`](NameCheck::Unchecked), never a mismatch.
pub fn name_check(record_name: Option<&str>, first: &str, last_initial: &str) -> NameCheck {
    let Some(name) = record_name else {
        return NameCheck::Unchecked;
    };
    let tokens: Vec<&str> = name.split_whitespace().collect();
    let (Some(rec_first), Some(rec_last)) = (tokens.first().copied(), tokens.last().copied())
    else {
        return NameCheck::Unchecked;
    };

    let first_ok = eq_fold(rec_first, first_token(first));
    let last_ok = initial_matches(rec_last, last_initial);

    // A single-token record name is ambiguous - it could be the first or the last name -
    // so accept the submission when either part lines up, and only warn when neither
    // does. A name with distinct first and last tokens must match on both.
    let matched = if tokens.len() == 1 {
        first_ok || last_ok
    } else {
        first_ok && last_ok
    };
    if matched {
        NameCheck::Match
    } else {
        NameCheck::Mismatch
    }
}

/// Case- and accent-insensitive string equality. Unlike `eq_ignore_ascii_case`, this
/// folds non-ASCII letters, so an accented name is not spuriously flagged.
fn eq_fold(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether `token`'s first character equals the submitted `initial`, case- and
/// accent-insensitively. Empty or blank inputs never match.
fn initial_matches(token: &str, initial: &str) -> bool {
    match (token.chars().next(), initial.trim().chars().next()) {
        (Some(t), Some(i)) => t.to_lowercase().eq(i.to_lowercase()),
        _ => false,
    }
}

/// The first whitespace-separated token of `s`, or the empty string when `s` is blank.
fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

/// Shown for every outcome that is not a clean grant - a miss, a conflict, a malformed
/// email, an unusable record - all identical so a probe cannot tell members from
/// non-members (no enumeration). The matching cases post a moderator review request out
/// of band, which is what "a moderator will review" refers to.
const UNIFORM_FAILURE: &str = "We couldn't verify you automatically. A moderator will review your request and follow up shortly.";
/// The only distinct failure: verification could not run (backend down / unconfigured).
const BACKEND_ERROR: &str = "Something went wrong on my end - please try again in a moment.";
const RATE_LIMITED: &str = "You're going a little fast - please wait a moment and try again.";
const SUCCESS: &str = "You're verified! Welcome.";

/// Route a raw gateway interaction. Acts only on the self-verify button and modal, and
/// only when they arrive from the guild this bot serves; every other interaction (poise
/// commands, other collectors) is ignored so the framework's own dispatch is untouched.
pub async fn on_interaction(
    ctx: &Context,
    interaction: &Interaction,
    data: &Data,
) -> Result<(), Error> {
    match interaction {
        Interaction::Component(c) => on_component(ctx, c, data).await,
        Interaction::Modal(m) => on_modal(ctx, m, data).await,
        _ => Ok(()),
    }
}

/// Route a button press: the standing prompt opens the form; the review buttons (which
/// carry the target member id as a `kind:id` suffix) drive the moderator approval flow.
async fn on_component(ctx: &Context, c: &ComponentInteraction, data: &Data) -> Result<(), Error> {
    if !in_home_guild(c.guild_id, data) {
        return Ok(());
    }
    let id = c.data.custom_id.as_str();
    if id == PROMPT_BUTTON_ID {
        return open_modal(ctx, c).await;
    }
    let Some((kind, arg)) = id.split_once(':') else {
        return Ok(());
    };
    if kind == REVIEW_REVERIFY_ID {
        review_reverify(ctx, c, data, arg).await
    } else if kind == REVIEW_OVERRIDE_ID {
        review_open_override(ctx, c, data, arg).await
    } else if kind == REVIEW_REJECT_ID {
        review_reject(ctx, c, data).await
    } else {
        Ok(())
    }
}

/// Route a modal submission: the self-verify form, or the manual-override reason modal
/// opened from a review request (its custom id carries `{member_id}:{message_id}`).
async fn on_modal(ctx: &Context, m: &ModalInteraction, data: &Data) -> Result<(), Error> {
    if !in_home_guild(m.guild_id, data) {
        return Ok(());
    }
    let id = m.data.custom_id.as_str();
    if id == SUBMIT_MODAL_ID {
        return on_submit(ctx, m, data).await;
    }
    if let Some((kind, rest)) = id.split_once(':')
        && kind == REVIEW_MODAL_ID
    {
        return review_override_submit(ctx, m, data, rest).await;
    }
    Ok(())
}

/// Whether an interaction came from the one guild this bot serves. Defence in depth
/// beneath `guild_guard` (which already makes the bot leave any other guild), mirroring
/// the same gate in [`crate::join`]; a DM interaction (no guild) is never ours.
fn in_home_guild(guild_id: Option<GuildId>, data: &Data) -> bool {
    guild_id.is_some_and(|g| g.get() == data.config.guild_id)
}

/// Open the verification form in response to the standing button.
async fn open_modal(ctx: &Context, c: &ComponentInteraction) -> Result<(), Error> {
    c.create_response(ctx, CreateInteractionResponse::Modal(self_verify_modal()))
        .await?;
    Ok(())
}

/// Read the value the member typed into the named modal field.
fn field_value(submit: &ModalInteraction, field_id: &str) -> String {
    submit
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            serenity::all::ActionRowComponent::InputText(input) if input.custom_id == field_id => {
                input.value.clone()
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// The name the member typed, formatted for the moderator log ("First L"). Either field
/// may arrive blank in an edge case, so the result falls back to a placeholder - Discord
/// rejects an empty embed field value.
fn format_submitted_name(first: &str, last_initial: &str) -> String {
    let name = format!("{} {}", first.trim(), last_initial.trim());
    let name = name.trim();
    if name.is_empty() {
        "(not given)".to_owned()
    } else {
        name.to_owned()
    }
}

/// Run the self-service verification for a submitted form.
async fn on_submit(ctx: &Context, submit: &ModalInteraction, data: &Data) -> Result<(), Error> {
    // Defer ephemerally first: the live Solidarity Tech read + grant below can
    // exceed Discord's 3-second interaction deadline. The result edits this reply.
    submit
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;

    let reply = |text: &str| {
        let text = text.to_owned();
        async move {
            let _ = submit
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await;
        }
    };

    let member_id = DiscordUserId(submit.user.id.get());
    if !data.self_verify_limiter.check(member_id, Instant::now()) {
        reply(RATE_LIMITED).await;
        return Ok(());
    }

    // Roles must be configured (this is also the role-write client we need).
    let Some(discord) = data.role_writer() else {
        tracing::warn!("self-verify attempted before the managed roles were configured");
        reply(BACKEND_ERROR).await;
        return Ok(());
    };

    let Some(email) = parse_email(&field_value(submit, EMAIL_FIELD_ID)) else {
        // A malformed email gives the same uniform response - no detail.
        reply(UNIFORM_FAILURE).await;
        return Ok(());
    };
    let first = field_value(submit, FIRST_FIELD_ID);
    let last_initial = field_value(submit, LAST_FIELD_ID);
    // Surfaced in the moderator log so a name can be eyeballed even when the record has
    // none on file; also fed to `name_check` for the mismatch flag.
    let submitted_name = format_submitted_name(&first, &last_initial);

    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
        data.config.guild(),
    );
    let target = Target {
        id: member_id,
        handle: DiscordHandle(submit.user.name.clone()),
    };
    // Self-service: the member is both actor and subject - that is what marks the
    // grant as self-service in the audit log (no extra field needed).
    let outcome = VerifyMember::new(&store, target)
        .verify_by_email_with_record(member_id, email.clone())
        .await;

    match outcome {
        Ok(EmailGrant::Verified { role, record }) => {
            // `name_check` and `NameCheck` are defined above in this same module.
            let name_mismatch = matches!(
                name_check(record.full_name.as_deref(), &first, &last_initial),
                NameCheck::Mismatch
            );
            let log = VerifyLog {
                member: submit.user.id,
                handle: &submit.user.name,
                submitted_name: &submitted_name,
                record_name: record.full_name.as_deref(),
                role,
                email: &email.0,
                name_mismatch,
            };
            post_log(ctx, data, &log).await;
            reply(SUCCESS).await;
        }
        // Could not auto-grant. The member sees the same no-detail message in every case
        // (anti-enumeration), while a moderator review request goes out of band so a human
        // can re-check, hand-approve, or dismiss. The reason rides only in the (private)
        // mod post.
        Ok(EmailGrant::Malformed) => {
            post_review_request(
                ctx,
                data,
                submit,
                &submitted_name,
                &email,
                ReviewReason::Malformed,
            )
            .await;
            reply(UNIFORM_FAILURE).await;
        }
        Ok(EmailGrant::Conflict) => {
            post_review_request(
                ctx,
                data,
                submit,
                &submitted_name,
                &email,
                ReviewReason::Conflict,
            )
            .await;
            reply(UNIFORM_FAILURE).await;
        }
        Ok(EmailGrant::NotFound) => {
            post_review_request(
                ctx,
                data,
                submit,
                &submitted_name,
                &email,
                ReviewReason::NotFound,
            )
            .await;
            reply(UNIFORM_FAILURE).await;
        }
        Err(e) => {
            tracing::error!(error = %e, "self-verify failed");
            reply(BACKEND_ERROR).await;
        }
    }
    Ok(())
}

/// Post the success record to the configured log channel. Missing channel -> warn
/// and skip; the grant already stands.
async fn post_log(ctx: &Context, data: &Data, log: &VerifyLog<'_>) {
    let embed = log_embed(log, data.config.accent_color);
    send_to_log(ctx, data, embed).await;
}

/// Post a moderator-approval request for a self-verification that could not be granted
/// automatically. No mod-approval channel set -> warn and skip. The member-facing reply
/// is uniform regardless of `reason`; only this (private) post names it.
async fn post_review_request(
    ctx: &Context,
    data: &Data,
    submit: &ModalInteraction,
    submitted_name: &str,
    email: &Email,
    reason: ReviewReason,
) {
    let Some(channel) = data.guild_config.load().mod_approval_channel else {
        tracing::warn!(
            "self-verify needs a moderator review but no mod-approval channel is set; skipping"
        );
        return;
    };
    let message = review_request(
        submit.user.id,
        &submit.user.name,
        submitted_name,
        &email.0,
        reason,
        data.config.accent_color,
    );
    if let Err(e) = ChannelId::new(channel.0)
        .send_message(&ctx.http, message)
        .await
    {
        tracing::warn!(error = %e, "self-verify: failed to post the moderator review request");
    }
}

/// Send a verification-log embed to the configured channel. No channel set -> warn and
/// skip; whatever outcome calls this has already taken effect, so the post is best-effort.
async fn send_to_log(ctx: &Context, data: &Data, embed: serenity::all::CreateEmbed) {
    let Some(channel) = data.guild_config.load().verification_log_channel else {
        tracing::warn!("self-verify needs a verification-log post but no channel is set; skipping");
        return;
    };
    if let Err(e) = serenity::all::ChannelId::new(channel.0)
        .send_message(&ctx.http, serenity::all::CreateMessage::new().embed(embed))
        .await
    {
        tracing::warn!(error = %e, "self-verify: failed to post the verification log");
    }
}

// --- Moderator review of a failed self-verification -------------------------------------

/// Re-run verification for a pending request (a moderator may have just added the member
/// to Solidarity Tech). On a clean grant, resolve the post; otherwise leave it open and
/// tell the moderator privately. The moderator is the audit actor for this re-check.
async fn review_reverify(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    arg: &str,
) -> Result<(), Error> {
    if !clicker_is_moderator(c.member.as_ref(), data) {
        return deny_non_moderator(ctx, c).await;
    }
    let Some(target) = parse_target(arg) else {
        return Ok(());
    };
    let Some(email) = field_from_post(&c.message, EMAIL_FIELD_LABEL).and_then(parse_email) else {
        return deny(
            ctx,
            c,
            "Couldn't read the email off this request - approve by hand instead.",
        )
        .await;
    };
    // Acknowledge as a deferred message update: the Solidarity Tech read can exceed the
    // 3-second deadline. A clean grant edits the post; anything else leaves it open.
    c.create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;

    let Some(discord) = data.role_writer() else {
        followup(ctx, c, BACKEND_ERROR).await;
        return Ok(());
    };
    let Some(handle) = fetch_handle(ctx, target).await else {
        followup(ctx, c, "Couldn't load that member - try again.").await;
        return Ok(());
    };
    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
        data.config.guild(),
    );
    let invoker = DiscordUserId(c.user.id.get());
    let outcome = VerifyMember::new(
        &store,
        Target {
            id: target,
            handle: handle.clone(),
        },
    )
    .verify_by_email_with_record(invoker, email)
    .await;
    match outcome {
        Ok(EmailGrant::Verified { role, .. }) => {
            // Keep the request post (signal approval, drop the buttons) and log the grant
            // to the verification channel, like an automatic self-verify.
            resolve_post(
                ctx,
                data,
                c.message.id.get(),
                c.user.id,
                "Approved by re-verify",
            )
            .await;
            post_manual_log(ctx, data, target, &handle.0, c.user.id, "re-verify", role).await;
            followup(ctx, c, &format!("Verified - granted {}.", role.as_str())).await;
        }
        Ok(_) => {
            followup(
                ctx,
                c,
                "Still no match for that email - the record may not be in Solidarity Tech yet, \
                 or it needs a manual override.",
            )
            .await;
        }
        Err(e) => {
            tracing::error!(error = %e, "self-verify review: re-verify failed");
            followup(ctx, c, BACKEND_ERROR).await;
        }
    }
    Ok(())
}

/// Open the manual-override reason modal in response to the "Manual override" button. The
/// modal custom id carries the target member id and the review message id so the submit
/// handler can grant and then resolve the same post.
async fn review_open_override(
    ctx: &Context,
    c: &ComponentInteraction,
    data: &Data,
    arg: &str,
) -> Result<(), Error> {
    if !clicker_is_moderator(c.member.as_ref(), data) {
        return deny_non_moderator(ctx, c).await;
    }
    let Some(target) = parse_target(arg) else {
        return Ok(());
    };
    // Hand-approval grants the Manual Override marker, so refuse before collecting a reason
    // when that role is unset (mirrors the /verify override gate).
    if data.guild_config.load().manual_override_role.is_none() {
        return deny(
            ctx,
            c,
            "No Manual Override role is configured - set one in /setup first.",
        )
        .await;
    }
    let display = fetch_handle(ctx, target)
        .await
        .map_or_else(|| "the member".to_owned(), |h| h.0);
    let modal_id = format!("{REVIEW_MODAL_ID}:{}:{}", target.0, c.message.id.get());
    c.create_response(
        ctx,
        CreateInteractionResponse::Modal(override_modal(&modal_id, &display)),
    )
    .await?;
    Ok(())
}

/// Dismiss a review request: keep the post, drop its buttons, and add a "Rejected by"
/// banner so it cannot be actioned again.
async fn review_reject(ctx: &Context, c: &ComponentInteraction, data: &Data) -> Result<(), Error> {
    if !clicker_is_moderator(c.member.as_ref(), data) {
        return deny_non_moderator(ctx, c).await;
    }
    // Acknowledge without changing the message, then resolve it in place.
    c.create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;
    resolve_post(ctx, data, c.message.id.get(), c.user.id, "Rejected").await;
    Ok(())
}

/// Grant a manual override from the review reason modal, then resolve the request post.
async fn review_override_submit(
    ctx: &Context,
    m: &ModalInteraction,
    data: &Data,
    rest: &str,
) -> Result<(), Error> {
    // Defer ephemerally: the grant below can exceed the 3-second deadline.
    m.create_response(
        ctx,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;
    let reply = |text: &str| {
        let text = text.to_owned();
        async move {
            let _ = m
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await;
        }
    };
    if !clicker_is_moderator(m.member.as_ref(), data) {
        reply("Only moderators can act on a verification request.").await;
        return Ok(());
    }
    // `rest` is "{member_id}:{message_id}".
    let (Some(target), Some(message_id)) = rest.split_once(':').map_or((None, None), |(t, msg)| {
        (parse_target(t), msg.parse::<u64>().ok())
    }) else {
        return Ok(());
    };
    let Some(discord) = data.role_writer() else {
        reply(BACKEND_ERROR).await;
        return Ok(());
    };
    let Some(handle) = fetch_handle(ctx, target).await else {
        reply("Couldn't load that member - try again.").await;
        return Ok(());
    };
    let reason = parse_reason(&field_value(m, REASON_FIELD_ID));
    let store = DataStore::new(
        &*data.solidarity_tech,
        &discord,
        &*data.store,
        &*data.auditor,
        data.config.guild(),
    );
    let invoker = DiscordUserId(m.user.id.get());
    match VerifyMember::new(
        &store,
        Target {
            id: target,
            handle: handle.clone(),
        },
    )
    .override_approve(invoker, reason)
    .await
    {
        Ok(()) => {
            // Keep the request post (signal approval, drop the buttons) and log the manual
            // verification to the verification channel.
            resolve_post(
                ctx,
                data,
                message_id,
                m.user.id,
                "Approved by manual override",
            )
            .await;
            post_manual_log(
                ctx,
                data,
                target,
                &handle.0,
                m.user.id,
                "manual override",
                Role::Member,
            )
            .await;
            reply("Approved - they now hold the Member role and the Manual Override marker.").await;
        }
        Err(e) => {
            tracing::error!(error = %e, "self-verify review: manual override failed");
            reply(BACKEND_ERROR).await;
        }
    }
    Ok(())
}

/// Whether the member who pressed a review button holds the configured moderator role.
/// The review buttons live in the (mod-only) approval channel, but this is the real gate.
fn clicker_is_moderator(member: Option<&Member>, data: &Data) -> bool {
    let Some(role) = data.guild_config.load().moderator_role else {
        return false;
    };
    let role_id = RoleId::new(role.0);
    member.is_some_and(|m| m.roles.contains(&role_id))
}

/// Parse the `kind:id` suffix of a review custom id into the target member id.
fn parse_target(arg: &str) -> Option<DiscordUserId> {
    arg.parse::<u64>().ok().map(DiscordUserId)
}

/// Read a labelled field's value back off a posted review embed (e.g. the submitted email).
fn field_from_post<'a>(message: &'a Message, label: &str) -> Option<&'a str> {
    message
        .embeds
        .iter()
        .flat_map(|e| &e.fields)
        .find(|f| f.name == label)
        .map(|f| f.value.as_str())
}

/// The target member's current handle, fetched live (the clicker is a moderator, not the
/// subject). `None` when the user cannot be loaded.
async fn fetch_handle(ctx: &Context, id: DiscordUserId) -> Option<DiscordHandle> {
    match ctx.http.get_user(UserId::new(id.0)).await {
        Ok(user) => Some(DiscordHandle(user.name)),
        Err(e) => {
            tracing::warn!(error = %e, "self-verify review: could not load the target member");
            None
        }
    }
}

/// Reply ephemerally to the moderator who pressed a review button (a refusal or error),
/// leaving the request post untouched.
async fn deny(ctx: &Context, c: &ComponentInteraction, text: &str) -> Result<(), Error> {
    c.create_response(
        ctx,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .ephemeral(true)
                .content(text),
        ),
    )
    .await?;
    Ok(())
}

async fn deny_non_moderator(ctx: &Context, c: &ComponentInteraction) -> Result<(), Error> {
    deny(ctx, c, "Only moderators can act on a verification request.").await
}

/// Send an ephemeral follow-up to the moderator after a deferred review action.
async fn followup(ctx: &Context, c: &ComponentInteraction, text: &str) {
    let _ = c
        .create_followup(
            ctx,
            CreateInteractionResponseFollowup::new()
                .ephemeral(true)
                .content(text),
        )
        .await;
}

/// Resolve a review request in place: keep the original request embed, drop the buttons,
/// and add a one-line status banner so it cannot be actioned twice. The post lives in the
/// configured mod-approval channel. `content` + cleared `components` is a partial edit, so
/// the embed is left intact.
async fn resolve_post(ctx: &Context, data: &Data, message_id: u64, resolver: UserId, status: &str) {
    let Some(channel) = data.guild_config.load().mod_approval_channel else {
        return;
    };
    let edit = EditMessage::new()
        .content(format!("{status} by <@{}>.", resolver.get()))
        .components(vec![]);
    if let Err(e) = ChannelId::new(channel.0)
        .edit_message(&ctx.http, MessageId::new(message_id), edit)
        .await
    {
        tracing::warn!(error = %e, "self-verify review: failed to resolve the request post");
    }
}

/// Log a moderator-driven grant (re-verify or manual override) to the verification channel,
/// the same place automatic self-verifications are recorded.
async fn post_manual_log(
    ctx: &Context,
    data: &Data,
    member: DiscordUserId,
    handle: &str,
    approver: UserId,
    how: &str,
    role: Role,
) {
    let embed = manual_log_embed(
        UserId::new(member.0),
        handle,
        approver,
        how,
        role,
        data.config.accent_color,
    );
    send_to_log(ctx, data, embed).await;
}

#[cfg(test)]
mod name_check_tests {
    use super::*;

    #[test]
    fn matches_first_and_last_initial_case_insensitively() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "rosy", "R"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("rosy rascal"), " ROSY ", "r"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "R."),
            NameCheck::Match
        );
    }

    #[test]
    fn first_name_or_initial_off_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Shadow", "R"),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "Z"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn an_absent_record_name_is_unchecked() {
        assert_eq!(name_check(None, "Rosy", "R"), NameCheck::Unchecked);
        assert_eq!(name_check(Some("   "), "Rosy", "R"), NameCheck::Unchecked);
    }

    #[test]
    fn a_single_token_record_name_compares_first_against_last() {
        // "Cher" -> first and last token are the same; first "Cher", initial "C".
        assert_eq!(name_check(Some("Cher"), "Cher", "C"), NameCheck::Match);
    }

    #[test]
    fn multi_token_name_uses_the_last_token_for_the_initial() {
        // "Amy Rose Hedgehog" -> first "Amy", last token "Hedgehog"; middle token "Rose" is ignored.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "H"),
            NameCheck::Match
        );
        // "R" matches the middle token "Rose", not the last - must be a mismatch.
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy", "R"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn empty_or_blank_last_initial_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", ""),
            NameCheck::Mismatch
        );
        assert_eq!(
            name_check(Some("Rosy Rascal"), "Rosy", "   "),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn a_first_name_only_record_is_not_flagged_on_the_surname() {
        // `full_name` built from a first name alone ("Rosy"): the member's real surname
        // initial must not produce a spurious mismatch.
        assert_eq!(name_check(Some("Rosy"), "Rosy", "S"), NameCheck::Match);
    }

    #[test]
    fn a_last_name_only_record_matches_on_the_initial() {
        // `full_name` built from a last name alone ("Rascal"): the lone token is the
        // surname, so the last initial lines up even though the first name does not.
        assert_eq!(name_check(Some("Rascal"), "Rosy", "R"), NameCheck::Match);
    }

    #[test]
    fn a_single_token_record_matching_neither_part_is_a_mismatch() {
        assert_eq!(
            name_check(Some("Rascal"), "Shadow", "H"),
            NameCheck::Mismatch
        );
    }

    #[test]
    fn accented_names_are_not_spuriously_flagged() {
        // `eq_ignore_ascii_case` folds only ASCII, so it would wrongly mismatch an
        // accented surname initial ('\u{c9}' vs '\u{e9}') and an accented given name.
        assert_eq!(
            name_check(Some("Amy \u{c9}lodie"), "amy", "\u{e9}"),
            NameCheck::Match
        );
        assert_eq!(
            name_check(Some("\u{c9}lodie Rascal"), "\u{e9}lodie", "R"),
            NameCheck::Match
        );
    }

    #[test]
    fn a_multi_word_given_name_matches_on_its_first_token() {
        assert_eq!(
            name_check(Some("Amy Rose Hedgehog"), "Amy Rose", "H"),
            NameCheck::Match
        );
    }
}
