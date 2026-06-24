//! Channel-permission terraform: reshape a guild's overwrites so the membership
//! roles, not `@everyone`, decide visibility, while leaving structure and every
//! other permission untouched.
//!
//! [`model`] is the pure, IO-free heart - the overwrite math and the
//! [`SetupConfig`] the classifier consumes. The facade, plan, snapshot, and
//! report modules arrive in later tasks and will be wired here as they land.
//! Nothing below this module touches the network.

mod model;

pub use model::SetupConfig;
