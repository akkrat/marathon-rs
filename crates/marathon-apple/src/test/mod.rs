//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/test/TestRequest.kt

use marathon_core::model::Test;

#[derive(Clone, Debug, PartialEq)]
pub struct TestRequest {
    pub workdir: String,
    pub remote_xctestrun: String,
    pub coverage: bool,
    pub tests: Option<Vec<Test>>,
    pub xcresult: Option<String>,
    /// Not a filter: `to_xcodebuild_test_filter` ignores it.
    pub test_target_name: Option<String>,
}
impl TestRequest {
    pub fn new(workdir: impl Into<String>, remote_xctestrun: impl Into<String>, coverage: bool) -> Self {
        Self {
            workdir: workdir.into(),
            remote_xctestrun: remote_xctestrun.into(),
            coverage,
            tests: None,
            xcresult: None,
            test_target_name: None,
        }
    }
    /// One literal-quoted `-only-testing:` per test, in order and with duplicates.
    pub fn to_xcodebuild_test_filter(&self) -> Vec<String> {
        self.tests
            .iter()
            .flatten()
            .map(|test| format!("'-only-testing:{}'", test.to_test_name('/', '/').to_string_lossy()))
            .collect()
    }
}

pub mod event;
pub use event::TestEvent;
