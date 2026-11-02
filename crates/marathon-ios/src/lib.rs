//! The iOS simulator vendor: simulator provider, device, provisioning, recovery, late join and
//! listeners.
//!
//! See PLAN.md §4.1.

pub mod apple_simulator_application_installer;
pub mod apple_simulator_device;
pub mod apple_simulator_media_importer;
pub mod device;
pub mod ios_vendor;
pub mod simulator;

pub mod listener;
pub mod recorder_type_selector;
