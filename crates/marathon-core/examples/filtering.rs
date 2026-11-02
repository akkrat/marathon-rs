//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt:252–256
//! Consumer example for WP 3.10: both run and parse call this before sharding.
use marathon_config::schema::FilteringConfiguration;
use marathon_core::{
    error::OperationResult,
    filter::{FilterDependencies, TestFilter},
    model::Test,
    runtime::VendorRuntime,
};

#[allow(dead_code)]
async fn select_tests(
    configuration: &FilteringConfiguration,
    mut tests: Vec<Test>,
    dependencies: &FilterDependencies,
    vendor: &dyn VendorRuntime,
) -> OperationResult<Vec<Test>> {
    for config in &configuration.allowlist {
        tests = TestFilter::from_config(config, dependencies)
            .filter(tests, vendor)
            .await?;
    }
    for config in &configuration.blocklist {
        tests = TestFilter::from_config(config, dependencies)
            .filter_not(tests, vendor)
            .await?;
    }
    Ok(tests)
}
fn main() {}
