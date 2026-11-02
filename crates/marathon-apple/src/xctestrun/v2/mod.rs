//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v2/Xctestrun.kt

mod code_coverage_buildable_info;
mod metadata;
mod test_configuration;
mod test_plan;
mod test_target;
mod xctestrun;

pub use code_coverage_buildable_info::{CodeCoverageBuildableInfo, CodeCoverageBuildableInfoParams};
pub use metadata::Metadata;
pub use test_configuration::TestConfiguration;
pub use test_plan::TestPlan;
pub use test_target::{ArtifactReinstallParams, ArtifactReuseParams, TestTarget};
pub use xctestrun::Xctestrun;
