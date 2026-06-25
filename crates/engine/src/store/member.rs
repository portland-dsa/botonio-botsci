//! The member roster: the flat [`MemberRecord`], the dedup rule both stores share, the
//! in-memory [`Index`], the roster read/write/repair traits, and the Solidarity Tech sweep.
//!
//! [`MemberRecord`] is deliberately flat and built from persistence-friendly primitives so a
//! Postgres-backed store maps it to one table with no nesting. The [`InMemoryStore`] impls of
//! [`MemberStore`]/[`RosterWrite`]/[`IdentityWrite`] live here too, reaching the store's
//! private fields and helpers from the hub.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use async_trait::async_trait;
use chrono::NaiveDate;

use domain::{MembershipStatus, MigsStatus};

use crate::backends::solidarity_tech::{
    DuesStatus, MembershipType, SolidarityTechClient, SolidarityTechMember,
};
use crate::paging::drain_pages;
use crate::seam::NoProgress;
use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};

use super::InMemoryStore;

/// A member projected to the flat shape the card and the future cache share.
/// Every field is a persistence-friendly primitive (`String`,
/// `Option<NaiveDate>`, small text-mapped enums) so a future implementation maps it to one
/// Postgres-backed table with no nesting.
///
/// `PartialEq`/`Eq` let two records be compared whole - the basis of the
/// `PgStore`/`InMemoryStore` conformance test, which asserts a record survives the
/// cache's encode/store/decode round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// The Solidarity Tech user id - the stable key, and the target a self-heal
    /// writes the Discord identity back to. Every cached record is sourced from a
    /// Solidarity Tech member, so this is always present.
    pub st_user_id: StUserId,
    pub discord_user_id: Option<DiscordUserId>,
    pub discord_handle: Option<DiscordHandle>,
    pub email: Email,
    pub full_name: Option<String>,
    /// Raw "Membership Status"; the [`Role`](domain::Role) is derived (never stored twice).
    pub standing: Option<MigsStatus>,
    pub join_date: Option<NaiveDate>,
    /// Dues-expiry date (`xdate`).
    pub expires: Option<NaiveDate>,
    pub membership_type: Option<MembershipType>,
    pub monthly_dues: Option<DuesStatus>,
    pub yearly_dues: Option<DuesStatus>,
}

impl MemberRecord {
    /// The computed membership status for this record. An absent standing is
    /// [`Malformed`](MembershipStatus::Malformed) - a matched record we cannot decide
    /// a role from - distinct from the live good-standing/lapsed values.
    pub fn membership(&self) -> MembershipStatus {
        self.standing
            .map(MembershipStatus::from)
            .unwrap_or(MembershipStatus::Malformed)
    }
}

impl From<SolidarityTechMember> for MemberRecord {
    fn from(m: SolidarityTechMember) -> Self {
        Self {
            st_user_id: m.id,
            discord_user_id: m.discord_user_id,
            discord_handle: m.discord_handle,
            email: m.email,
            full_name: match (m.first_name, m.last_name) {
                (Some(f), Some(l)) => Some(format!("{f} {l}")),
                (Some(f), None) => Some(f),
                (None, Some(l)) => Some(l),
                (None, None) => None,
            },
            standing: m.membership_standing,
            join_date: m.join_date,
            expires: m.xdate,
            membership_type: m.membership_type,
            monthly_dues: m.monthly_dues,
            yearly_dues: m.yearly_dues,
        }
    }
}

/// Deduplicate projected records the way both stores must agree on: first-wins on the
/// Solidarity Tech id, then first-wins on the Discord id (a later record claiming an id an
/// earlier one already holds is dropped, keeping the id lookups unambiguous). Records with
/// no Discord id are kept - they are exactly who a verify backfill repairs - and are found
/// afterwards by handle. The kept records are returned in input order.
///
/// This is the single definition of the dedup rule. Both the in-memory [`Index`] and the
/// Postgres store run their inputs through it, so the two stores can never silently diverge
/// on which of a pair of colliding records survives.
pub fn dedup_records(records: Vec<MemberRecord>) -> Vec<MemberRecord> {
    let mut seen_st = HashSet::new();
    let mut seen_id = HashSet::new();
    let mut kept = Vec::with_capacity(records.len());
    for rec in records {
        if !seen_st.insert(rec.st_user_id.0.clone()) {
            continue; // the same Solidarity Tech member was already kept
        }
        // A later record claiming an already-taken Discord id is dropped (first-wins).
        if let Some(id) = rec.discord_user_id
            && !seen_id.insert(id.0)
        {
            continue;
        }
        kept.push(rec);
    }
    kept
}

