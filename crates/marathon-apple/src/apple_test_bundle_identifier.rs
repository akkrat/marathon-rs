//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleTestBundleIdentifier.kt
use crate::{cmd::failure, model::AppleTestBundle};
use marathon_core::{
    error::{VendorError, VendorResult},
    model::Test,
    vendor::{BundleRef, TestBundleIdentifier},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
pub struct AppleTestBundleIdentifier(Mutex<HashMap<Test, Arc<AppleTestBundle>>>);
impl AppleTestBundleIdentifier {
    pub fn put(&self, test: Test, bundle: Arc<AppleTestBundle>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(test, bundle);
    }
    pub fn bundle(&self, test: &Test) -> VendorResult<Arc<AppleTestBundle>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(test)
            .cloned()
            .ok_or_else(|| {
                VendorError::Other(failure(
                    "com.malinskiy.marathon.config.exceptions.ConfigurationException",
                    format!(
                        "Invalid test {}: can't locate test bundle",
                        test.to_test_name('.', '#').to_owned()
                    ),
                ))
            })
    }
}
impl TestBundleIdentifier for AppleTestBundleIdentifier {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        Ok(Some(BundleRef(self.bundle(test)?)))
    }
}
