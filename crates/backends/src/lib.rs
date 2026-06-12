//! Backend client traits, their HTTP implementations, and the [`Clients`] bundle
//! that hands them to the engine.
//!
//! Each submodule follows the same shape: a `*Client` trait (mockable via the
//! `mock` cargo feature), a live `*Http` struct that implements it, a `*Error`
//! enum, and `from_env` plus test constructors. [`Clients`] aggregates one live
//! client per backend so the engine can take a single `&Clients` rather than two
//! separate arguments.
//!
//! This crate depends only on `domain`. The id newtypes and [`DryRun`] it speaks
//! in come from there (re-exported through [`util`]); the status/role vocabulary
//! lives there too, and Solidarity Tech decodes its own raw membership status
//! into the shared `domain::MigsStatus`.
//!
//! [`DryRun`]: util::DryRun

pub mod prelude {
    pub use super::discord::DiscordClient;
    pub use super::solidarity_tech::SolidarityTechClient;
}

pub mod discord;
pub mod solidarity_tech;
pub mod util;

pub use util::secret::from_credstore_or_env;

use discord::DiscordError;
use solidarity_tech::SolidarityTechError;

/// The crate's top-level error: either backend's failure, surfaced to the engine
/// without flattening. Each arm carries the backend's own `thiserror` enum
/// verbatim (via `#[from]`), so `?` in [`Clients::from_env`] lifts cleanly while
/// the precise cause stays inspectable.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Discord(#[from] DiscordError),
    #[error(transparent)]
    SolidarityTech(#[from] SolidarityTechError),
}

/// One page of a paginated member read.
///
/// Plain pagination data - `backends` knows nothing about progress bars. A front
/// end that wants a progress bar drives the page loop itself (see the engine's
/// `paging::drain_pages`) and reads `scanned`/`total` to size and advance it;
/// callers that just want the whole list drive the same `drain_pages` loop with
/// a no-op progress sink.
///
/// ## Why a manual `members_page(cursor)` cursor and not a `Stream`
///
/// Pagination here is async (each page is an HTTP round-trip), so a plain
/// [`Iterator`] can't express it - `next` would have to `.await`. (The GAT /
/// lending-iterator limitation is *not* the issue: we yield owned `Vec`s, so
/// nothing borrows the cursor.) The async equivalent, a `futures::Stream`,
/// *would* work, but the backend trait here is object-safe and `#[automock]`ed
/// so the engine can unit-test against a mock. A stream-returning method can't
/// keep both: `-> impl Stream` makes the trait non-object-safe and
/// un-automockable, and the object-safe alternative is `BoxStream`
/// (`Pin<Box<dyn Stream>>`) - reintroducing the `dyn` we deliberately avoid, and
/// making every mock hand back a boxed stream. So instead the backend exposes a
/// concrete `members_page(cursor) -> MemberPage` (the `next`), and the engine's
/// generic `drain_pages` does the looping (the consumer): callers get
/// iterator-like ergonomics - a `Vec` back, no loop of their own - with zero
/// `dyn` and a trivially mockable backend.
///
/// - `members` - the records kept from this page (projected; malformed ones are
///   already dropped).
/// - `scanned` - raw rows paged over, which can exceed `members.len()` when a
///   malformed row is skipped.
/// - `total` - the collection's row count, which Solidarity Tech reports.
/// - `next` - the opaque cursor to pass back for the following page, or `None`
///   when the read is complete.
pub struct MemberPage<T> {
    pub members: Vec<T>,
    pub scanned: u64,
    pub total: Option<u64>,
    pub next: Option<String>,
}

/// Every live backend client, bundled so the engine takes one `&Clients`
/// argument instead of two.
///
/// The fields are public so a caller can reach a backend-specific method that is
/// not part of any shared trait. Built once at startup by
/// [`from_env`](Clients::from_env).
pub struct Clients {
    pub discord: discord::DiscordHttp,
    pub solidarity_tech: solidarity_tech::SolidarityTechHttp,
}

impl Clients {
    /// Builds both backend clients from environment variables.
    ///
    /// Each client wraps its token in `secrecy::SecretString`, so the tokens
    /// never `Debug`-print. This is the single startup step that reads every
    /// backend credential; a caller that needs only one backend builds that
    /// client directly instead.
    pub async fn from_env() -> Result<Self, Error> {
        // Backends listed alphabetically so future additions slot in by name
        // without overlapping diffs.
        let discord = discord::DiscordHttp::from_env().await?;
        let solidarity_tech = solidarity_tech::SolidarityTechHttp::from_env().await?;

        Ok(Clients {
            discord,
            solidarity_tech,
        })
    }
}
