//! An async ADB client, plus a scripted ADB server used by tests and the oracle.
//!
//! See PLAN.md §4.1.

#[cfg(feature = "server")]
pub mod server;

pub mod client;
pub mod instrumentation;
pub mod testrunner;

pub mod instrument_request;
