//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/BaseAndroidDevice.kt
//! (the com.android.sdklib.AndroidVersion subset used by device setup).
use crate::support::trim;
use jcompat::writers::JavaString;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidVersion {
    pub api_level: i32,
    pub codename: Option<JavaString>,
}
impl Default for AndroidVersion {
    fn default() -> Self {
        Self {
            api_level: 1,
            codename: None,
        }
    }
}
impl AndroidVersion {
    pub fn new(api_level: i32, codename: &str) -> Self {
        let code = trim(codename);
        Self {
            api_level,
            codename: (!code.is_empty() && code != "REL").then(|| code.as_str().into()),
        }
    }
    pub fn feature_level(&self) -> i32 {
        self.api_level.wrapping_add(i32::from(self.codename.is_some()))
    }
    pub fn is_greater_or_equal_than(&self, api: i32) -> bool {
        self.api_level >= api
    }
    pub fn api_string_without_extension(&self) -> JavaString {
        self.codename
            .clone()
            .unwrap_or_else(|| self.api_level.to_string().as_str().into())
    }
}
