//! Channel-permission terraform: reshape a guild's overwrites so the membership
//! roles, not `@everyone`, decide visibility, while leaving structure and every
//! other permission untouched.
//!
//! [`model`] is the pure, IO-free heart - the overwrite math and the
//! [`SetupConfig`] the classifier consumes. The facade, plan, snapshot, and
//! report modules arrive in later tasks and will be wired here as they land.
//! Nothing below this module touches the network.

mod facade;
mod model;
mod plan;
mod report;
pub(crate) mod snapshot;

pub use facade::{ApplyOutcome, Channels, ChannelsError, RestoreOutcome};
pub use model::SetupConfig;
pub use plan::{
    ChannelAction, ChannelPlan, DesyncEntry, DesyncReport, PlanCounts, PlannedChannel,
    desync_report, resolve_plan, verification_breaches,
};
pub use report::{detail_markdown, summary_lines, unverified_visibility};
pub use snapshot::{ChannelSnapshot, SNAPSHOT_FORMAT_VERSION, SavedChannel, SnapshotMeta};
