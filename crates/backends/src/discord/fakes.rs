//! Hand-written, state-based fake of [`DiscordClient`] for offline tests.
//!
//! [`FakeDiscord`] actually applies role writes to in-memory state, so a test
//! asserts the member's resulting roles ([`roles_of`](FakeDiscord::roles_of))
//! rather than which calls were made. Seed held roles with
//! [`with_roles`](FakeDiscord::with_roles) and the sweep roster with
//! [`with_roster`](FakeDiscord::with_roster); force an error path with
//! [`failing`](FakeDiscord::failing). Seed channels with
//! [`with_channels`](FakeDiscord::with_channels) and read recorded overwrite
//! writes with [`written_overwrites`](FakeDiscord::written_overwrites).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::MemberPage;
use crate::util::{DiscordChannelId, DiscordGuildId, DiscordUserId};

use super::channels::{DiscordChannel, GuildChannels, PermOverwrite};
use super::client::DiscordClient;
use super::error::DiscordError;
use super::roles::{DiscordRosterMember, ManagedRole, MarkerRole, MemberRoles, Role};

/// A [`DiscordClient`] operation a [`FakeDiscord`] can be told to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscordOp {
    MemberRoles,
    SetRole,
    RemoveRoles,
    MembersPage,
    AssignMarkerRole,
    RemoveMarkerRole,
}

/// An in-memory [`DiscordClient`] for offline tests. See the module docs.
#[derive(Debug)]
pub struct FakeDiscord {
    guild: DiscordGuildId,
    managed: Vec<ManagedRole>,
    roles: Mutex<HashMap<DiscordUserId, Vec<Role>>>,
    markers: Mutex<HashSet<(DiscordUserId, MarkerRole)>>,
    roster: Mutex<Vec<DiscordRosterMember>>,
    fail: Mutex<HashSet<DiscordOp>>,
    /// Seeded channel list; [`set_channel_overwrites`] reflects writes back in.
    channels: Mutex<Vec<DiscordChannel>>,
    /// Whether the `@everyone` role grants `VIEW_CHANNEL` at the base level.
    everyone_base_view: Mutex<bool>,
    /// Log of every [`set_channel_overwrites`] call, in call order.
    overwrite_writes: Mutex<Vec<(DiscordChannelId, Vec<PermOverwrite>)>>,
    /// Per-member status for [`member_status_role`]: present = guild member with
    /// that role, absent = not in the guild (`None` return).
    member_status: Mutex<HashMap<DiscordUserId, Role>>,
}

impl FakeDiscord {
    /// An empty fake: no members, no roster, no channels, nothing failing.
    pub fn new() -> Self {
        Self {
            guild: DiscordGuildId(0),
            managed: Vec::new(),
            roles: Mutex::new(HashMap::new()),
            markers: Mutex::new(HashSet::new()),
            roster: Mutex::new(Vec::new()),
            fail: Mutex::new(HashSet::new()),
            channels: Mutex::new(Vec::new()),
            everyone_base_view: Mutex::new(false),
            overwrite_writes: Mutex::new(Vec::new()),
            member_status: Mutex::new(HashMap::new()),
        }
    }

    /// Seed `user`'s guild membership status for [`member_status_role`].
    ///
    /// A seeded user is a guild member holding `role`; an unseeded user returns
    /// `None` (not in the guild).
    pub fn seed_status(&self, user: DiscordUserId, role: Role) {
        self.member_status.lock().unwrap().insert(user, role);
    }

    /// Seed the managed status roles `member` currently holds.
    pub fn with_roles(self, member: DiscordUserId, roles: Vec<Role>) -> Self {
        self.roles.lock().unwrap().insert(member, roles);
        self
    }

    /// Seed the roster [`members_page`](DiscordClient::members_page) yields.
    pub fn with_roster(self, members: Vec<DiscordRosterMember>) -> Self {
        *self.roster.lock().unwrap() = members;
        self
    }

    /// Make `op` return a [`DiscordError`] when called.
    pub fn failing(self, op: DiscordOp) -> Self {
        self.fail.lock().unwrap().insert(op);
        self
    }

