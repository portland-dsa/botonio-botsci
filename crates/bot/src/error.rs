//! The bot's error type and the poise error hook that turns failures into friendly,
//! ephemeral messages (never a raw error, never PII).

use crate::data::{Data, Error};

#[derive(Debug, thiserror::Error)]
pub enum BotError {
    #[error("engine error")]
    Engine(#[from] engine::Error),
    #[error("card lookup failed")]
    Card(#[from] engine::card::CardError),
    #[error("serenity error")]
    Serenity(#[from] serenity::Error),
}

impl BotError {
    /// A stable, PII-free label for logs/metrics - never the error payload, so an
    /// engine/serenity error's Debug chain can't leak a member handle, email, or
    /// `Context` string into the logs (per the SECURITY posture: PII out of logs).
    fn kind(&self) -> &'static str {
        match self {
            BotError::Engine(_) => "engine",
            BotError::Card(engine::card::CardError::NoRecord) => "card_no_record",
            BotError::Serenity(_) => "serenity",
        }
    }
}

/// poise calls this for any command error. Reply with a friendly ephemeral message
/// and log the detail (without member identifiers at info).
pub async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            let user_msg = match &error {
                BotError::Card(engine::card::CardError::NoRecord) => {
                    "I couldn't find a membership record for you. If you think this is wrong, ask a moderator."
                }
                _ => "Something went wrong on my end - please try again in a moment.",
            };
            tracing::error!(kind = error.kind(), "command error");
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
