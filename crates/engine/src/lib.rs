//! Membership-card read layer: resolve a present Discord member to their
//! membership record.
//!
//! This crate sits between the [`backends`] clients and the bot front-end. It
//! owns the reverse-lookup [`store`] - a Discord-id-keyed [`Index`](store::Index)
//! built from the Solidarity Tech roster - and the [`card`] resolver the bot
//! reads through, so the lookup and decision logic stay network-free and
//! unit-testable rather than living in the gateway.
//!
//! It depends only on [`domain`] (the shared vocabulary) and [`backends`] (the
//! clients, re-exported below); it never reaches up to a front-end.

#![forbid(unsafe_code)]

// Re-export the backends crate so callers keep addressing the clients as
// `engine::backends::...`.
pub use backends;

pub mod audit;
pub mod bulk;
pub mod card;
pub mod error;
pub mod verify;
pub use error::{Error, Result};
pub mod paging;
pub mod scan;
pub mod seam;
pub mod store;
pub mod util;
