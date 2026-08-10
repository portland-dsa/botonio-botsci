//! The moderator verify-and-assign use case: match a Discord member to their
//! Solidarity Tech record, repair the stored identity link, and assign the role their
//! standing earns.
//!
//! [`locate`] reads id-first then handle; [`decide`] is the pure decision over where
//! the record was found, with a guard that never re-links a record already bound to a
//! different account. [`Member`] is the facade handle the verbs hang off: build a
//! [`DataStore`] from the four backends, wrap it with [`Member::new`], and call the
//! verb (`verify`, `resync`, `verify_by_email`, `override_approve`, `forget`).
//! [`MemberError`] is the one concrete error the verbs surface.

mod datastore;
mod decision;
mod facade;
mod member;

pub use datastore::DataStore;
pub use decision::{
    EmailMatchOutcome, HealAction, Located, MatchOutcome, decide, locate, match_by_email,
};
pub use facade::{Heal, MemberError, MemberRead, MemberWrite};
pub use member::{EmailGrant, Member, ResyncOutcome, StripOutcome, Target, VerifyOutcome};
/// The shared "a hold forces Member, else the standing-derived role" decision, reused by the
/// scheduled scan, the bulk preview, and the membership card so none of them can drift from
/// the verify path.
pub use member::{Hold, effective_role, hold_for};
