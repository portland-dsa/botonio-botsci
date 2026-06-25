//! [`Channels`] - the IO verbs of the terraform, the analog of
//! [`verify::Member`](crate::verify::Member). It owns the Discord read/write and
//! the snapshot store; all decisions are delegated to the pure [`plan`](super::plan)
//! layer. One concrete [`ChannelsError`] surfaces every failure.

use std::collections::HashMap;

use chrono::Utc;

use domain::DiscordChannelId;

use crate::backends::discord::{DiscordChannel, DiscordClient, DiscordError, GuildChannels};
use crate::seam::{Progress, ProgressBar};
use crate::store::ChannelSnapshotStore;

use super::model::{SetupConfig, overwrites_equal};
use super::plan::{ChannelPlan, DesyncReport, desync_report, resolve_plan, verification_breaches};
use super::snapshot::{ChannelSnapshot, SnapshotMeta};

/// Consecutive failed channel writes before a run aborts - catches a dead token
/// without bailing on one sporadic error.
const CIRCUIT_BREAKER: usize = 10;

/// Everything a terraform verb can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ChannelsError {
    #[error("no unverified channel is configured; run /setup first")]
    NoUnverifiedChannel,
    #[error("the unverified and dues-expired channel sets overlap")]
    ChannelSetsOverlap,
    #[error("plan would hide {} unverified channel(s) from the Unverified role", .0.len())]
    VerificationBreach(Vec<DiscordChannelId>),
    #[error("the server changed since the preview: expected {expected} writes, found {found}")]
    PlanChanged { expected: usize, found: usize },
    #[error("aborted after {failed} consecutive write failures; restore from the pre-run snapshot")]
    CircuitBreaker { failed: usize },
    #[error("no snapshot exists for this guild")]
    NoSnapshot,
    /// Boxed: serenity::Error is large; keep ChannelsError small (clippy::result_large_err).
    #[error(transparent)]
    Discord(Box<DiscordError>),
    #[error("snapshot store: {0}")]
    Snapshot(String),
}

impl From<DiscordError> for ChannelsError {
    fn from(e: DiscordError) -> Self {
        ChannelsError::Discord(Box::new(e))
    }
}

/// What an apply did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub written: usize,
    pub failed: usize,
}

/// What a restore did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub written: usize,
    /// Channels already equal to the snapshot - no write needed.
    pub skipped_no_op: usize,
    pub failed: usize,
}

/// The terraform handle over a Discord client and a snapshot store.
pub struct Channels<'a, D: DiscordClient, S: ChannelSnapshotStore> {
    discord: &'a D,
    store: &'a S,
}

impl<'a, D: DiscordClient, S: ChannelSnapshotStore> Channels<'a, D, S> {
    pub fn new(discord: &'a D, store: &'a S) -> Self {
        Self { discord, store }
    }

    fn store_err(e: S::Error) -> ChannelsError {
        ChannelsError::Snapshot(e.to_string())
    }

    fn validate(cfg: &SetupConfig) -> Result<(), ChannelsError> {
        if cfg.unverified_channels.is_empty() {
            return Err(ChannelsError::NoUnverifiedChannel);
        }
        if cfg
            .unverified_channels
            .intersection(&cfg.dues_expired_channels)
            .next()
            .is_some()
        {
            return Err(ChannelsError::ChannelSetsOverlap);
        }
        Ok(())
    }

    /// Read-only desync report - the `check` subcommand.
    pub async fn check(&self) -> Result<DesyncReport, ChannelsError> {
        let read = self.discord.read_channels().await?;
        Ok(desync_report(&read.channels))
    }

    /// Snapshot the current overwrites to the store - the `save` subcommand. Reads the
    /// guild once, then delegates to [`snapshot_read`](Self::snapshot_read).
    pub async fn save(&self) -> Result<ChannelSnapshot, ChannelsError> {
        let read = self.discord.read_channels().await?;
        self.snapshot_read(&read).await
    }

