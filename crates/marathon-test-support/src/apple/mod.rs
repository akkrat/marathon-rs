//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
pub mod fake_simulator_worker;
pub mod world_executor;
pub use fake_simulator_worker::FakeSimulatorWorker;
pub use world_executor::{WorldExecutor, WorldFileBridge};
