//! Hand-written, state-based fake of [`SolidarityTechClient`] for offline tests.
//!
//! [`FakeSolidarityTech`] holds member fixtures and applies the Discord-identity
//! write-backs to them, so a test reads a member back with
//! [`get`](FakeSolidarityTech::get) to confirm a self-heal landed, and reads
//! [`writes`](FakeSolidarityTech::writes) to confirm none did. Failure injection
//! is intentionally omitted: no current test exercises a Solidarity Tech error
//! path; add it (with the matching [`SolidarityTechError`] variant) when one does.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::MemberPage;
use crate::util::{DiscordHandle, DiscordUserId, Email, Phone};

use super::client::{SolidarityTechClient, StClearFlags};
use super::error::SolidarityTechError;
use super::member::{CustomUserProperty, SolidarityTechMember};

/// An in-memory [`SolidarityTechClient`] for offline tests. See the module docs.
#[derive(Debug, Default)]
pub struct FakeSolidarityTech {
    members: Mutex<Vec<SolidarityTechMember>>,
    writes: Mutex<usize>,
}

impl FakeSolidarityTech {
    /// An empty fake: no members, no write-backs recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the member fixtures reads resolve against.
    pub fn with_members(self, members: Vec<SolidarityTechMember>) -> Self {
        *self.members.lock().unwrap() = members;
        self
    }

    /// The stored member with Solidarity Tech id `st_user_id`, to assert a
    /// write-back landed (its `discord_user_id` / `discord_handle` columns).
    pub fn get(&self, st_user_id: &str) -> Option<SolidarityTechMember> {
        self.members
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.id.0 == st_user_id)
            .cloned()
    }

    /// How many identity write-back CALLS ran. A call is counted whether or not
    /// it matched a seeded member, mirroring a real backend that issues the
    /// request regardless of local fixtures; assert == 0 to prove the
    /// fail-closed path issued none.
    pub fn writes(&self) -> usize {
        *self.writes.lock().unwrap()
    }

    fn mutate(&self, member_id: &str, f: impl FnOnce(&mut SolidarityTechMember)) {
        let mut members = self.members.lock().unwrap();
        if let Some(m) = members.iter_mut().find(|m| m.id.0 == member_id) {
            f(m);
        }
        // Count per call issued, not per member found: a real backend PUTs
        // regardless of whether our fixtures happen to hold a matching member.
        *self.writes.lock().unwrap() += 1;
    }
}

#[async_trait]
impl SolidarityTechClient for FakeSolidarityTech {
    async fn find_members(
        &self,
        email: Option<&Email>,
        phone: Option<&Phone>,
    ) -> Result<Vec<SolidarityTechMember>, SolidarityTechError> {
        let members = self.members.lock().unwrap();
        let hits = members
            .iter()
            .filter(|m| email.is_none_or(|e| &m.email == e))
            .filter(|m| phone.is_none_or(|p| m.phone.as_ref() == Some(p)))
            .cloned()
            .collect();
        Ok(hits)
    }

    async fn members_page(
        &self,
        _cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError> {
        let members = self.members.lock().unwrap().clone();
        let scanned = members.len() as u64;
        Ok(MemberPage {
            members,
            scanned,
            total: Some(scanned),
            next: None,
        })
    }

    async fn members_in_list_page(
        &self,
        _list_id: &str,
        _cursor: Option<&str>,
    ) -> Result<MemberPage<SolidarityTechMember>, SolidarityTechError> {
        let members = self.members.lock().unwrap().clone();
        let scanned = members.len() as u64;
        Ok(MemberPage {
            members,
            scanned,
            total: Some(scanned),
            next: None,
        })
    }

    async fn set_discord_handle(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
    ) -> Result<(), SolidarityTechError> {
        self.mutate(member_id, |m| m.discord_handle = Some(handle.clone()));
        Ok(())
    }

    async fn set_alternate_email(
        &self,
        _member_id: &str,
        _alternate_email: &Email,
    ) -> Result<(), SolidarityTechError> {
        // No test exercises the alternate-email write; record it as a write so the
        // fail-closed count stays honest, but store nothing.
        *self.writes.lock().unwrap() += 1;
        Ok(())
    }

    async fn set_discord_identity(
        &self,
        member_id: &str,
        handle: &DiscordHandle,
        id: DiscordUserId,
    ) -> Result<(), SolidarityTechError> {
        self.mutate(member_id, |m| {
            m.discord_handle = Some(handle.clone());
            m.discord_user_id = Some(id);
        });
        Ok(())
    }

    async fn clear_discord_identity(
        &self,
        member_id: &str,
        flags: StClearFlags,
    ) -> Result<(), SolidarityTechError> {
        self.mutate(member_id, |m| {
            if flags.handle {
                m.discord_handle = None;
            }
            if flags.user_id {
                m.discord_user_id = None;
            }
        });
        Ok(())
    }

    async fn list_custom_user_properties(
        &self,
    ) -> Result<Vec<CustomUserProperty>, SolidarityTechError> {
        Ok(Vec::new())
    }
}
