//! Port of core/src/test/kotlin/com/malinskiy/marathon/TestGenerator.kt
//! and execution/filter test helpers.
#![allow(dead_code)]
#[path = "../strategy_support/mod.rs"]
mod strategy_support;

use jcompat::{JavaRegex, logging::Level};
use marathon_config::schema::{RegexValuesFileFilter, TestFilterConfiguration as C};
use marathon_core::{
    error::{Failure, OperationResult},
    executor::{CoreExecutor, hooks::RuntimeHooks},
    filter::{
        AnnotationRenderer, FilterDependencies, TestFilter,
        allure::{AllurePlanSupplier, TestPlan},
        values::{FileValuesReader, FilterLogger},
    },
    model::{MetaArray, MetaMap, MetaProperty, MetaValue, Test},
    time::ObjectIdentity,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
pub use strategy_support::generate_test as test;
pub fn generate_tests(n: usize) -> Vec<Test> {
    strategy_support::generate_tests(n)
}

pub fn fail(message: &str) -> Failure {
    Failure {
        class: "java.io.IOException".into(),
        message: Some(message.into()),
        cause: None,
        stacktrace: None,
    }
}
pub fn regex(s: &str) -> JavaRegex {
    JavaRegex::compile(s, "filter test").expect("supported test regex")
}
pub fn config(r: Option<&str>, values: Option<&[&str]>, enabled: bool) -> RegexValuesFileFilter {
    RegexValuesFileFilter {
        regex: r.map(regex),
        values: values.map(|v| v.iter().map(|s| String::from(*s)).collect()),
        file: None,
        enabled,
    }
}
pub fn property(name: &str, value: MetaValue) -> MetaProperty {
    MetaProperty {
        name: name.into(),
        values: [("value".into(), value)].into_iter().collect(),
    }
}
pub fn annotated(name: &str, value: MetaValue) -> Test {
    let mut t = test("com.sample", "SimpleTest", "fakeMethod");
    t.meta_properties.push(property(name, value));
    t
}
pub fn array(values: Vec<MetaValue>) -> MetaValue {
    MetaValue::Array(Arc::new(MetaArray {
        jvm_class: "[Ljava.lang.Object;".into(),
        elements: values,
    }))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub level: Level,
    pub logger: String,
    pub message: String,
    pub cause: Option<Failure>,
}
#[derive(Default)]
pub struct Logs(pub Mutex<Vec<Record>>);
impl FilterLogger for Logs {
    fn log(&self, level: Level, logger: &str, message: String, cause: Option<Failure>) {
        self.0.lock().expect("test log lock").push(Record {
            level,
            logger: logger.into(),
            message,
            cause,
        });
    }
}
pub struct Plan(pub Option<TestPlan>);
impl AllurePlanSupplier for Plan {
    fn supply(&self) -> OperationResult<Option<TestPlan>> {
        Ok(self.0.clone())
    }
}
pub fn deps(plan: Option<TestPlan>, logs: Arc<Logs>) -> FilterDependencies {
    FilterDependencies {
        values: Arc::new(FileValuesReader { logger: logs }),
        allure: Arc::new(Plan(plan)),
        rendering: Arc::new(AnnotationRenderer::new(
            RuntimeHooks::live(),
            ObjectIdentity(0),
            Arc::new({
                let next = AtomicUsize::new(1);
                move || ObjectIdentity(next.fetch_add(1, Ordering::SeqCst) as u64)
            }),
        )),
    }
}
pub fn filter(config: &C) -> TestFilter {
    TestFilter::from_config(config, &deps(None, Arc::default()))
}
pub fn apply(filter: TestFilter, tests: Vec<Test>, not: bool) -> OperationResult<Vec<Test>> {
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            if not {
                filter.filter_not(tests, core.vendor().as_ref()).await
            } else {
                filter.filter(tests, core.vendor().as_ref()).await
            }
        })
    })
}
pub fn check(config: C, tests: &[Test], not: bool, expected: &[Test]) -> OperationResult<()> {
    assert_eq!(apply(filter(&config), tests.to_vec(), not)?, expected);
    Ok(())
}
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(&format!(
        "{}/../../fixtures/core/testfilters/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new() -> OperationResult<Self> {
        let p = std::env::temp_dir().join(format!(
            "mrs-filter-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&p).map_err(|e| fail(&e.to_string()))?;
        Ok(Self(p))
    }
    pub fn path(&self, name: &str) -> PathBuf {
        PathBuf::from(self.0.join(name).to_string_lossy().into_owned())
    }
    pub fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> OperationResult<()> {
        std::fs::write(self.0.join(name), bytes).map_err(|e| fail(&e.to_string()).into())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn file_config(path: PathBuf) -> C {
    C::FullyQualifiedClassname(RegexValuesFileFilter {
        file: Some(path),
        ..config(None, None, true)
    })
}
pub fn read(path: &std::path::Path) -> OperationResult<Vec<String>> {
    use marathon_core::filter::values::FilterValuesReader;
    FileValuesReader {
        logger: Arc::new(Logs::default()),
    }
    .read_values(path)
}
pub fn mkdir(path: impl AsRef<Path>) -> OperationResult<()> {
    std::fs::create_dir(path).map_err(|e| fail(&e.to_string()).into())
}
pub fn remove_dir(path: impl AsRef<Path>) -> OperationResult<()> {
    std::fs::remove_dir(path).map_err(|e| fail(&e.to_string()).into())
}
pub fn empty_map() -> MetaMap {
    MetaMap::default()
}