    /// Seed the channel list [`read_channels`](super::client::DiscordClient::read_channels) yields.
    pub fn with_channels(self, channels: Vec<DiscordChannel>) -> Self {
        *self.channels.lock().unwrap() = channels;
        self
    }

    /// Set whether `@everyone` grants `VIEW_CHANNEL` at the base level.
    pub fn set_everyone_base_view(self, value: bool) -> Self {
        *self.everyone_base_view.lock().unwrap() = value;
        self
    }

    /// Every [`set_channel_overwrites`](super::client::DiscordClient::set_channel_overwrites)
    /// call recorded so far, in call order.
    pub fn written_overwrites(&self) -> Vec<(DiscordChannelId, Vec<PermOverwrite>)> {
        self.overwrite_writes.lock().unwrap().clone()
    }

    /// The managed status roles `member` currently holds.
    pub fn roles_of(&self, member: DiscordUserId) -> Vec<Role> {
        self.roles
            .lock()
            .unwrap()
            .get(&member)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `member` currently holds `marker`.
    pub fn has_marker(&self, member: DiscordUserId, marker: MarkerRole) -> bool {
        self.markers.lock().unwrap().contains(&(member, marker))
    }

    fn guard(&self, op: DiscordOp) -> Result<(), DiscordError> {
        if self.fail.lock().unwrap().contains(&op) {
            Err(DiscordError::MissingEnv("simulated discord failure"))
        } else {
            Ok(())
        }
    }
}

impl Default for FakeDiscord {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DiscordClient for FakeDiscord {
    fn guild_id(&self) -> DiscordGuildId {
        self.guild
    }

    fn managed_roles(&self) -> Vec<ManagedRole> {
        self.managed.clone()
    }

    async fn set_role(
        &self,
        user: DiscordUserId,
        current: Option<Role>,
        target: Role,
    ) -> Result<(), DiscordError> {
        self.guard(DiscordOp::SetRole)?;
        let mut roles = self.roles.lock().unwrap();
        let held = roles.entry(user).or_default();
        if let Some(current) = current {
            held.retain(|&r| r != current);
        }
        if !held.contains(&target) {
            held.push(target);
        }
        Ok(())
    }

    async fn remove_roles(
        &self,
        user: DiscordUserId,
        roles_to_remove: &[Role],
    ) -> Result<(), DiscordError> {
        self.guard(DiscordOp::RemoveRoles)?;
        let mut roles = self.roles.lock().unwrap();
        roles
            .entry(user)
            .or_default()
            .retain(|r| !roles_to_remove.contains(r));
        Ok(())
    }

    async fn member_roles(&self, user: DiscordUserId) -> Result<MemberRoles, DiscordError> {
        self.guard(DiscordOp::MemberRoles)?;
        Ok(MemberRoles {
            held: self.roles_of(user),
            ..Default::default()
        })
    }

    async fn members_page(
        &self,
        _cursor: Option<&str>,
    ) -> Result<MemberPage<DiscordRosterMember>, DiscordError> {
        self.guard(DiscordOp::MembersPage)?;
        let members = self.roster.lock().unwrap().clone();
        let scanned = members.len() as u64;
        Ok(MemberPage {
            members,
            scanned,
            total: Some(scanned),
            next: None,
        })
    }

    async fn assign_marker_role(
        &self,
        user: DiscordUserId,
        marker: MarkerRole,
    ) -> Result<(), DiscordError> {
        self.guard(DiscordOp::AssignMarkerRole)?;
        self.markers.lock().unwrap().insert((user, marker));
        Ok(())
    }

    async fn remove_marker_role(
        &self,
        user: DiscordUserId,
        marker: MarkerRole,
    ) -> Result<(), DiscordError> {
        self.guard(DiscordOp::RemoveMarkerRole)?;
        self.markers.lock().unwrap().remove(&(user, marker));
        Ok(())
    }

    async fn member_status_role(&self, user: DiscordUserId) -> Result<Option<Role>, DiscordError> {
        Ok(self.member_status.lock().unwrap().get(&user).copied())
    }

