//! The read-only membership-card use case: resolve a present Discord member to
//! their [`MemberRecord`].
//!
//! Presence is encoded by the input type. [`resolve`] takes a [`PresentMember`] -
//! a token the front-end can build only from a member it actually holds - and does
//! no Discord I/O. A member who has left the server has opted out of being looked
//! up, and that is enforced structurally: you cannot construct a [`PresentMember`]
//! for someone you do not hold.

use crate::store::{MemberRecord, MemberStore};
use crate::util::DiscordUserId;

/// The identity key a card lookup needs, carried as proof that the subject is a
/// member the caller holds. The Discord **user id** - an immutable snowflake - is
/// the only key: a card is resolved by id alone, *never* by handle, because Discord
/// usernames are mutable and recyclable, so matching PII on a handle would let a
/// member who claimed a freed-up username inherit the prior holder's record. Built
/// from an interaction's member (today) or a `fetch_member` result - the
/// id always comes from a member actually present, not an arbitrary input.
pub struct PresentMember {
    pub id: DiscordUserId,
}

/// Why a card could not be produced. Generic over the store's own error so a
/// fallible (e.g. Postgres-backed) [`MemberStore`] surfaces its read failure as
/// [`CardError::Store`]; the in-memory store's `E` is [`std::convert::Infallible`],
/// so that arm is unreachable there.
#[derive(Debug, thiserror::Error)]
pub enum CardError<E: std::error::Error + Send + Sync + 'static> {
    /// No Solidarity Tech record matched the subject.
    #[error("no membership record found")]
    NoRecord,
    /// The store itself failed to answer the lookup.
    #[error(transparent)]
    Store(E),
}

/// Resolve a present member to their record by Discord **user id** only. Matching is
/// never done by handle: usernames are mutable and recyclable, so a handle match
/// could hand one member another member's PII (see [`PresentMember`]). A miss -
/// including a member whose backend record was never id-linked - is
/// [`CardError::NoRecord`]; a store read failure is [`CardError::Store`].
pub async fn resolve<S: MemberStore>(
    store: &S,
    subject: &PresentMember,
) -> Result<MemberRecord, CardError<S::Error>> {
    store
        .by_discord_id(subject.id)
        .await
        .map_err(CardError::Store)?
        .ok_or(CardError::NoRecord)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::solidarity_tech::SolidarityTechMember;
    use crate::store::{InMemoryStore, Index};
    use crate::util::{DiscordHandle, DiscordUserId, Email, StUserId};

    fn member(handle: &str, id: u64) -> SolidarityTechMember {
        SolidarityTechMember {
            id: StUserId(id.to_string()),
            email: Email("m@b.test".into()),
            discord_handle: Some(DiscordHandle(handle.into())),
            discord_user_id: Some(DiscordUserId(id)),
            ..Default::default()
        }
    }

    fn subject(id: u64) -> PresentMember {
        PresentMember {
            id: DiscordUserId(id),
        }
    }

    #[tokio::test]
    async fn resolves_by_id() {
        let store = InMemoryStore::new(Index::build(vec![member("zoop", 42)]));
        let rec = resolve(&store, &subject(42)).await.unwrap();
        assert_eq!(rec.discord_user_id, Some(DiscordUserId(42)));
    }

    #[tokio::test]
    async fn id_miss_is_no_record() {
        // A record exists under handle "zoop" (and id 42). A subject whose id is not in
        // the index gets NoRecord: there is no handle fallback, so someone who took the
        // username "zoop" but isn't the indexed member cannot reach this record's PII.
        let store = InMemoryStore::new(Index::build(vec![member("zoop", 42)]));
        assert!(matches!(
            resolve(&store, &subject(999)).await,
            Err(CardError::NoRecord)
        ));
    }

    #[tokio::test]
    async fn missing_record_is_no_record() {
        let store = InMemoryStore::new(Index::default_for_test());
        assert!(matches!(
            resolve(&store, &subject(1)).await,
            Err(CardError::NoRecord)
        ));
    }
}
