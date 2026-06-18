//! Service-manager readiness and liveness notifications.
//!
//! On Linux these speak the `sd_notify` protocol to `systemd`; everywhere else (and on
//! Linux when not started by a service manager) they are no-ops. Detection is automatic:
//! the protocol keys off `$NOTIFY_SOCKET` / `$WATCHDOG_USEC`, so running `./botonio-botsci` by
//! hand sends nothing. Notification failures are logged at debug and never propagated -
//! a missing socket must not take the bot down.

use std::time::Duration;

/// Tell the service manager the bot is up and able to serve (`READY=1`).
pub fn ready() {
    notify(&[State::Ready], "ready");
}

/// Tell the service manager the bot is shutting down (`STOPPING=1`).
pub fn stopping() {
    notify(&[State::Stopping], "stopping");
}

/// Send one watchdog keep-alive (`WATCHDOG=1`).
pub fn watchdog_ping() {
    notify(&[State::Watchdog], "watchdog");
}

/// The interval at which to send watchdog pings - half the configured `WatchdogSec` -
/// or `None` if no watchdog is configured (or off Linux). A `WatchdogSec` so small that
/// half of it rounds to zero is treated as unconfigured: `tokio::time::interval(0)`
/// panics, and a sub-microsecond ping cadence is meaningless.
pub fn watchdog_interval() -> Option<Duration> {
    watchdog_usec().and_then(|usec| {
        let half = usec / 2;
        (half > 0).then(|| Duration::from_micros(half))
    })
}

#[cfg(target_os = "linux")]
enum State {
    Ready,
    Stopping,
    Watchdog,
}

#[cfg(target_os = "linux")]
fn notify(states: &[State], what: &str) {
    let mapped: Vec<sd_notify::NotifyState> = states
        .iter()
        .map(|s| match s {
            State::Ready => sd_notify::NotifyState::Ready,
            State::Stopping => sd_notify::NotifyState::Stopping,
            State::Watchdog => sd_notify::NotifyState::Watchdog,
        })
        .collect();
    if let Err(e) = sd_notify::notify(false, &mapped) {
        tracing::debug!(error = %e, "sd_notify {what} failed (no service manager?)");
    }
}

#[cfg(target_os = "linux")]
fn watchdog_usec() -> Option<u64> {
    let mut usec = 0u64;
    sd_notify::watchdog_enabled(false, &mut usec).then_some(usec)
}

// --- non-Linux no-op stubs -------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
enum State {
    Ready,
    Stopping,
    Watchdog,
}

#[cfg(not(target_os = "linux"))]
fn notify(_states: &[State], _what: &str) {}

#[cfg(not(target_os = "linux"))]
fn watchdog_usec() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_interval_is_none_without_a_configured_watchdog() {
        // No `$WATCHDOG_USEC` in the test environment (and always None off Linux),
        // so the bot must not try to ping a watchdog that was never asked for.
        assert_eq!(watchdog_interval(), None);
    }
}
