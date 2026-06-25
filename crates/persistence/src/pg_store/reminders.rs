//! The dues-reminder bookkeeping: the [`PgStore`] impls of [`ReminderStore`] (cycle state +
//! opt-out) and [`MessageTemplates`] over the `dues_reminder_state`, `dues_reminder_optout`,
//! and `message_template` tables.

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::DiscordGuildId;
use engine::reminders::{MessageKind, Milestone};
use engine::store::{MessageTemplates, OptOutSource, ReminderCycleState, ReminderStore};
use engine::util::DiscordUserId;

use crate::PersistenceError;

use super::PgStore;

#[async_trait]
impl ReminderStore for PgStore {
    type Error = PersistenceError;

    /// The member's per-cycle reminder state, or `None` if they have never been reminded.
    async fn reminder_state(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<ReminderCycleState>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT cycle_xdate, last_sent, expiring_marked, thread_id
               FROM dues_reminder_state
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let last_sent = r
            .last_sent
            .as_deref()
            .map(|t| {
                Milestone::from_token(t)
                    .ok_or_else(|| PersistenceError::BadToken(format!("milestone {:?}", t)))
            })
            .transpose()?;
        Ok(Some(ReminderCycleState {
            cycle_xdate: r.cycle_xdate,
            last_sent,
            expiring_marked: r.expiring_marked,
            thread_id: r.thread_id,
        }))
    }

    /// Record that `milestone` was sent for the cycle ending `cycle_xdate`. Upserts the row
    /// and resets `expiring_marked` to false (and clears `last_sent`) when `cycle_xdate`
    /// differs from the stored one (the cycle has turned); otherwise preserves
    /// `expiring_marked`.
    async fn record_sent(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        milestone: Milestone,
        thread_id: i64,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, expiring_marked, thread_id, updated_at)
               VALUES ($1, $2, $3, $4, false, $5, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 last_sent        = EXCLUDED.last_sent,
                 cycle_xdate      = EXCLUDED.cycle_xdate,
                 thread_id        = EXCLUDED.thread_id,
                 expiring_marked  = CASE
                                        WHEN dues_reminder_state.cycle_xdate <> EXCLUDED.cycle_xdate
                                        THEN false
                                        ELSE dues_reminder_state.expiring_marked
                                    END,
                 updated_at       = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
            milestone.as_token(),
            thread_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist the lifecycle thread id without recording a send, preserving the existing cycle
    /// fields. Seeds a new row with `cycle_xdate` and no recorded milestone.
    async fn set_thread(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        thread_id: i64,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, expiring_marked, thread_id, updated_at)
               VALUES ($1, $2, $3, NULL, false, $4, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 thread_id  = EXCLUDED.thread_id,
                 updated_at = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
            thread_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set the Dues Expiring marker flag for the member's current cycle, upserting the row
    /// if needed. The sweep flips this on grant/removal so the marker is written to Discord
    /// once on entry and once on exit, not every pass.
    async fn set_expiring_marked(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        cycle_xdate: NaiveDate,
        marked: bool,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_state
                 (guild_id, discord_user_id, cycle_xdate, last_sent, expiring_marked, thread_id, updated_at)
               VALUES ($1, $2, $3, NULL, $4, NULL, now())
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 cycle_xdate     = EXCLUDED.cycle_xdate,
                 expiring_marked = EXCLUDED.expiring_marked,
                 updated_at      = now()"#,
            guild.0 as i64,
            id.0 as i64,
            cycle_xdate,
            marked,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every member whose cycle state currently has `expiring_marked = true`. Drives the
    /// one-time cleanup when reminders are disabled.
    async fn marked_members(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Vec<DiscordUserId>, PersistenceError> {
        let rows = sqlx::query_scalar!(
            r#"SELECT discord_user_id FROM dues_reminder_state
               WHERE guild_id = $1 AND expiring_marked"#,
            guild.0 as i64,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|i| DiscordUserId(i as u64)).collect())
    }

    /// Whether `id` has a permanent opt-out row in `dues_reminder_optout`.
    async fn is_opted_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<bool, PersistenceError> {
        let opted_out = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM dues_reminder_optout
                   WHERE guild_id = $1 AND discord_user_id = $2
               ) AS "opted_out!""#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(opted_out)
    }

    /// Insert the permanent opt-out row. On conflict (already opted out) updates the
    /// source so a moderator reversal then re-opt-out records the latest actor.
    async fn opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        source: OptOutSource,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_reminder_optout
                 (guild_id, discord_user_id, opted_out_at, source)
               VALUES ($1, $2, now(), $3)
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 opted_out_at = EXCLUDED.opted_out_at,
                 source       = EXCLUDED.source"#,
            guild.0 as i64,
            id.0 as i64,
            source.as_token(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove the opt-out row. A member with none is a no-op.
    async fn clear_opt_out(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM dues_reminder_optout WHERE guild_id = $1 AND discord_user_id = $2",
            guild.0 as i64,
            id.0 as i64,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl MessageTemplates for PgStore {
    type Error = PersistenceError;

    /// The stored body for `kind`, or `None` to use the built-in default.
    async fn template(
        &self,
        guild: DiscordGuildId,
        kind: MessageKind,
    ) -> Result<Option<String>, PersistenceError> {
        let row = sqlx::query_scalar!(
            "SELECT body FROM message_template WHERE guild_id = $1 AND kind = $2",
            guild.0 as i64,
            kind.as_token(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Upsert the body for `kind`.
    async fn set_template(
        &self,
        guild: DiscordGuildId,
        kind: MessageKind,
        body: String,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO message_template (guild_id, kind, body)
               VALUES ($1, $2, $3)
               ON CONFLICT (guild_id, kind) DO UPDATE SET body = EXCLUDED.body"#,
            guild.0 as i64,
            kind.as_token(),
            body,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
