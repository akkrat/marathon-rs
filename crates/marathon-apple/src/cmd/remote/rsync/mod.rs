//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt
pub mod rsync_file_bridge;
pub mod rsync_target;
pub use rsync_file_bridge::RsyncFileBridge;
pub use rsync_target::RsyncTarget;
