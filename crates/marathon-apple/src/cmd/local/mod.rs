//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/local/KotlinProcessHost.kt

pub mod jvm_file_bridge;
pub mod kotlin_process_command_executor;
use super::CommandHost;
pub use jvm_file_bridge::JvmFileBridge;
pub use kotlin_process_command_executor::KotlinProcessCommandExecutor;

pub struct KotlinProcessHost;
impl KotlinProcessHost {
    pub fn host() -> CommandHost {
        CommandHost {
            id: "local".into(),
            charset: "UTF-8".into(),
        }
    }
}
