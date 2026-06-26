//! The SSO role-check endpoint: a localhost-only OAuth broker that answers, with a
//! short-lived signed assertion, whether a person holds the configured Member role.

// On non-unix targets (Windows dev box) the `serve` function and the types it drives
// are not compiled. Suppress dead-code warnings there while still letting Linux/CI
// catch genuine dead code (on Linux everything is reachable from `serve`).
#![cfg_attr(not(unix), allow(dead_code))]

pub mod assertion;
pub mod config;
pub mod flow;
pub mod server;
pub mod store;
