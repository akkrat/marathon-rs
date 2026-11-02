//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v1/Xctestrun.kt

use std::sync::{Arc, Mutex};

use crate::plist::delegates::OrderedStringMap;
use crate::plist::{PlistDict, PlistError, PlistResult, PlistValue, PropertyList};

type Targets = Vec<(String, XctestrunTarget)>;

#[derive(Clone, Debug)]
pub struct Xctestrun {
    dict: PlistDict,
    /// `by lazy` in Kotlin: computed once, stale after later root mutation.
    targets_cache: Arc<Mutex<Option<Targets>>>,
}

impl Xctestrun {
    /// Targets are every dictionary value except `__xctestrun_metadata__`, in key order;
    /// unlike v0, non-dictionary values are silently skipped.
    pub fn targets(&self) -> Vec<(String, XctestrunTarget)> {
        let mut cache = self
            .targets_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(targets) = &*cache {
            return targets.clone();
        }
        let targets: Vec<(String, XctestrunTarget)> = self
            .dict
            .snapshot()
            .into_iter()
            .filter_map(|(key, value)| match value {
                PlistValue::Dict(dict) if key != "__xctestrun_metadata__" => Some((key, XctestrunTarget::wrap(dict))),
                _ => None,
            })
            .collect();
        *cache = Some(targets.clone());
        targets
    }

    /// `Map<String, Any>?` in Kotlin. The nested dictionary itself is the Rust surface:
    /// registered as a required delegate, so an absent key errors instead of returning
    /// `null`, despite the nullable declaration.
    pub fn metadata(&self) -> PlistResult<PlistDict> {
        match self.dict.get_cloned("__xctestrun_metadata__") {
            None => Err(PlistError::MissingKey {
                key: "__xctestrun_metadata__".to_owned(),
            }),
            Some(PlistValue::Dict(dict)) => Ok(dict),
            Some(other) => Err(PlistError::WrongType {
                key: "__xctestrun_metadata__".to_owned(),
                expected: "dictionary",
                found: other.type_name(),
            }),
        }
    }
}

impl PartialEq for Xctestrun {
    fn eq(&self, other: &Self) -> bool {
        self.dict == other.dict
    }
}

impl PropertyList for Xctestrun {
    fn wrap(dict: PlistDict) -> Self {
        Self {
            dict,
            targets_cache: Arc::new(Mutex::new(None)),
        }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct XctestrunTarget {
    dict: PlistDict,
}

impl XctestrunTarget {
    pub fn product_module_name(&self) -> PlistResult<String> {
        self.dict.required("ProductModuleName")
    }
    pub fn set_product_module_name(&self, value: String) {
        self.dict.set_value("ProductModuleName", value);
    }

    pub fn is_ui_test_bundle(&self) -> PlistResult<bool> {
        self.dict.required("IsUITestBundle")
    }
    pub fn set_is_ui_test_bundle(&self, value: bool) {
        self.dict.set_value("IsUITestBundle", value);
    }

    pub fn test_bundle_path(&self) -> PlistResult<String> {
        self.dict.required("TestBundlePath")
    }
    pub fn set_test_bundle_path(&self, value: String) {
        self.dict.set_value("TestBundlePath", value);
    }

    pub fn test_host_path(&self) -> PlistResult<String> {
        self.dict.required("TestHostPath")
    }
    pub fn set_test_host_path(&self, value: String) {
        self.dict.set_value("TestHostPath", value);
    }

    pub fn ui_target_app_path(&self) -> PlistResult<String> {
        self.dict.required("UITargetAppPath")
    }
    pub fn set_ui_target_app_path(&self, value: String) {
        self.dict.set_value("UITargetAppPath", value);
    }

    pub fn dependent_product_paths(&self) -> PlistResult<Vec<String>> {
        self.dict.required_array("DependentProductPaths")
    }
    pub fn set_dependent_product_paths(&self, value: Vec<String>) {
        self.dict.set_array("DependentProductPaths", value);
    }

    pub fn user_attachment_lifetime(&self) -> PlistResult<String> {
        self.dict.required("UserAttachmentLifetime")
    }
    pub fn set_user_attachment_lifetime(&self, value: String) {
        self.dict.set_value("UserAttachmentLifetime", value);
    }

    pub fn system_attachment_lifetime(&self) -> PlistResult<String> {
        self.dict.required("SystemAttachmentLifetime")
    }
    pub fn set_system_attachment_lifetime(&self, value: String) {
        self.dict.set_value("SystemAttachmentLifetime", value);
    }

    pub fn environment_variables(&self) -> PlistResult<OrderedStringMap> {
        self.dict.required_map("EnvironmentVariables")
    }
    pub fn set_environment_variables(&self, value: OrderedStringMap) {
        self.dict.set_map("EnvironmentVariables", value);
    }

    pub fn testing_environment_variables(&self) -> PlistResult<OrderedStringMap> {
        self.dict.required_map("TestingEnvironmentVariables")
    }
    pub fn set_testing_environment_variables(&self, value: OrderedStringMap) {
        self.dict.set_map("TestingEnvironmentVariables", value);
    }
}

impl PropertyList for XctestrunTarget {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
