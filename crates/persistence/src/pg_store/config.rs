//! The `guild_config` row and the channel-permission snapshots: the [`PgStore`] impls of
//! [`ConfigStore`] and [`ChannelSnapshotStore`].

use async_trait::async_trait;

use domain::{DiscordChannelId, DiscordGuildId, DiscordMessageId, DiscordRoleId};
use engine::channels::{ChannelSnapshot, SNAPSHOT_FORMAT_VERSION, SnapshotMeta};
use engine::store::{ChannelSnapshotStore, ConfigStore, GuildConfig, MessageRef};

use crate::PersistenceError;

use super::PgStore;

/// One `guild_config` row as `sqlx` reads it: every snowflake an `Option<i64>`.
struct GuildConfigRow {
    moderator_role_id: Option<i64>,
    member_role_id: Option<i64>,
    dues_expired_role_id: Option<i64>,
    unverified_role_id: Option<i64>,
    manual_override_role_id: Option<i64>,
    dues_expiring_role_id: Option<i64>,
    mod_approval_channel_id: Option<i64>,
    unverified_channel_id: Option<i64>,
    dues_expired_channel_id: Option<i64>,
    verification_log_channel_id: Option<i64>,
    dues_signup_url: Option<String>,
    reminders_enabled: bool,
    scan_enabled: bool,
    sso_enabled: bool,
    unverified_prompt_channel_id: Option<i64>,
    unverified_prompt_message_id: Option<i64>,
    dues_banner_channel_id: Option<i64>,
    dues_banner_message_id: Option<i64>,
}

impl From<GuildConfigRow> for GuildConfig {
    fn from(r: GuildConfigRow) -> Self {
        let role = |v: Option<i64>| v.map(|i| DiscordRoleId(i as u64));
        let chan = |v: Option<i64>| v.map(|i| DiscordChannelId(i as u64));
        // A posted-message reference is stored as a channel/message id pair and is only a
        // reference when both are present; a half-set row collapses to `None`.
        let msg_ref = |chan: Option<i64>, msg: Option<i64>| match (chan, msg) {
            (Some(c), Some(m)) => Some(MessageRef {
                channel: DiscordChannelId(c as u64),
                message: DiscordMessageId(m as u64),
            }),
            _ => None,
        };
        GuildConfig {
            moderator_role: role(r.moderator_role_id),
            member_role: role(r.member_role_id),
            dues_expired_role: role(r.dues_expired_role_id),
            unverified_role: role(r.unverified_role_id),
            manual_override_role: role(r.manual_override_role_id),
            dues_expiring_role: role(r.dues_expiring_role_id),
            mod_approval_channel: chan(r.mod_approval_channel_id),
            unverified_channel: chan(r.unverified_channel_id),
            dues_expired_channel: chan(r.dues_expired_channel_id),
            verification_log_channel: chan(r.verification_log_channel_id),
            dues_signup_url: r.dues_signup_url,
            reminders_enabled: r.reminders_enabled,
            scan_enabled: r.scan_enabled,
            sso_enabled: r.sso_enabled,
            unverified_prompt: msg_ref(
                r.unverified_prompt_channel_id,
                r.unverified_prompt_message_id,
            ),
            dues_banner: msg_ref(r.dues_banner_channel_id, r.dues_banner_message_id),
        }
    }
}

#[async_trait]
impl ConfigStore for PgStore {
    type Error = PersistenceError;

    async fn load_config(&self, guild: DiscordGuildId) -> Result<GuildConfig, PersistenceError> {
        let row = sqlx::query_as!(
            GuildConfigRow,
            r#"SELECT moderator_role_id, member_role_id, dues_expired_role_id,
                      unverified_role_id, manual_override_role_id, dues_expiring_role_id,
                      mod_approval_channel_id, unverified_channel_id, dues_expired_channel_id,
                      verification_log_channel_id,
                      dues_signup_url, reminders_enabled, scan_enabled, sso_enabled,
                      unverified_prompt_channel_id, unverified_prompt_message_id,
                      dues_banner_channel_id, dues_banner_message_id
               FROM guild_config WHERE guild_id = $1"#,
            guild.0 as i64
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(GuildConfig::from).unwrap_or_default())
    }

