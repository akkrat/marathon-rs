//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AllureTestFilter.kt
//! and io.qameta.allure:allure-test-filter:2.29.0 FileTestPlanSupplier/TestPlanV1_0.

use super::{
    failure, select,
    values::{FilterComponent, io_failure},
};
use crate::{
    error::OperationResult,
    model::{MetaValue, Test},
};
use marathon_error::FailureKind;
use marathon_logging::Level;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestPlan {
    V1_0(Vec<PlanTest>),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTest {
    pub id: Option<String>,
    pub selector: Option<String>,
}
impl TestPlan {
    pub fn is_selected(&self, id: Option<&str>, name: &str) -> bool {
        match self {
            Self::V1_0(tests) => tests
                .iter()
                .flat_map(|t| t.id.iter().chain(&t.selector))
                .any(|value| value == name || Some(value.as_str()) == id),
            Self::Unknown => false,
        }
    }
}
pub trait AllurePlanSupplier: Send + Sync {
    fn supply(&self) -> OperationResult<Option<TestPlan>>;
}
pub trait PlanEnvironment: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}
pub struct ProcessEnvironment;
impl PlanEnvironment for ProcessEnvironment {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var_os(key).map(|value| String::from_utf8_lossy(value.as_encoded_bytes()).into_owned())
    }
}
pub struct FileTestPlanSupplier {
    pub environment: Arc<dyn PlanEnvironment>,
    pub logger: Arc<dyn FilterComponent>,
}
const LOGGER: &str = "marathon_core::filter::allure";
impl AllurePlanSupplier for FileTestPlanSupplier {
    fn supply(&self) -> OperationResult<Option<TestPlan>> {
        let Some(path) = ["ALLURE_TESTPLAN_PATH", "AS_TESTPLAN_PATH"]
            .into_iter()
            .filter_map(|key| self.environment.get(key))
            .find(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let native = PathBuf::from(&path);
        if path.contains('\0') {
            let mut message = String::from("could not read test plan file ");
            message.push_str(&path);
            message.push_str(": invalid path");
            self.logger.log(Level::WARN, LOGGER, message, None);
            return Ok(None);
        }
        if !native.is_file() {
            return Ok(None);
        }
        // NIO Paths.get collapses repeated slashes, but preserves dot segments.
        let display = jcompat::file_manager::file_path(&native.to_string_lossy());
        let result = std::fs::read(&native)
            .map_err(|e| io_failure(&display, &e))
            .and_then(|bytes| decode_plan(&bytes));
        match result {
            Ok(plan) => Ok(Some(plan)),
            Err(error) => {
                let mut message = String::from("could not read test plan file ");
                message.push_str(&display.to_string_lossy());
                self.logger.log(Level::WARN, LOGGER, message, Some(error));
                Ok(None)
            }
        }
    }
}

/// P2: malformed/exotic Jackson inputs are a warning plus absence. Ordinary
/// objects, unknown fields and unknown/missing version dispatch retain parity.
pub fn decode_plan(bytes: &[u8]) -> Result<TestPlan, crate::error::Failure> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes)
        .ok()
        .map(jcompat::jackson::normalize_json_strings);
    let bytes = text.as_deref().map_or(bytes, str::as_bytes);
    // serde_json keeps the last duplicate key where Jackson's type dispatch can
    // see the first, so a duplicate `version` could silently flip V1 to
    // Unknown. Duplicate keys at any depth are malformed under P2 instead.
    serde_json::from_slice::<UniqueKeys>(bytes).map_err(|e| failure(FailureKind::Io, &e.to_string()))?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| failure(FailureKind::Io, &e.to_string()))?;
    let invalid = || failure(FailureKind::Io, "invalid Allure test plan");
    let object = value.as_object().ok_or_else(invalid)?;
    let version = object.get("version");
    if !matches!(version, Some(serde_json::Value::String(v)) if v == "1.0") {
        if version.is_some_and(|v| !v.is_string() && !v.is_null()) {
            return Err(invalid());
        }
        return Ok(TestPlan::Unknown);
    }
    let tests = object.get("tests").and_then(|v| v.as_array()).ok_or_else(invalid)?;
    let string = |v: Option<&serde_json::Value>| -> Result<Option<String>, crate::error::Failure> {
        match v {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.as_str().into())),
            _ => Err(invalid()),
        }
    };
    let tests = tests
        .iter()
        .map(|test| {
            let test = test.as_object().ok_or_else(invalid)?;
            Ok(PlanTest {
                id: string(test.get("id"))?,
                selector: string(test.get("selector"))?,
            })
        })
        .collect::<Result<_, crate::error::Failure>>()?;
    Ok(TestPlan::V1_0(tests))
}

/// Validation-only JSON value that rejects an object repeating a key.
struct UniqueKeys;
impl<'de> serde::Deserialize<'de> for UniqueKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueKeysVisitor)
    }
}
struct UniqueKeysVisitor;
impl<'de> serde::de::Visitor<'de> for UniqueKeysVisitor {
    type Value = UniqueKeys;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_i64<E>(self, _: i64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_u64<E>(self, _: u64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_f64<E>(self, _: f64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_str<E>(self, _: &str) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_unit<E>(self) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueKeys, A::Error> {
        while seq.next_element::<UniqueKeys>()?.is_some() {}
        Ok(UniqueKeys)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<UniqueKeys, A::Error> {
        let mut keys = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            map.next_value::<UniqueKeys>()?;
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
        }
        Ok(UniqueKeys)
    }
}

#[derive(Clone)]
pub struct AllureFilter {
    enabled: bool,
    supplier: Arc<dyn AllurePlanSupplier>,
    cache: Arc<Mutex<Option<Option<TestPlan>>>>,
}
impl AllureFilter {
    pub(super) fn new(enabled: bool, supplier: Arc<dyn AllurePlanSupplier>) -> Self {
        Self {
            enabled,
            supplier,
            cache: Arc::default(),
        }
    }
    pub(super) fn select(&self, tests: &[Test], negate: bool) -> OperationResult<Vec<Test>> {
        let plan = if self.enabled {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| failure(FailureKind::IllegalState, "Allure cache poisoned"))?;
            if cache.is_none() {
                *cache = Some(self.supplier.supply()?);
            }
            cache.as_ref().cloned().flatten()
        } else {
            None
        };
        let Some(plan @ TestPlan::V1_0(_)) = plan else {
            return Ok(if negate { Vec::new() } else { tests.to_vec() });
        };
        select(tests, negate, |test| {
            let first = test
                .meta_properties
                .iter()
                .find(|p| p.name == "io.qameta.allure.AllureId" || p.name == "io.qameta.allure.kotlin.AllureId");
            let id = first
                .and_then(|p| p.values.entries().iter().find(|(k, _)| *k == "value"))
                .and_then(|(_, v)| if let MetaValue::String(s) = v { Some(s) } else { None });
            Ok(plan.is_selected(id.map(|x| x.as_str()), &test.to_test_name('.', '.')))
        })
    }
}