    /// Snapshot an already-read channel list to the store, so the `apply` path - which
    /// has just read the guild to resolve its plan - does not read it a second time.
    async fn snapshot_read(&self, read: &GuildChannels) -> Result<ChannelSnapshot, ChannelsError> {
        let snap = ChannelSnapshot::from_channels(read.guild_id, &read.channels, Utc::now());
        self.store
            .save_snapshot(&snap)
            .await
            .map_err(Self::store_err)?;
        Ok(snap)
    }

    /// Read, validate, resolve, and guard - the preview the report renders. No writes.
    pub async fn plan(&self, cfg: &SetupConfig) -> Result<ChannelPlan, ChannelsError> {
        Self::validate(cfg)?;
        let read = self.discord.read_channels().await?;
        let plan = resolve_plan(&read.channels, cfg, read.everyone_base_view, Utc::now());
        let breaches = verification_breaches(&plan, cfg);
        if !breaches.is_empty() {
            return Err(ChannelsError::VerificationBreach(breaches));
        }
        Ok(plan)
    }

    /// Snapshot, re-resolve, confirm the planned write-set still matches the preview,
    /// then apply each planned write with the circuit breaker. `expected` is the plan
    /// the moderator confirmed against; its
    /// [`write_signature`](ChannelPlan::write_signature) must still match the
    /// freshly-resolved plan, so a server that drifted into a different set of writes
    /// of the same count is rejected just like a different count.
    pub async fn apply(
        &self,
        cfg: &SetupConfig,
        expected: &ChannelPlan,
        progress: &impl Progress,
    ) -> Result<ApplyOutcome, ChannelsError> {
        Self::validate(cfg)?;

        // One guild read backs both the recovery snapshot and the plan, so the snapshot
        // captures exactly the state the plan was resolved against.
        let read = self.discord.read_channels().await?;
        self.snapshot_read(&read).await?; // auto-snapshot before any writes

        let plan = resolve_plan(&read.channels, cfg, read.everyone_base_view, Utc::now());
        let breaches = verification_breaches(&plan, cfg);
        if !breaches.is_empty() {
            return Err(ChannelsError::VerificationBreach(breaches));
        }
        if plan.write_signature() != expected.write_signature() {
            return Err(ChannelsError::PlanChanged {
                expected: expected.counts.writes,
                found: plan.counts.writes,
            });
        }

        let writes: Vec<&_> = plan.writes().collect();
        let bar = progress.bar(writes.len() as u64, "applying channel permissions");
        let mut out = ApplyOutcome::default();
        let mut consecutive = 0usize;
        for p in writes {
            bar.inc(1);
            match self
                .discord
                .set_channel_overwrites(p.id, &p.final_overwrites)
                .await
            {
                Ok(()) => {
                    out.written += 1;
                    consecutive = 0;
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %p.id.0,
                        name = %p.name,
                        error = %e,
                        "channel write failed"
                    );
                    out.failed += 1;
                    consecutive += 1;
                    if consecutive >= CIRCUIT_BREAKER {
                        bar.abandon_with_message("aborted (circuit breaker)");
                        return Err(ChannelsError::CircuitBreaker { failed: out.failed });
                    }
                }
            }
        }
        bar.finish_and_clear();
        Ok(out)
    }

    /// Write a snapshot's overwrites back - the `restore` subcommand. Skips a
    /// channel already equal to its snapshot (the no-op skip) and circuit-breaks on
    /// a run of failures.
    pub async fn restore(
        &self,
        snapshot: &ChannelSnapshot,
        progress: &impl Progress,
    ) -> Result<RestoreOutcome, ChannelsError> {
        let read = self.discord.read_channels().await?;
        let live: HashMap<DiscordChannelId, &DiscordChannel> =
            read.channels.iter().map(|c| (c.id, c)).collect();

        let bar = progress.bar(
            snapshot.channels.len() as u64,
            "restoring channel permissions",
        );
        let mut out = RestoreOutcome::default();
        let mut consecutive = 0usize;
        for sc in &snapshot.channels {
            bar.inc(1);
            // No-op skip: already matches the snapshot, nothing to write.
            if live
                .get(&sc.id)
                .is_some_and(|c| overwrites_equal(&c.overwrites, &sc.overwrites))
            {
                out.skipped_no_op += 1;
                continue;
            }
            match self
                .discord
                .set_channel_overwrites(sc.id, &sc.overwrites)
                .await
            {
                Ok(()) => {
                    out.written += 1;
                    consecutive = 0;
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %sc.id.0,
                        name = %sc.name,
                        error = %e,
                        "restore write failed"
                    );
                    out.failed += 1;
                    consecutive += 1;
                    if consecutive >= CIRCUIT_BREAKER {
                        bar.abandon_with_message("aborted (circuit breaker)");
                        return Err(ChannelsError::CircuitBreaker { failed: out.failed });
                    }
                }
            }
        }
        bar.finish_and_clear();
        Ok(out)
    }

    pub async fn latest_snapshot(&self) -> Result<ChannelSnapshot, ChannelsError> {
        let guild = self.discord.guild_id();
        self.store
            .latest_snapshot(guild)
            .await
            .map_err(Self::store_err)?
            .ok_or(ChannelsError::NoSnapshot)
    }

    pub async fn list_snapshots(&self) -> Result<Vec<SnapshotMeta>, ChannelsError> {
        let guild = self.discord.guild_id();
        self.store
            .list_snapshots(guild)
            .await
            .map_err(Self::store_err)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use domain::{DiscordChannelId, DiscordRoleId, DiscordUserId};

    use crate::backends::discord::{ChannelKind, DiscordChannel, FakeDiscord, PermOverwrite};
    use crate::seam::NoProgress;
    use crate::store::InMemoryStore;

    use super::super::model::SetupConfig;
    use super::*;

    /// Build a minimal [`SetupConfig`] with one unverified channel.
    fn cfg_with_unverified(ch_id: u64) -> SetupConfig {
        let mut unverified_channels = BTreeSet::new();
        unverified_channels.insert(DiscordChannelId(ch_id));
        SetupConfig {
            everyone: DiscordRoleId(1),
            member_role: DiscordRoleId(10),
            dues_expired_role: DiscordRoleId(11),
            dues_expiring_role: None,
            unverified_role: DiscordRoleId(12),
            moderator_role: DiscordRoleId(40),
            bot_user: DiscordUserId(99),
            unverified_channels,
            dues_expired_channels: BTreeSet::new(),
            exclude_channels: BTreeSet::new(),
        }
    }

    /// Build an empty `SetupConfig` (no unverified channels).
    fn cfg_empty() -> SetupConfig {
        SetupConfig {
            everyone: DiscordRoleId(1),
            member_role: DiscordRoleId(10),
            dues_expired_role: DiscordRoleId(11),
            dues_expiring_role: None,
            unverified_role: DiscordRoleId(12),
            moderator_role: DiscordRoleId(40),
            bot_user: DiscordUserId(99),
            unverified_channels: BTreeSet::new(),
            dues_expired_channels: BTreeSet::new(),
            exclude_channels: BTreeSet::new(),
        }
    }

    /// Build a plain text channel with no overwrites.
    fn text_channel(id: u64) -> DiscordChannel {
        DiscordChannel {
            id: DiscordChannelId(id),
            name: format!("ch-{id}"),
            kind: ChannelKind::Text,
            parent_id: None,
            position: 0,
            overwrites: Vec::new(),
        }
    }

    /// Build a saved channel with a populated overwrite to distinguish it from the
    /// default empty state.
    fn saved_channel_with_ow(id: u64) -> super::super::snapshot::SavedChannel {
        super::super::snapshot::SavedChannel {
            id: DiscordChannelId(id),
            name: format!("ch-{id}"),
            kind: ChannelKind::Text,
            parent_id: None,
            overwrites: vec![PermOverwrite {
                target: crate::backends::discord::OverwriteTarget::Role(DiscordRoleId(1)),
                allow: crate::backends::discord::Permissions::VIEW_CHANNEL,
                deny: crate::backends::discord::Permissions::empty(),
            }],
        }
    }

    #[tokio::test]
    async fn plan_refuses_without_unverified_channel() {
        let fake = FakeDiscord::new()
            .with_channels(vec![text_channel(1)])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        let result = channels.plan(&cfg_empty()).await;
        assert!(
            matches!(result, Err(ChannelsError::NoUnverifiedChannel)),
            "expected NoUnverifiedChannel, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn plan_refuses_overlapping_sets() {
        let id = DiscordChannelId(42);
        let mut cfg = cfg_with_unverified(42);
        cfg.dues_expired_channels.insert(id); // same id in both sets

        let fake = FakeDiscord::new()
            .with_channels(vec![text_channel(42)])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        let result = channels.plan(&cfg).await;
        assert!(
            matches!(result, Err(ChannelsError::ChannelSetsOverlap)),
            "expected ChannelSetsOverlap, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn apply_snapshots_then_writes_expected_count() {
        // A public channel (id 1) and the nominated unverified channel (id 2).
        // base_view=true so the public channel gets a MemberOnly write; the
        // unverified channel gets its restrict write.
        let cfg = cfg_with_unverified(2);
        let fake = FakeDiscord::new()
            .with_channels(vec![text_channel(1), text_channel(2)])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        // Plan first to learn the expected write count.
        let plan = channels.plan(&cfg).await.expect("plan should succeed");
        let expected = plan.counts.writes;
        assert!(expected > 0, "test needs at least one write");

        // apply must succeed and written count must match.
        let outcome = channels
            .apply(&cfg, &plan, &NoProgress)
            .await
            .expect("apply should succeed");
        assert_eq!(
            outcome.written, expected,
            "written count must equal expected_writes"
        );

        // Auto-snapshot was taken before the writes.
        let snap = channels
            .latest_snapshot()
            .await
            .expect("latest_snapshot must be Ok after apply");
        assert_eq!(
            snap.channels.len(),
            2,
            "snapshot must capture both channels"
        );

        // The write log must contain an entry for each expected write.
        let written_ids: Vec<DiscordChannelId> = fake
            .written_overwrites()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            written_ids.len(),
            expected,
            "fake write log must have the expected count"
        );
    }

    #[tokio::test]
    async fn apply_rejects_a_drifted_plan() {
        use super::super::plan::resolve_plan;

        let cfg = cfg_with_unverified(2);
        let fake = FakeDiscord::new()
            .with_channels(vec![text_channel(1), text_channel(2)])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        // A preview resolved against one extra public channel the live server does not
        // have: same restrict write for ch-2, but an extra MemberOnly write for ch-3.
        // Its write-set no longer matches reality, so the drift guard must reject it -
        // even though a count-only check could have been fooled by an equal total.
        let stale_preview = resolve_plan(
            &[text_channel(1), text_channel(2), text_channel(3)],
            &cfg,
            true,
            Utc::now(),
        );

        let result = channels.apply(&cfg, &stale_preview, &NoProgress).await;
        assert!(
            matches!(result, Err(ChannelsError::PlanChanged { .. })),
            "expected PlanChanged, got: {result:?}"
        );

        // No overwrites were written (the check fires before the write loop).
        assert!(
            fake.written_overwrites().is_empty(),
            "no writes must occur when PlanChanged fires"
        );

        // The auto-snapshot was taken before the drift guard, so recovery is still
        // possible even when PlanChanged aborts the run.
        assert!(
            channels.latest_snapshot().await.is_ok(),
            "apply must snapshot before the drift guard, so recovery stays possible even on PlanChanged"
        );
    }

    #[tokio::test]
    async fn apply_is_idempotent_on_rerun() {
        let cfg = cfg_with_unverified(2);
        let fake = FakeDiscord::new()
            .with_channels(vec![text_channel(1), text_channel(2)])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        // First apply.
        let plan = channels.plan(&cfg).await.expect("plan should succeed");
        channels
            .apply(&cfg, &plan, &NoProgress)
            .await
            .expect("first apply should succeed");

        // Second plan after the first apply must see zero writes (idempotent).
        let plan2 = channels
            .plan(&cfg)
            .await
            .expect("second plan should succeed");
        assert_eq!(
            plan2.counts.writes, 0,
            "second plan must report zero writes (already in desired state)"
        );

        // A second apply confirming the zero-write plan2 must write nothing.
        let outcome2 = channels
            .apply(&cfg, &plan2, &NoProgress)
            .await
            .expect("second apply should succeed");
        assert_eq!(outcome2.written, 0, "second apply must write nothing");
    }

    #[tokio::test]
    async fn restore_writes_back_changed_channels_only() {
        // Two channels; save a snapshot when both are empty, then mutate ch-1.
        let ch1 = text_channel(1);
        let ch2 = text_channel(2);
        let fake = FakeDiscord::new()
            .with_channels(vec![ch1, ch2])
            .set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        // Snapshot while both are in their initial empty-overwrite state.
        let snap = channels.save().await.expect("save should succeed");

        // Mutate ch-1 directly (simulates a manual Discord edit or a prior apply).
        let new_ows = vec![PermOverwrite {
            target: crate::backends::discord::OverwriteTarget::Role(DiscordRoleId(99)),
            allow: crate::backends::discord::Permissions::VIEW_CHANNEL,
            deny: crate::backends::discord::Permissions::empty(),
        }];
        fake.set_channel_overwrites(DiscordChannelId(1), &new_ows)
            .await
            .expect("mutate ch-1");

        // Clear the write log so we can assert cleanly on the restore calls.
        // The FakeDiscord doesn't expose a reset, so we count relative writes.
        let writes_before = fake.written_overwrites().len();

        // restore must rewrite ch-1 (changed) and skip ch-2 (still matches snapshot).
        let outcome = channels
            .restore(&snap, &NoProgress)
            .await
            .expect("restore should succeed");
        assert_eq!(
            outcome.written, 1,
            "only the mutated channel must be restored"
        );
        assert_eq!(
            outcome.skipped_no_op, 1,
            "the unchanged channel must be counted as a no-op skip"
        );

        let all_writes = fake.written_overwrites();
        let restore_writes = &all_writes[writes_before..];
        assert_eq!(restore_writes.len(), 1, "exactly one restore write");
        assert!(
            restore_writes
                .iter()
                .any(|(id, _)| *id == DiscordChannelId(1)),
            "restore must rewrite ch-1"
        );
    }

    /// Circuit-breaker test via restore: the snapshot references channel ids that no
    /// longer exist in the fake, so every write attempt returns Err. Once the run
    /// accumulates CIRCUIT_BREAKER consecutive failures it aborts.
    #[tokio::test]
    async fn restore_circuit_breaks_after_threshold_consecutive_failures() {
        // The fake has NO channels; the snapshot references CIRCUIT_BREAKER + 1 channels.
        // Every set_channel_overwrites call will return Err (unknown id), so consecutive
        // failures will hit the threshold and circuit-break.
        let fake = FakeDiscord::new().set_everyone_base_view(true);
        let store = InMemoryStore::new(crate::store::Index::default_for_test());
        let channels = Channels::new(&fake, &store);

        // Build a snapshot with more channels than the CIRCUIT_BREAKER threshold.
        let stale_channels: Vec<super::super::snapshot::SavedChannel> = (1..=(CIRCUIT_BREAKER + 1)
            as u64)
            .map(saved_channel_with_ow)
            .collect();
        let snap = ChannelSnapshot {
            format_version: super::super::snapshot::SNAPSHOT_FORMAT_VERSION,
            guild_id: domain::DiscordGuildId(0),
            saved_at: chrono::Utc::now(),
            channels: stale_channels,
        };

        let result = channels.restore(&snap, &NoProgress).await;
        assert!(
            matches!(result, Err(ChannelsError::CircuitBreaker { .. })),
            "expected CircuitBreaker error, got: {result:?}"
        );
    }
}