/// An immutable lookup index. Keyed by Discord id for the card's id-only read, and
/// also by handle so a member known to Solidarity Tech by handle but not yet linked
/// to a Discord id is still found - the population verification repairs. The card
/// still reads `by_id` only (see [`crate::card::resolve`]); the handle map exists for
/// the verify path.
#[derive(Default)]
pub struct Index {
    by_id: HashMap<u64, MemberRecord>,
    by_handle: HashMap<String, MemberRecord>,
}

impl Index {
    /// Build from a Solidarity Tech sweep.
    pub fn build(st: Vec<SolidarityTechMember>) -> Self {
        Self::from_records(st.into_iter().map(MemberRecord::from).collect())
    }

    /// Build from already-projected [`MemberRecord`]s (the shape the cache stores).
    ///
    /// Runs the input through [`dedup_records`] - the rule the Postgres store shares - so
    /// a record whose Solidarity Tech id or Discord id was already claimed is dropped from
    /// both maps, keeping the two stores equivalent.
    pub fn from_records(records: Vec<MemberRecord>) -> Self {
        let mut idx = Index::default();
        for rec in dedup_records(records) {
            idx.insert(rec);
        }
        idx
    }

    /// Insert a record the caller has already deduplicated, into whichever maps its
    /// identity supports.
    fn insert(&mut self, rec: MemberRecord) {
        if let Some(handle) = rec.discord_handle.clone() {
            self.by_handle
                .entry(handle.0)
                .or_insert_with(|| rec.clone());
        }
        if let Some(id) = rec.discord_user_id {
            self.by_id.entry(id.0).or_insert(rec);
        }
    }

    #[cfg(test)]
    pub(crate) fn default_for_test() -> Self {
        Index::default()
    }

    /// Look up by Discord user id.
    pub fn by_id(&self, id: DiscordUserId) -> Option<MemberRecord> {
        self.by_id.get(&id.0).cloned()
    }

    /// Look up by Discord handle. Used only by the verify path; the card resolves by id.
    pub fn by_handle(&self, handle: &DiscordHandle) -> Option<MemberRecord> {
        self.by_handle.get(&handle.0).cloned()
    }

    /// Whether the index holds no members (every input record was a duplicate or
    /// lacked a Discord id and a handle). Used to refuse replacing a populated roster
    /// with an empty sweep.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.by_handle.is_empty()
    }
}

/// Reverse lookup from a Discord id to a [`MemberRecord`]. Async and fallible from
/// the start so a later Postgres-backed implementation drops in without a signature
/// change; the in-memory impl's [`Error`](MemberStore::Error) is [`Infallible`].
#[async_trait]
pub trait MemberStore: Send + Sync {
    /// How a read can fail. [`Infallible`] for the in-memory store; a database
    /// error for a Postgres-backed one.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Look up a member by their Discord user snowflake.
    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Self::Error>;

    /// Look up a member by their current Discord handle. The repair fallback when an
    /// id lookup misses; the card never uses it.
    ///
    /// Handles are not unique in the cache - a handle can be recycled between roster
    /// sweeps - so when several records share one, which is returned is unspecified and may
    /// differ between implementations. That is acceptable because the immutable id is
    /// authoritative: verify reads this only after [`by_discord_id`](Self::by_discord_id)
    /// misses, and the conflict guard still refuses to re-link a record already bound to a
    /// different id.
    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Self::Error>;

    /// Every record currently in the roster, in unspecified order. The reminder planner
    /// iterates the whole cached roster once per sweep to build the plan. The
    /// `InMemoryStore` impl clones its index's records; the `PgStore` impl runs
    /// `SELECT * FROM member_cache`.
    async fn all_records(&self) -> Result<Vec<MemberRecord>, Self::Error>;
}

/// Replace the whole cached roster in one shot - the write half of a refresh sweep.
/// Fallible from the start for the same reason as [`MemberStore`]: the in-memory
/// impl's [`Error`](RosterWrite::Error) is [`Infallible`], a Postgres-backed one's
/// is a database error.
#[async_trait]
pub trait RosterWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Atomically replace the stored roster with `records`. An empty roster is a
    /// no-op that preserves the current one: a sweep resolving to zero members is
    /// treated as an upstream glitch, never a real membership of zero.
    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Self::Error>;
}

/// Repair one member's stored Discord identity in place, keyed by their Solidarity
/// Tech id. The write-through half of verification's self-heal: distinct from
/// [`RosterWrite`], which only ever replaces the whole roster. Fallible from the
/// start for the same reason as the other store traits.
#[async_trait]
pub trait IdentityWrite: Send + Sync {
    /// How a write can fail. [`Infallible`] for the in-memory store.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Set the Discord user id and handle on the member with `st_user_id`. A member
    /// the store does not hold is a silent no-op (nothing to repair).
    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Self::Error>;

