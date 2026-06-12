//! Shared test helpers.
//!
//! Gated behind `cfg(test)` for this crate's own unit tests. Holds the
//! `async_trait` mock-future helper the store tests would otherwise re-declare.

use std::future::Future;
use std::pin::Pin;

/// Wraps an owned value as the boxed future the `async_trait`-desugared mock
/// methods expect, so a `.returning(..)` reads `ready_ok(v)` instead of a
/// hand-rolled pin/box.
pub fn ready_ok<T, E>(v: T) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send>>
where
    T: Send + 'static,
    E: 'static,
{
    Box::pin(async move { Ok(v) })
}
