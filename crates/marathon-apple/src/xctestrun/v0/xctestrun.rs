//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/v0/Xctestrun.kt

use std::sync::{Arc, Mutex};

use super::test_target::TestTarget;
use crate::plist::{PlistDict, PlistError, PlistResult, PlistValue, PropertyList};

type Targets = Vec<(String, TestTarget)>;

#[derive(Clone, Debug)]
pub struct Xctestrun {
    dict: PlistDict,
    /// Kotlin `targets` is `by lazy`: computed once and stale after later mutation of
    /// the root dictionary. A failed computation is not cached.
    targets_cache: Arc<Mutex<Option<Targets>>>,
}

impl Xctestrun {
    pub fn new(target_name: &str, target: &TestTarget) -> Self {
        let run = Self::wrap(PlistDict::new());
        run.dict.insert(target_name, PlistValue::Dict(target.dict().clone()));
        run
    }

    /// Every top-level key becomes a target, in key order; a non-dictionary value is an
    /// error (a `ClassCastException` in Kotlin).
    pub fn targets(&self) -> PlistResult<Vec<(String, TestTarget)>> {
        let mut cache = self
            .targets_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(targets) = &*cache {
            return Ok(targets.clone());
        }
        let mut targets = Vec::new();
        for (key, value) in self.dict.snapshot() {
            match value {
                PlistValue::Dict(dict) => targets.push((key, TestTarget::wrap(dict))),
                other => {
                    return Err(PlistError::WrongType {
                        key,
                        expected: "dictionary",
                        found: other.type_name(),
                    });
                }
            }
        }
        *cache = Some(targets.clone());
        Ok(targets)
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