    /// Clear the Discord identity (id and handle) from whichever cached row currently
    /// holds `discord_id`, returning the member to an unlinked state so a later verify
    /// misses by both id and handle. A `discord_id` no row holds is a silent no-op.
    async fn unlink_by_discord_id(&self, discord_id: DiscordUserId) -> Result<(), Self::Error>;
}

/// Sweep the Solidarity Tech user list (pre-filtered to Discord-linked members)
/// into the flat [`MemberRecord`]s the cache stores. The store-agnostic half of the
/// refresh: the caller hands the result to [`RosterWrite::replace_roster`].
pub async fn sweep_roster(
    st: &impl SolidarityTechClient,
    list_id: &str,
) -> crate::Result<Vec<MemberRecord>> {
    let st_members = drain_pages(
        &NoProgress,
        "solidarity tech discord list",
        |cursor| async move { st.members_in_list_page(list_id, cursor.as_deref()).await },
    )
    .await?;
    tracing::info!(
        members = st_members.len(),
        list_id,
        "fetched discord-list members from solidarity tech"
    );
    Ok(st_members.into_iter().map(MemberRecord::from).collect())
}

/// Roster-specific [`InMemoryStore`] internals that touch [`Index`]'s private maps, so they
/// must live in this module rather than the hub.
impl InMemoryStore {
    /// Rebuild the index from the current snapshot with `mutate` applied to every record,
    /// then swap it in - the copy-on-write the roster refresh uses, shared by the identity
    /// link and unlink. A record mapped by both id and handle is collected from both maps;
    /// the duplicates collapse in [`Index::from_records`], the single dedup point, so no
    /// pre-dedup is needed.
    fn rebuild_records(&self, mutate: impl FnMut(&mut MemberRecord)) {
        let mut records: Vec<MemberRecord> = {
            let snap = self.snapshot();
            snap.by_id
                .values()
                .chain(snap.by_handle.values())
                .cloned()
                .collect()
        };
        records.iter_mut().for_each(mutate);
        self.swap(Index::from_records(records));
    }
}

#[async_trait]
impl MemberStore for InMemoryStore {
    type Error = Infallible;

    async fn by_discord_id(&self, id: DiscordUserId) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_id(id))
    }

    async fn by_handle(&self, handle: &DiscordHandle) -> Result<Option<MemberRecord>, Infallible> {
        Ok(self.snapshot().by_handle(handle))
    }

    async fn all_records(&self) -> Result<Vec<MemberRecord>, Infallible> {
        let snap = self.snapshot();
        // Collect from both maps then dedup: a record with both a Discord id and a handle
        // appears in both maps and collapses to one entry via `Index::from_records`'s rule.
        let records: Vec<MemberRecord> = snap
            .by_id
            .values()
            .chain(snap.by_handle.values())
            .cloned()
            .collect();
        Ok(dedup_records(records))
    }
}

#[async_trait]
impl RosterWrite for InMemoryStore {
    type Error = Infallible;

    async fn replace_roster(&self, records: Vec<MemberRecord>) -> Result<(), Infallible> {
        let index = Index::from_records(records);
        // Mirror PgStore: never overwrite a populated roster with an empty sweep.
        if index.is_empty() {
            return Ok(());
        }
        self.swap(index);
        Ok(())
    }
}

#[async_trait]
impl IdentityWrite for InMemoryStore {
    type Error = Infallible;

    async fn link_identity(
        &self,
        st_user_id: &StUserId,
        discord_id: DiscordUserId,
        handle: &DiscordHandle,
    ) -> Result<(), Infallible> {
        // Update the one record keyed by `st_user_id` with the discovered identity.
        self.rebuild_records(|rec| {
            if rec.st_user_id == *st_user_id {
                rec.discord_user_id = Some(discord_id);
                rec.discord_handle = Some(handle.clone());
            }
        });
        Ok(())
    }

    /// Clear the Discord identity from the record holding `discord_id`. With both identity
    /// columns cleared the record falls out of both index maps, so a later lookup misses by
    /// id and handle alike.
    async fn unlink_by_discord_id(&self, discord_id: DiscordUserId) -> Result<(), Infallible> {
        self.rebuild_records(|rec| {
            if rec.discord_user_id == Some(discord_id) {
                rec.discord_user_id = None;
                rec.discord_handle = None;
            }
        });
        Ok(())
    }
}