    async fn read_channels(&self) -> Result<GuildChannels, DiscordError> {
        Ok(GuildChannels {
            guild_id: self.guild,
            everyone_base_view: *self.everyone_base_view.lock().unwrap(),
            channels: self.channels.lock().unwrap().clone(),
        })
    }

    async fn set_channel_overwrites(
        &self,
        id: DiscordChannelId,
        overwrites: &[PermOverwrite],
    ) -> Result<(), DiscordError> {
        let mut channels = self.channels.lock().unwrap();
        // Mirror live Discord: writing to an unknown channel is an error.
        // A failed write is NOT recorded - the caller's circuit-breaker path can
        // exercise this against a stale snapshot without poisoning the write log.
        let Some(c) = channels.iter_mut().find(|c| c.id == id) else {
            return Err(DiscordError::MissingEnv("channel not found (fake)"));
        };
        // Reflect the write back so a read-after-write sees the new state.
        c.overwrites = overwrites.to_vec();
        drop(channels);
        self.overwrite_writes
            .lock()
            .unwrap()
            .push((id, overwrites.to_vec()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::channels::{ChannelKind, OverwriteTarget, Permissions};
    use super::*;
    use domain::DiscordRoleId;

    #[tokio::test]
    async fn marker_roles_are_tracked_independently() {
        let fake = FakeDiscord::new();
        let u = DiscordUserId(1);
        fake.assign_marker_role(u, MarkerRole::DuesExpiring)
            .await
            .unwrap();
        assert!(fake.has_marker(u, MarkerRole::DuesExpiring));
        assert!(!fake.has_marker(u, MarkerRole::ManualOverride));
        fake.remove_marker_role(u, MarkerRole::DuesExpiring)
            .await
            .unwrap();
        assert!(!fake.has_marker(u, MarkerRole::DuesExpiring));
    }

    fn make_channel(id: u64, kind: ChannelKind, parent_id: Option<u64>) -> DiscordChannel {
        DiscordChannel {
            id: DiscordChannelId(id),
            name: format!("channel-{id}"),
            kind,
            parent_id: parent_id.map(DiscordChannelId),
            position: 0,
            overwrites: Vec::new(),
        }
    }

    #[tokio::test]
    async fn fake_set_channel_overwrites_errors_on_unknown_id() {
        let fake =
            FakeDiscord::new().with_channels(vec![make_channel(100, ChannelKind::Text, None)]);

        let ows = vec![PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(1)),
            allow: Permissions::VIEW_CHANNEL,
            deny: Permissions::empty(),
        }];

        // Unknown id returns Err and does not appear in the write log.
        let result = fake
            .set_channel_overwrites(DiscordChannelId(999), &ows)
            .await;
        assert!(result.is_err(), "expected Err for unknown channel id");
        assert!(
            fake.written_overwrites().is_empty(),
            "failed write must not be recorded"
        );
    }

    #[tokio::test]
    async fn fake_reads_seeded_channels_and_records_writes() {
        let fake = FakeDiscord::new()
            .with_channels(vec![
                make_channel(100, ChannelKind::Category, None),
                make_channel(101, ChannelKind::Text, Some(100)),
            ])
            .set_everyone_base_view(true);

        let read = fake.read_channels().await.unwrap();
        assert_eq!(read.channels.len(), 2);
        assert!(read.everyone_base_view);

        let ows = vec![PermOverwrite {
            target: OverwriteTarget::Role(DiscordRoleId(1)),
            allow: Permissions::VIEW_CHANNEL,
            deny: Permissions::empty(),
        }];
        fake.set_channel_overwrites(DiscordChannelId(100), &ows)
            .await
            .unwrap();

        assert_eq!(
            fake.written_overwrites(),
            vec![(DiscordChannelId(100), ows.clone())]
        );
        let after = fake.read_channels().await.unwrap();
        assert_eq!(
            after
                .channels
                .iter()
                .find(|c| c.id == DiscordChannelId(100))
                .unwrap()
                .overwrites,
            ows
        );
    }
}
