//! Hand-written, state-based fake of [`DiscordClient`] for offline tests.
//!
//! [`FakeDiscord`] actually applies role writes to in-memory state, so a test
//! asserts the member's resulting roles ([`roles_of`](FakeDiscord::roles_of))
//! rather than which calls were made. Seed held roles with
//! [`with_roles`](FakeDiscord::with_roles) and the sweep roster with
//! [`with_roster`](FakeDiscord::with_roster); force an error path with
//! [`failing`](FakeDiscord::failing).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::MemberPage;
use crate::util::{DiscordGuildId, DiscordUserId};

use super::client::DiscordClient;
use super::error::DiscordError;
use super::roles::{DiscordRosterMember, ManagedRole, MemberRoles, Role};

/// A [`DiscordClient`] operation a [`FakeDiscord`] can be told to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscordOp {
    MemberRoles,
    SetRole,
    RemoveRoles,
    MembersPage,
    AssignOverrideMarker,
    RemoveOverrideMarker,
}

/// An in-memory [`DiscordClient`] for offline tests. See the module docs.
#[derive(Debug)]
pub struct FakeDiscord {
    guild: DiscordGuildId,
    managed: Vec<ManagedRole>,
    roles: Mutex<HashMap<DiscordUserId, Vec<Role>>>,
    markers: Mutex<HashSet<DiscordUserId>>,
    roster: Mutex<Vec<DiscordRosterMember>>,
    fail: Mutex<HashSet<DiscordOp>>,
}

impl FakeDiscord {
    /// An empty fake: no members, no roster, nothing failing.
    pub fn new() -> Self {
        Self {
            guild: DiscordGuildId(0),
            managed: Vec::new(),
            roles: Mutex::new(HashMap::new()),
            markers: Mutex::new(HashSet::new()),
            roster: Mutex::new(Vec::new()),
            fail: Mutex::new(HashSet::new()),
        }
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

    /// The managed status roles `member` currently holds.
    pub fn roles_of(&self, member: DiscordUserId) -> Vec<Role> {
        self.roles
            .lock()
            .unwrap()
            .get(&member)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `member` currently holds the Manual Override marker.
    pub fn has_marker(&self, member: DiscordUserId) -> bool {
        self.markers.lock().unwrap().contains(&member)
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

    async fn assign_override_marker(&self, user: DiscordUserId) -> Result<(), DiscordError> {
        self.guard(DiscordOp::AssignOverrideMarker)?;
        self.markers.lock().unwrap().insert(user);
        Ok(())
    }

    async fn remove_override_marker(&self, user: DiscordUserId) -> Result<(), DiscordError> {
        self.guard(DiscordOp::RemoveOverrideMarker)?;
        self.markers.lock().unwrap().remove(&user);
        Ok(())
    }
}
