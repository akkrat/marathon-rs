//! Reusable loopback SSH worker for differential tests (WP 0.4a).
//! This is test infrastructure, not the WP 4.2 production executor.

pub mod apple;
pub mod execution;
pub mod fs;
pub mod processes;
pub mod router;
pub mod script;
pub mod server;
pub mod transcript;

pub use server::{Config, RunningWorker, start};