    async fn save_config(
        &self,
        guild: DiscordGuildId,
        config: &GuildConfig,
    ) -> Result<(), PersistenceError> {
        let role = |r: Option<DiscordRoleId>| r.map(|x| x.0 as i64);
        let chan = |c: Option<DiscordChannelId>| c.map(|x| x.0 as i64);
        // A posted-message reference splits back into its channel/message id columns.
        let ref_chan = |m: Option<MessageRef>| m.map(|r| r.channel.0 as i64);
        let ref_msg = |m: Option<MessageRef>| m.map(|r| r.message.0 as i64);
        sqlx::query!(
            r#"INSERT INTO guild_config
                 (guild_id, moderator_role_id, member_role_id, dues_expired_role_id,
                  unverified_role_id, manual_override_role_id, dues_expiring_role_id,
                  mod_approval_channel_id, unverified_channel_id, dues_expired_channel_id,
                  verification_log_channel_id,
                  dues_signup_url, reminders_enabled, scan_enabled,
                  unverified_prompt_channel_id, unverified_prompt_message_id,
                  dues_banner_channel_id, dues_banner_message_id, sso_enabled, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                       $15, $16, $17, $18, $19, now())
               ON CONFLICT (guild_id) DO UPDATE SET
                 moderator_role_id            = EXCLUDED.moderator_role_id,
                 member_role_id               = EXCLUDED.member_role_id,
                 dues_expired_role_id         = EXCLUDED.dues_expired_role_id,
                 unverified_role_id           = EXCLUDED.unverified_role_id,
                 manual_override_role_id      = EXCLUDED.manual_override_role_id,
                 dues_expiring_role_id        = EXCLUDED.dues_expiring_role_id,
                 mod_approval_channel_id      = EXCLUDED.mod_approval_channel_id,
                 unverified_channel_id        = EXCLUDED.unverified_channel_id,
                 dues_expired_channel_id      = EXCLUDED.dues_expired_channel_id,
                 verification_log_channel_id  = EXCLUDED.verification_log_channel_id,
                 dues_signup_url              = EXCLUDED.dues_signup_url,
                 reminders_enabled            = EXCLUDED.reminders_enabled,
                 scan_enabled                 = EXCLUDED.scan_enabled,
                 sso_enabled                  = EXCLUDED.sso_enabled,
                 unverified_prompt_channel_id = EXCLUDED.unverified_prompt_channel_id,
                 unverified_prompt_message_id = EXCLUDED.unverified_prompt_message_id,
                 dues_banner_channel_id       = EXCLUDED.dues_banner_channel_id,
                 dues_banner_message_id       = EXCLUDED.dues_banner_message_id,
                 updated_at                   = now()"#,
            guild.0 as i64,
            role(config.moderator_role),
            role(config.member_role),
            role(config.dues_expired_role),
            role(config.unverified_role),
            role(config.manual_override_role),
            role(config.dues_expiring_role),
            chan(config.mod_approval_channel),
            chan(config.unverified_channel),
            chan(config.dues_expired_channel),
            chan(config.verification_log_channel),
            config.dues_signup_url,
            config.reminders_enabled,
            config.scan_enabled,
            ref_chan(config.unverified_prompt),
            ref_msg(config.unverified_prompt),
            ref_chan(config.dues_banner),
            ref_msg(config.dues_banner),
            config.sso_enabled,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Per-guild snapshot history is bounded on every save: keep at most this many of the
/// newest snapshots, and (in the same prune) drop any older than the 6-month TTL spelled
/// in [`save_snapshot`](PgStore::save_snapshot)'s DELETE. The undo stack is for recent
/// mistakes, so a row several applies back - or older than the retention window - is no
/// longer a useful restore target and is reaped rather than kept forever.
const SNAPSHOT_KEEP_MAX: i64 = 5;

#[async_trait]
impl ChannelSnapshotStore for PgStore {
    type Error = PersistenceError;

    /// Append a whole-guild channel-permission snapshot, then prune the guild's history.
    /// Each call inserts a new row (history is never overwritten, so successive saves form
    /// an undo stack) and the same transaction reaps every row past the bound: older than
    /// the 6-month TTL, or beyond the newest [`SNAPSHOT_KEEP_MAX`], whichever removes more.
    /// The `channels` Vec is stored as JSONB so the restore path can deserialize it without
    /// a per-overwrite join.
    async fn save_snapshot(&self, snapshot: &ChannelSnapshot) -> Result<(), PersistenceError> {
        let channels = serde_json::to_value(&snapshot.channels)?;
        let guild = snapshot.guild_id.0 as i64;

        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "INSERT INTO channel_perms_snapshot (guild_id, saved_at, format_version, channels) \
             VALUES ($1, $2, $3, $4)",
            guild,
            snapshot.saved_at,
            snapshot.format_version as i32,
            channels,
        )
        .execute(&mut *tx)
        .await?;
        // Keep the undo stack bounded: drop rows older than the 6-month TTL, and any beyond
        // the newest SNAPSHOT_KEEP_MAX for this guild. Same transaction as the insert, so
        // the cap holds atomically and the table can never grow without limit.
        sqlx::query!(
            "DELETE FROM channel_perms_snapshot \
             WHERE guild_id = $1 \
               AND (saved_at < now() - interval '6 months' \
                    OR id NOT IN ( \
                        SELECT id FROM channel_perms_snapshot \
                        WHERE guild_id = $1 \
                        ORDER BY saved_at DESC, id DESC \
                        LIMIT $2 \
                    ))",
            guild,
            SNAPSHOT_KEEP_MAX,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The most recent snapshot for `guild`, ordered by `saved_at DESC` so the
    /// same newest-first rule the in-memory store enforces by insertion-order also
    /// holds in Postgres.
    async fn latest_snapshot(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Option<ChannelSnapshot>, PersistenceError> {
        let row = sqlx::query!(
            r#"SELECT format_version, guild_id, saved_at,
                      channels AS "channels: serde_json::Value"
               FROM channel_perms_snapshot
               WHERE guild_id = $1
               ORDER BY saved_at DESC LIMIT 1"#,
            guild.0 as i64,
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        if r.format_version > SNAPSHOT_FORMAT_VERSION as i32 {
            return Err(PersistenceError::SnapshotVersion {
                found: r.format_version,
                known: SNAPSHOT_FORMAT_VERSION as i32,
            });
        }
        let channels = serde_json::from_value(r.channels)?;
        Ok(Some(ChannelSnapshot {
            format_version: r.format_version as u32,
            guild_id: DiscordGuildId(r.guild_id as u64),
            saved_at: r.saved_at,
            channels,
        }))
    }

    /// All snapshots' metadata for `guild`, newest first - for the restore picker. Uses
    /// `jsonb_array_length` so only the length, not the full channel data, crosses the
    /// wire.
    async fn list_snapshots(
        &self,
        guild: DiscordGuildId,
    ) -> Result<Vec<SnapshotMeta>, PersistenceError> {
        let rows = sqlx::query!(
            r#"SELECT saved_at, jsonb_array_length(channels) AS "channel_count!"
               FROM channel_perms_snapshot
               WHERE guild_id = $1
               ORDER BY saved_at DESC"#,
            guild.0 as i64,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| SnapshotMeta {
                saved_at: r.saved_at,
                channel_count: r.channel_count as usize,
            })
            .collect())
    }
}
