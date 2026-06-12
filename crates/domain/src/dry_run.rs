//! The [`DryRun`] flag every write path threads through, with its named
//! `LIVE`/`DRY` constants.

/// A marker, carried through every write path, that suppresses network calls
/// when set.
///
/// When [`is_dry`](DryRun::is_dry) is true a backend write MUST log the call it
/// *would* have made - target and payload - at `info` and return `Ok(...)`
/// without touching the network. It is a newtype rather than a bare `bool` so
/// the intent is legible in every signature: `set_role(.., DryRun::DRY)` reads
/// unambiguously where `set_role(.., true)` would not, and the named constants
/// [`LIVE`](DryRun::LIVE) and [`DRY`](DryRun::DRY) can stand in for the raw
/// value. Every backend write method takes one, and the caller chooses
/// [`LIVE`](DryRun::LIVE) or [`DRY`](DryRun::DRY) at the call site (the bot's
/// write paths arrive in a future implementation).
///
/// ```
/// use domain::DryRun;
///
/// // A backend write method's guard clause is shaped like this:
/// fn perform_write(dry_run: DryRun) -> bool {
///     if dry_run.is_dry() {
///         // log the intended call, then skip the network
///         return false;
///     }
///     // ...the real request would go here
///     true
/// }
///
/// assert!(!perform_write(DryRun::DRY));
/// assert!(perform_write(DryRun::LIVE));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRun(pub bool);

impl DryRun {
    /// Live mode: writes hit the network. Prefer this over `DryRun(false)`.
    pub const LIVE: DryRun = DryRun(false);
    /// Dry-run mode: writes are logged and skipped. Prefer this over `DryRun(true)`.
    pub const DRY: DryRun = DryRun(true);

    /// Returns `true` when writes should be logged and skipped instead of sent.
    ///
    /// ```
    /// use domain::DryRun;
    ///
    /// assert!(DryRun::DRY.is_dry());
    /// assert!(!DryRun::LIVE.is_dry());
    /// ```
    pub fn is_dry(self) -> bool {
        self.0
    }
}
