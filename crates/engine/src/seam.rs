//! Progress reporting for the long member-list reads.
//!
//! The page-draining sweeps ([`crate::paging::drain_pages`]) report through the
//! [`Progress`] trait so the front end owns presentation while the engine owns
//! the page loop. The bot runs unattended and supplies [`NoProgress`]; an
//! interactive front end would hand over real bars instead.

/// A single in-flight progress bar, advanced as work proceeds and finished when
/// the loop ends. Mirrors the indicatif methods the member loops call.
pub trait ProgressBar {
    fn inc(&self, n: u64);
    /// Replace the bar's message (used to surface a running secondary count, e.g.
    /// "N found", alongside the position).
    fn set_message(&self, msg: &str);
    fn finish_and_clear(&self);
    fn abandon_with_message(&self, msg: &str);
}

/// Builds the progress bars the long loops report through.
pub trait Progress {
    type Bar: ProgressBar;
    /// Start a determinate bar of `len` steps labeled `label`.
    fn bar(&self, len: u64, label: &str) -> Self::Bar;
    /// Start an indeterminate spinner labeled `label`, for a loop whose total is
    /// not known up front. Advanced the same way; it just shows the running count.
    fn spinner(&self, label: &str) -> Self::Bar;
}

/// A progress reporter that does nothing - the tests' stand-in and the default
/// for an unattended (bot) run.
pub struct NoProgress;

/// The no-op bar [`NoProgress`] hands out.
pub struct NoBar;

impl ProgressBar for NoBar {
    fn inc(&self, _n: u64) {}
    fn set_message(&self, _msg: &str) {}
    fn finish_and_clear(&self) {}
    fn abandon_with_message(&self, _msg: &str) {}
}

impl Progress for NoProgress {
    type Bar = NoBar;
    fn bar(&self, _len: u64, _label: &str) -> NoBar {
        NoBar
    }
    fn spinner(&self, _label: &str) -> NoBar {
        NoBar
    }
}
