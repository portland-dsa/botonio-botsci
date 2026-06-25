//! Moderator-driven stores: the [`PgStore`] impls of [`OverrideLog`], [`BulkSessionStore`],
//! and [`GraceStore`] over the `manual_override`, `bulk_verify_*`, and `dues_grace_override`
//! tables.

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::DiscordGuildId;
use engine::store::{
    BulkQueueEntry, BulkQueueKind, BulkScope, BulkSession, BulkSessionStore, BulkStatus,
    GraceOverride, GraceStore, MissCounts, MissState, OverrideLog, OverrideRecord,
};
use engine::util::{DiscordHandle, DiscordUserId};

use crate::PersistenceError;

use super::PgStore;

#[async_trait]
impl OverrideLog for PgStore {
    type Error = PersistenceError;

    /// Stamp a hand approval. `ON CONFLICT DO NOTHING` makes it insert-once: a re-stamp
    /// for the same subject leaves the original `approved_by`/`approved_at` untouched.
    async fn stamp_override(
        &self,
        subject: DiscordUserId,
        approver: DiscordUserId,
        note: Option<String>,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO manual_override (discord_user_id, approved_by, note)
               VALUES ($1, $2, $3) ON CONFLICT (discord_user_id) DO NOTHING"#,
            subject.0 as i64,
            approver.0 as i64,
            note,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The override stamp for `subject`, or `None`. Read-only `SELECT`, which the
    /// runtime role already holds on `manual_override`.
    async fn get_override(
        &self,
        subject: DiscordUserId,
    ) -> Result<Option<OverrideRecord>, PersistenceError> {
        let row = sqlx::query!(
            "SELECT approved_by, approved_at, note FROM manual_override WHERE discord_user_id = $1",
            subject.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| OverrideRecord {
            approved_by: DiscordUserId(r.approved_by as u64),
            approved_at: r.approved_at,
            note: r.note,
        }))
    }

