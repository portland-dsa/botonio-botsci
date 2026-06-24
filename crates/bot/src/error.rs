//! The bot's error type and the poise error hook that turns failures into friendly,
//! ephemeral messages (never a raw error, never PII).

use crate::data::{Data, Error};

#[derive(Debug, thiserror::Error)]
pub enum BotError {
    #[error("engine error")]
    Engine(#[from] engine::Error),
    #[error("serenity error")]
    Serenity(#[from] serenity::Error),
    #[error("discord error")]
    Discord(#[from] engine::backends::discord::DiscordError),
    #[error("database error")]
    Db(#[from] persistence::PersistenceError),
}

impl BotError {
    /// A stable, PII-free label for logs/metrics - never the error payload, so an
    /// engine/serenity error's Debug chain can't leak a member handle, email, or
    /// `Context` string into the logs (per the SECURITY posture: PII out of logs).
    fn kind(&self) -> &'static str {
        match self {
            BotError::Engine(_) => "engine",
            BotError::Serenity(_) => "serenity",
            BotError::Discord(_) => "discord",
            BotError::Db(_) => "database",
        }
    }
}

/// poise calls this for any command error. Reply with a friendly ephemeral message
/// and log the detail (without member identifiers at info).
pub async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            // Card-specific outcomes (no record / store failure) are answered inside the
            // card command itself, which then returns Ok; anything reaching here is an
            // engine or serenity failure, so a single generic reply is right.
            let user_msg = "Something went wrong on my end - please try again in a moment.";
            // Always log the PII-free kind. For a serenity (Discord HTTP) error, also log the
            // detail: it is a Discord API status/message, never our member PII, so an
            // interaction-deadline or permission failure is diagnosable from the logs instead
            // of an opaque `kind="serenity"`.
            match &error {
                BotError::Serenity(e) => {
                    tracing::error!(kind = error.kind(), detail = %e, "command error")
                }
                _ => tracing::error!(kind = error.kind(), "command error"),
            }
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .content(user_msg)
                        .ephemeral(true),
                )
                .await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                tracing::error!(error = %e, "error in the error handler");
            }
        }
    }
}
