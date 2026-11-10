//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt
//!
//! Model and service contracts, with the dedicated core runtime in executor.
//! See docs/design/lifecycle.md and docs/design/model-contract.md.

pub mod analytics;
pub mod construction;
pub mod diagnostics;
pub mod error;
pub mod filter;
pub mod listener;
pub mod logging;
pub mod marathon;
pub mod model;
pub mod output;
pub mod runtime;
pub mod strategy;
pub mod time;
pub mod vendor;
pub mod wiring;

pub mod execution;
pub mod executor;
pub mod state_machine;

mod artifact_io;
pub mod artifacts;
pub mod ci;
pub mod report;
pub mod transfer;

pub use artifact_io::ServiceLog;
pub mod reporters;
pub mod screenshot;