    async fn delete_override(&self, subject: DiscordUserId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM manual_override WHERE discord_user_id = $1",
            subject.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl BulkSessionStore for PgStore {
    type Error = PersistenceError;

    async fn load_session(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<BulkSession>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT scope, status, started_by, created_at, updated_at
               FROM bulk_verify_session WHERE guild_id = $1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let scope = BulkScope::from_token(&r.scope)
            .ok_or_else(|| PersistenceError::BadToken(format!("bulk scope {:?}", r.scope)))?;
        let status = BulkStatus::from_token(&r.status)
            .ok_or_else(|| PersistenceError::BadToken(format!("bulk status {:?}", r.status)))?;
        Ok(Some(BulkSession {
            guild,
            scope,
            status,
            started_by: DiscordUserId(r.started_by as u64),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }))
    }

    async fn start_session(
        &self,
        session: &BulkSession,
        misses: &[BulkQueueEntry],
    ) -> Result<(), PersistenceError> {
        let mut tx = self.pool.begin().await?;
        // Wholesale replace: the CASCADE clears any prior queue with the session row.
        sqlx::query!(
            "DELETE FROM bulk_verify_session WHERE guild_id = $1",
            session.guild.0 as i64
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            r#"INSERT INTO bulk_verify_session (guild_id, scope, status, started_by)
               VALUES ($1, $2, $3, $4)"#,
            session.guild.0 as i64,
            session.scope.as_token(),
            session.status.as_token(),
            session.started_by.0 as i64,
        )
        .execute(&mut *tx)
        .await?;

        if !misses.is_empty() {
            // One UNNEST batch insert, mirroring replace_roster's single round-trip.
            let cap = misses.len();
            let mut ids: Vec<i64> = Vec::with_capacity(cap);
            let mut handles: Vec<Option<String>> = Vec::with_capacity(cap);
            let mut positions: Vec<i32> = Vec::with_capacity(cap);
            let mut states: Vec<String> = Vec::with_capacity(cap);
            let mut kinds: Vec<String> = Vec::with_capacity(cap);
            for m in misses {
                ids.push(m.discord_user_id.0 as i64);
                handles.push(m.handle.as_ref().map(|h| h.0.clone()));
                positions.push(m.position);
                states.push(m.state.as_token().to_owned());
                kinds.push(m.kind.as_token().to_owned());
            }
            sqlx::query!(
                r#"INSERT INTO bulk_verify_miss (guild_id, discord_user_id, handle, position, state, kind)
                   SELECT $1, * FROM UNNEST($2::bigint[], $3::text[], $4::int[], $5::text[], $6::text[])"#,
                session.guild.0 as i64,
                &ids,
                &handles as &[Option<String>],
                &positions,
                &states,
                &kinds,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn next_pending(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<BulkQueueEntry>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT discord_user_id, handle, position, state, kind
               FROM bulk_verify_miss
               WHERE guild_id = $1 AND state = 'pending'
               ORDER BY position ASC LIMIT 1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let state = MissState::from_token(&r.state)
            .ok_or_else(|| PersistenceError::BadToken(format!("miss state {:?}", r.state)))?;
        let kind = BulkQueueKind::from_token(&r.kind)
            .ok_or_else(|| PersistenceError::BadToken(format!("queue kind {:?}", r.kind)))?;
        Ok(Some(BulkQueueEntry {
            discord_user_id: DiscordUserId(r.discord_user_id as u64),
            handle: r.handle.map(DiscordHandle),
            position: r.position,
            state,
            kind,
        }))
    }

    async fn mark_miss(
        &self,
        guild: DiscordGuildId,
        member: DiscordUserId,
        state: MissState,
    ) -> Result<(), PersistenceError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            r#"UPDATE bulk_verify_miss SET state = $3
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            member.0 as i64,
            state.as_token(),
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE bulk_verify_session SET updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn counts(&self, guild: DiscordGuildId) -> Result<MissCounts, PersistenceError> {
        let rows = sqlx::query!(
            r#"SELECT state, COUNT(*) AS "n!" FROM bulk_verify_miss
               WHERE guild_id = $1 GROUP BY state"#,
            guild.0 as i64
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = MissCounts::default();
        for r in rows {
            match MissState::from_token(&r.state) {
                Some(MissState::Pending) => counts.pending = r.n as usize,
                Some(MissState::Verified) => counts.verified = r.n as usize,
                Some(MissState::Skipped) => counts.skipped = r.n as usize,
                None => {
                    return Err(PersistenceError::BadToken(format!(
                        "miss state {:?}",
                        r.state
                    )));
                }
            }
        }
        Ok(counts)
    }

    async fn complete_session(&self, guild: DiscordGuildId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "UPDATE bulk_verify_session SET status = 'complete', updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn abandon_session(&self, guild: DiscordGuildId) -> Result<(), PersistenceError> {
        sqlx::query!(
            "UPDATE bulk_verify_session SET status = 'abandoned', updated_at = now() WHERE guild_id = $1",
            guild.0 as i64
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl GraceStore for PgStore {
    type Error = PersistenceError;

    /// `true` when `id` has a row in `dues_grace_override` whose `grace_until >= today`.
    async fn active_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        today: NaiveDate,
    ) -> Result<bool, PersistenceError> {
        let active = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM dues_grace_override
                   WHERE guild_id = $1 AND discord_user_id = $2
                     AND grace_until >= $3
               ) AS "active!""#,
            guild.0 as i64,
            id.0 as i64,
            today,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(active)
    }

    /// The full grace stamp for `id`, or `None` if there is none.
    async fn grace_override(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<Option<GraceOverride>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT grace_until, granted_by, granted_at, reason
               FROM dues_grace_override
               WHERE guild_id = $1 AND discord_user_id = $2"#,
            guild.0 as i64,
            id.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| GraceOverride {
            until: r.grace_until,
            granted_by: DiscordUserId(r.granted_by as u64),
            granted_at: r.granted_at,
            reason: r.reason,
        }))
    }

    /// Upsert a grace stamp (insert or extend the window).
    async fn set_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
        until: NaiveDate,
        granted_by: DiscordUserId,
        reason: Option<String>,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"INSERT INTO dues_grace_override
                 (guild_id, discord_user_id, grace_until, granted_by, granted_at, reason)
               VALUES ($1, $2, $3, $4, now(), $5)
               ON CONFLICT (guild_id, discord_user_id) DO UPDATE SET
                 grace_until = EXCLUDED.grace_until,
                 granted_by  = EXCLUDED.granted_by,
                 granted_at  = EXCLUDED.granted_at,
                 reason      = EXCLUDED.reason"#,
            guild.0 as i64,
            id.0 as i64,
            until,
            granted_by.0 as i64,
            reason,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a member's grace stamp. A member with none is a no-op.
    async fn clear_grace(
        &self,
        guild: DiscordGuildId,
        id: DiscordUserId,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            "DELETE FROM dues_grace_override WHERE guild_id = $1 AND discord_user_id = $2",
            guild.0 as i64,
            id.0 as i64,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
