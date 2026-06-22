//! The read-only membership-card use case: resolve a present Discord member to
//! their [`MemberRecord`].
//!
//! Presence is encoded by the input type. [`resolve`] takes a [`PresentMember`] -
//! a token the front-end can build only from a member it actually holds - and does
//! no Discord I/O. A member who has left the server has opted out of being looked
//! up, and that is enforced structurally: you cannot construct a [`PresentMember`]
//! for someone you do not hold.

use crate::store::{MemberRecord, MemberStore, OverrideLog, OverrideRecord};
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

/// What a card lookup resolves to, in precedence order: a Solidarity Tech record if
/// one exists, otherwise a standing manual-override approval, otherwise nothing. The
/// override never becomes a synthetic [`MemberRecord`] - it is a distinct view, so the
/// record type stays a faithful Solidarity Tech projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardView {
    /// A Solidarity Tech-backed record.
    Member(MemberRecord),
    /// No Solidarity Tech record, but a moderator hand-approved this member.
    Override(OverrideRecord),
    /// Neither - the subject is unknown to us.
    Unknown,
}

/// Why [`resolve_view`] could not answer: one of its two backing reads failed. Generic
/// over each store's own error so a Postgres read failure and an override read failure
/// stay distinct and typed.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError<SE, OE>
where
    SE: std::error::Error + Send + Sync + 'static,
    OE: std::error::Error + Send + Sync + 'static,
{
    #[error(transparent)]
    Store(SE),
    #[error(transparent)]
    Override(OE),
}

/// Resolve a present member to the card view to show. A Solidarity Tech record wins;
/// with none, a manual-override stamp produces [`CardView::Override`]; with neither,
/// [`CardView::Unknown`]. Reuses [`resolve`] for the record read, so the id-only rule
/// (never match on handle) holds here too.
pub async fn resolve_view<S, O>(
    store: &S,
    overrides: &O,
    subject: &PresentMember,
) -> Result<CardView, ResolveError<S::Error, O::Error>>
where
    S: MemberStore,
    O: OverrideLog,
{
    match resolve(store, subject).await {
        Ok(rec) => Ok(CardView::Member(rec)),
        Err(CardError::NoRecord) => match overrides
            .get_override(subject.id)
            .await
            .map_err(ResolveError::Override)?
        {
            Some(stamp) => Ok(CardView::Override(stamp)),
            None => Ok(CardView::Unknown),
        },
        Err(CardError::Store(e)) => Err(ResolveError::Store(e)),
    }
}

/// The card-read facade: resolve a present member to the [`CardView`] to show, through one
/// trait method instead of two raw store handles. A combiner over the two backing reads -
/// [`MemberStore`] for the Solidarity Tech record, [`OverrideLog`] for the hand-approval stamp -
/// with [`card_view`](CardRead::card_view) a defaulted method, mirroring [`crate::verify::Heal`].
/// The blanket impl makes the production `PgStore` and the test `InMemoryStore` both `CardRead`
/// with no glue, so a caller reads cards through `store.card_view(..)` rather than wiring
/// [`resolve_view`] up by hand.
#[async_trait::async_trait]
pub trait CardRead: MemberStore + OverrideLog {
    /// Resolve `subject` to its card view by Discord user id only (record > override > unknown).
    /// The default delegates to [`resolve_view`], so the id-only rule (never match on handle)
    /// holds for every implementor. Bounded `Self: Sized` because the delegation hands `self` to
    /// the generic [`resolve_view`]; every implementor is a concrete store, so this never bites.
    async fn card_view(
        &self,
        subject: &PresentMember,
    ) -> Result<CardView, ResolveError<<Self as MemberStore>::Error, <Self as OverrideLog>::Error>>
    where
        Self: Sized,
    {
        resolve_view(self, self, subject).await
    }
}

// Empty blanket impl: every store that can read records and overrides gets `card_view` from the
// trait default. No method bodies, so no `#[async_trait]`.
impl<T: MemberStore + OverrideLog> CardRead for T {}

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

    #[tokio::test]
    async fn view_prefers_member_record_over_override() {
        let store = InMemoryStore::new(Index::build(vec![member("zoop", 42)]));
        store
            .stamp_override(DiscordUserId(42), DiscordUserId(1), None)
            .await
            .unwrap();
        let view = resolve_view(&store, &store, &subject(42)).await.unwrap();
        assert!(matches!(view, CardView::Member(_)));
    }

    #[tokio::test]
    async fn view_is_override_when_no_record() {
        let store = InMemoryStore::new(Index::default_for_test());
        store
            .stamp_override(DiscordUserId(7), DiscordUserId(1), None)
            .await
            .unwrap();
        let view = resolve_view(&store, &store, &subject(7)).await.unwrap();
        match view {
            CardView::Override(s) => assert_eq!(s.approved_by, DiscordUserId(1)),
            other => panic!("expected override, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn view_is_unknown_with_neither() {
        let store = InMemoryStore::new(Index::default_for_test());
        let view = resolve_view(&store, &store, &subject(7)).await.unwrap();
        assert!(matches!(view, CardView::Unknown));
    }

    #[tokio::test]
    async fn card_view_trait_method_matches_resolve_view() {
        // The CardRead blanket impl gives any store `card_view`, delegating to resolve_view:
        // a present member resolves to their record, a stranger to Unknown.
        let store = InMemoryStore::new(Index::build(vec![member("zoop", 42)]));
        assert!(matches!(
            store.card_view(&subject(42)).await.unwrap(),
            CardView::Member(_)
        ));
        assert!(matches!(
            store.card_view(&subject(999)).await.unwrap(),
            CardView::Unknown
        ));
    }
}
