//! In-process mock of Solidarity Tech's `GET /users`, for staging only.
//!
//! [`spawn`] stands up a roster of fabricated members - built from named
//! [`Persona`]s keyed to real test-server Discord ids - so the staging bot reads
//! every membership state without touching real records. Read-only by design: it
//! answers only the list read the bot's index build makes, nothing else.

#![forbid(unsafe_code)]

mod persona;
mod roster;
mod server;

pub use persona::Persona;

use std::net::SocketAddr;
use std::sync::Arc;

/// Start the mock server and return the address it bound.
///
/// The listener is bound before this returns, so a caller that points its client at
/// the address and reads immediately cannot race a not-yet-listening server; serving
/// then continues on a background task. `personas` is the
/// `SOLIDARITY_TECH_MOCK_PERSONAS` value - an empty string yields an empty roster, so
/// every member reads as "not a member". Errors if `listen` cannot be bound, most
/// often because another process already holds that port.
pub async fn spawn(listen: &str, personas: &str) -> std::io::Result<SocketAddr> {
    let today = chrono::Local::now().date_naive();
    let roster = Arc::new(roster::build(personas, today));
    let app = server::router(roster);

    let listener = tokio::net::TcpListener::bind(listen).await.map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "could not bind the mock Solidarity Tech server to {listen} \
                 (is another process already using that port?): {e}"
            ),
        )
    })?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "mock Solidarity Tech server bound");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "mock Solidarity Tech server exited");
        }
    });
    Ok(addr)
}
