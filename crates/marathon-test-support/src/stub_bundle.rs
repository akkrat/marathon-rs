//! Port of vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubTestBundle.kt
//! and vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubTestBundleIdentifier.kt.
use crate::lock;

use marathon_core::{
    error::*,
    executor::shared::failure,
    model::Test,
    vendor::{BundleRef, TestBundle, TestBundleIdentifier},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// An ordinary class: identity, not the id, is equality (stable `BundleRef`).
pub struct StubTestBundle {
    id: String,
}
impl StubTestBundle {
    pub fn bundle(id: &str) -> BundleRef {
        BundleRef(Arc::new(Self { id: id.into() }))
    }
}
impl TestBundle for StubTestBundle {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Default)]
pub struct StubTestBundleIdentifier {
    test_to_bundle: Mutex<HashMap<Test, BundleRef>>,
}
impl StubTestBundleIdentifier {
    pub fn put(&self, test: Test, bundle: BundleRef) {
        lock(&self.test_to_bundle).insert(test, bundle);
    }
}
impl TestBundleIdentifier for StubTestBundleIdentifier {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        match lock(&self.test_to_bundle).get(test) {
            Some(bundle) => Ok(Some(bundle.clone())),
            None => {
                let mut message = String::from("Invalid test ");
                message.push_str(&test.to_test_name('.', '#'));
                message.push_str(": can't locate test bundle");
                Err(VendorError::Other(Failure {
                    message: Some(message),
                    ..failure("com.malinskiy.marathon.config.exceptions.ConfigurationException", "")
                }))
            }
        }
    }
}

/// Mockito's `mock<TestBundleIdentifier>()` in MarathonFactory: every lookup is null.
pub struct MockTestBundleIdentifier;
impl TestBundleIdentifier for MockTestBundleIdentifier {
    fn identify(&self, _test: &Test) -> VendorResult<Option<BundleRef>> {
        Ok(None)
    }
}
