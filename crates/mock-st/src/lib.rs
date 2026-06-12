//! In-process mock of Solidarity Tech's `GET /users`, for staging only.
//!
//! Serves a roster built from named personas (see [`Persona`]) keyed to real
//! test-server Discord ids, so the staging bot reads fabricated members across
//! every membership state without touching real records. Read-only and one
//! endpoint by design; see the crate's design doc.

#![forbid(unsafe_code)]

mod persona;
mod roster;
mod server;

pub use persona::Persona;

use std::net::SocketAddr;
use std::sync::Arc;

/// Bind the mock to `listen`, build the roster from the `personas` map (dated to
/// today), spawn the accept loop, and return the bound address.
///
/// The listener is bound before returning, so a caller that points its client at
/// the returned address and reads immediately cannot race a not-yet-listening
/// server. `personas` is the `SOLIDARITY_TECH_MOCK_PERSONAS` value (an empty
/// string yields an empty roster - everyone reads as "not a member").
pub async fn spawn(listen: &str, personas: &str) -> std::io::Result<SocketAddr> {
    let today = chrono::Local::now().date_naive();
    let roster = Arc::new(roster::build(personas, today));
    let app = server::router(roster);

    let listener = tokio::net::TcpListener::bind(listen).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "mock Solidarity Tech server bound");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "mock Solidarity Tech server exited");
        }
    });
    Ok(addr)
}
