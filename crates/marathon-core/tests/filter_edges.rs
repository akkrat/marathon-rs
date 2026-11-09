//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/*.kt
//! (JVM differential probes and lifecycle regressions beyond the Kotlin tests).
mod filter_support;
use filter_support::*;
use jcompat::logging::Level;
use marathon_config::schema::{CompositionOperation as Op, TestFilterConfiguration as C, TestPackageFilter};
use marathon_core::{
    error::{Cancellation, OperationError, OperationResult},
    executor::CoreExecutor,
    filter::{
        AnnotationRenderer, TestFilter,
        allure::{AllurePlanSupplier, FileTestPlanSupplier, PlanEnvironment, PlanTest, TestPlan},
        array_element_value,
        values::{FilterValuesReader, parse_values},
    },
    model::{DecodedWrapper as D, MetaValue as M, Test},
};
use serde_json::{Value as J, json};
use std::path::PathBuf;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn rows(table: &str) -> Vec<J> {
    let text = match table {
        "leaves" => include_str!("../../../fixtures/oracle/filters/leaves.jsonl"),
        "composition" => include_str!("../../../fixtures/oracle/filters/composition.jsonl"),
        "annotation" => include_str!("../../../fixtures/oracle/filters/annotation.jsonl"),
        "files" => include_str!("../../../fixtures/oracle/filters/files.jsonl"),
        "allure" => include_str!("../../../fixtures/oracle/filters/allure.jsonl"),
        _ => panic!("table"),
    };
    text.split('\n')
        .skip(1)
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(s).expect("oracle JSON"))
        .collect()
}
fn names(tests: &[Test]) -> J {
    json!(
        tests
            .iter()
            .map(|t| [t.pkg.to_owned(), t.clazz.to_owned(), t.method.to_owned()])
            .collect::<Vec<_>>()
    )
}
fn annotations(tests: &[Test]) -> J {
    json!(
        tests
            .iter()
            .map(|t| t.meta_properties.iter().map(|p| p.name.to_owned()).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    )
}
fn inputs(row: &J) -> Vec<Test> {
    row["input"]
        .as_array()
        .expect("inputs")
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut t = test(
                t[0].as_str().expect("pkg"),
                t[1].as_str().expect("class"),
                t[2].as_str().expect("method"),
            );
            if let Some(props) = row["annotations"][i].as_array() {
                t.meta_properties = props
                    .iter()
                    .map(|n| property(n.as_str().expect("annotation"), M::String("yes".into())))
                    .collect();
            }
            t
        })
        .collect()
}
fn spec(row: &J) -> C {
    let r = row["regex"].as_str();
    let values = row["values"]
        .as_array()
        .map(|v| v.iter().map(|s| s.as_str().expect("value")).collect::<Vec<_>>());
    let cfg = config(r, values.as_deref(), row["enabled"].as_bool().expect("enabled"));
    match row["type"].as_str().expect("type") {
        "class" => C::SimpleClassname(cfg),
        "simple" => C::SimpleTestname(cfg),
        "fqclass" => C::FullyQualifiedClassname(cfg),
        "fqtest" => C::FullyQualifiedTestname(cfg),
        "method" => C::TestMethod(cfg),
        "annotation" => C::Annotation(cfg),
        "package" => C::TestPackage(TestPackageFilter {
            regex: cfg.regex,
            values: cfg.values,
            file: None,
            enabled: cfg.enabled,
            subpackages: row["subpackages"].as_bool().expect("subpackages"),
        }),
        _ => panic!("spec"),
    }
}
#[test]
fn every_leaf_and_fragment_replay_the_jvm() -> OperationResult<()> {
    for row in rows("leaves") {
        if row.get("d6").is_some() {
            continue;
        }
        let cnf = if let Some(fragment) = row["fragment"].as_array() {
            C::Fragmentation {
                index: fragment[0].as_i64().expect("index") as i32,
                count: fragment[1].as_i64().expect("count") as i32,
                enabled: true,
            }
        } else {
            spec(&row["config"])
        };
        for (not, key) in [(false, "filter"), (true, "filterNot")] {
            let result = apply(filter(&cnf), inputs(&row), not);
            if row[key]["error"].is_object() {
                let Err(OperationError::Failed(e)) = result else {
                    panic!("expected failure")
                };
                assert_eq!(
                    e.kind,
                    marathon_test_support::recorded_errors::kind(row[key]["error"]["class"].as_str().unwrap())
                );
                assert_eq!(
                    e.message.map(|s| s.to_owned()),
                    row[key]["error"]["message"].as_str().map(String::from)
                );
            } else {
                assert_eq!(names(&result?), row[key], "{row}");
            }
        }
        if row.get("fragment").is_some() {
            assert_eq!(names(&apply(filter(&cnf), vec![], false)?), row["empty"]);
        }
    }
    Ok(())
}
fn composition_config(op: Op, n: usize) -> C {
    let children = [
        C::Annotation(config(None, Some(&["B"]), true)),
        C::TestPackage(TestPackageFilter {
            regex: None,
            values: Some(vec!["pkg".into()]),
            file: None,
            subpackages: false,
            enabled: true,
        }),
        C::TestMethod(config(Some("three"), None, true)),
    ];
    C::Composition {
        filters: children.into_iter().take(n).collect(),
        op,
        enabled: true,
    }
}
#[test]
fn composition_order_and_metadata_replay_the_jvm() -> OperationResult<()> {
    let all = rows("composition");
    for row in &all {
        if row.get("nested").is_some() {
            let c = C::Composition {
                filters: vec![
                    C::SimpleClassname(config(Some("Other"), None, true)),
                    C::Composition {
                        filters: vec![C::TestMethod(config(Some("three"), None, true))],
                        op: Op::Subtract,
                        enabled: true,
                    },
                ],
                op: Op::Union,
                enabled: true,
            };
            assert_eq!(names(&apply(filter(&c), inputs(&all[0]), false)?), row["filter"]);
            continue;
        }
        let op = match row["op"].as_str().expect("op") {
            "UNION" => Op::Union,
            "INTERSECTION" => Op::Intersection,
            _ => Op::Subtract,
        };
        let c = composition_config(op, row["children"].as_u64().expect("children") as usize);
        for (not, key, meta) in [(false, "filter", "filterMeta"), (true, "filterNot", "filterNotMeta")] {
            let out = apply(filter(&c), inputs(row), not)?;
            assert_eq!(names(&out), row[key]);
            assert_eq!(annotations(&out), row[meta]);
        }
    }
    Ok(())
}
fn wrapper(kind: &str) -> M {
    M::Decoded(match kind {
        "byte" => D::DecodedByte(-1),
        "short" => D::DecodedShort(-2),
        "int" => D::DecodedInt(-3),
        "long" => D::DecodedLong(-4),
        "float" => D::DecodedFloat(-0.0),
        "double" => D::DecodedDouble(f64::NAN),
        "char" => D::DecodedChar(b')'.into()),
        "boolean" => D::DecodedBoolean(true),
        "decodedString" => D::DecodedString("a)b".into()),
        "type" => D::DecodedType("Ltype;".into()),
        "enum" => D::DecodedEnum("ENUM".into()),
        "decodedNull" => D::DecodedNull,
        "nested" => D::DecodedArrayValue(vec![D::DecodedString("a)b".into()), D::DecodedNull].into()),
        "empty" => D::DecodedArrayValue(vec![].into()),
        _ => panic!("wrapper"),
    })
}
#[test]
fn c28_wrapper_and_scalar_rendering_replays_the_jvm() -> OperationResult<()> {
    let render = Arc::new(AnnotationRenderer);
    for row in rows("annotation")
        .into_iter()
        .filter(|row| row.get("producer").is_none() && row.get("eagerRendering").is_none())
    {
        let kind = row["kind"].as_str().expect("kind");
        let value = match kind {
            "string" => M::String("a)b(value=x)".into()),
            "null" => M::Null,
            "scalar" => M::String("a)b".into()),
            "list" => M::List(vec![M::String("a".into()), M::String("b".into())]),
            "map" => M::Map([("x".into(), M::Int(1)), ("y".into(), M::Null)].into_iter().collect()),
            _ => wrapper(kind),
        };
        let rendered = render.render(&value);
        let expected = match kind {
            "decodedNull" => "null",
            "nested" => "DecodedArrayValue(values=[DecodedString(value=a)b), null])",
            _ => row["rendered"].as_str().expect("rendered"),
        };
        assert_eq!(rendered, expected);
        let extracted = if row.get("extracted").is_some() {
            array_element_value(&rendered)
        } else {
            rendered
        };
        if row.get("extracted").is_some() {
            let expected = if kind == "decodedNull" {
                "null"
            } else {
                row["extracted"].as_str().expect("extracted")
            };
            assert_eq!(extracted, expected);
        }
        // Exact literals here contain metacharacters; use a full-match supported
        // pattern synthesized by escaping every punctuation byte.
        let pattern = escape(&extracted);
        let c = C::AnnotationData {
            name_regex: regex("A"),
            value_regex: regex(&pattern),
            enabled: true,
        };
        let mut dependencies = deps(None, Arc::default());
        dependencies.rendering = render.clone();
        let data = if row.get("extracted").is_some() {
            array(vec![value])
        } else {
            value
        };
        let selected = apply(
            TestFilter::from_config(&c, &dependencies),
            vec![annotated("A", data)],
            false,
        )?;
        assert_eq!(selected.len() as u64, row["selected"].as_u64().expect("selected"));
    }
    Ok(())
}
fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
#[test]
fn values_file_byte_decoding_and_comment_rules_replay_the_jvm() -> OperationResult<()> {
    let native: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/phase8/d57-process-text.json")).unwrap();
    for row in rows("files") {
        if let Some(bytes) = row["bytes"].as_array() {
            let bytes: Vec<u8> = bytes.iter().map(|n| n.as_u64().expect("byte") as u8).collect();
            let expected = if row["bytes"] == native["filter_values"]["bytes"] {
                &native["filter_values"]["values"]
            } else {
                &row["values"]
            };
            assert_eq!(
                &json!(parse_values(&bytes).iter().map(|s| s.to_owned()).collect::<Vec<_>>()),
                expected
            );
        }
    }
    let row = rows("files")
        .into_iter()
        .find(|row| row.get("invalidPath").is_some())
        .expect("invalid File row");
    let tmp = Temp::new()?;
    let logs = Arc::new(Logs::default());
    let reader = marathon_core::filter::values::FileValuesReader { logger: logs.clone() };
    let path = tmp.path(row["invalidPath"].as_str().expect("path"));
    assert_eq!(
        json!(
            reader
                .read_values(&path)?
                .iter()
                .map(|s| s.to_owned())
                .collect::<Vec<_>>()
        ),
        row["values"]
    );
    let records = logs.0.lock().expect("logs");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]
            .message
            .to_owned()
            .replace(tmp.0.to_str().expect("temp UTF-8"), "<tmp>"),
        row["logs"][0]["message"]
    );
    Ok(())
}

type ReadValues = dyn Fn(&std::path::Path) -> OperationResult<Vec<String>> + Send + Sync;
struct Reader(Arc<ReadValues>);
impl FilterValuesReader for Reader {
    fn read_values(&self, path: &std::path::Path) -> OperationResult<Vec<String>> {
        (self.0)(path)
    }
}
struct Supplier(Arc<dyn Fn() -> OperationResult<Option<TestPlan>> + Send + Sync>);
impl AllurePlanSupplier for Supplier {
    fn supply(&self) -> OperationResult<Option<TestPlan>> {
        (self.0)()
    }
}
#[test]
fn missing_values_are_logged_once_and_cached_across_operations() -> OperationResult<()> {
    let tmp = Temp::new()?;
    let path = tmp.path("values");
    let logs = Arc::new(Logs::default());
    let dependencies = deps(None, logs.clone());
    let f = TestFilter::from_config(&file_config(path.clone()), &dependencies);
    let t = vec![test("pkg", "clazz", "method")];
    assert!(apply(f.clone(), vec![], false)?.is_empty());
    assert!(logs.0.lock().expect("logs").is_empty());
    assert!(apply(f.clone(), t.clone(), false)?.is_empty());
    tmp.write("values", "pkg.clazz")?;
    assert_eq!(apply(f.clone(), t.clone(), true)?, t);
    assert!(apply(f, t, false)?.is_empty());
    let records = logs.0.lock().expect("logs");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].level, Level::Error);
    assert_eq!(records[0].logger, "FilterValuesFileReader");
    assert_eq!(
        records[0].message.to_owned(),
        format!(
            "Filtering configuration file {} does not exist. Applying empty list.",
            path.display()
        )
    );
    assert_eq!(records[0].cause, None);
    Ok(())
}
#[test]
fn lazy_files_do_not_read_on_disabled_empty_or_explicit_values_and_retry_errors() -> OperationResult<()> {
    let tmp = Temp::new()?;
    mkdir(tmp.0.join("values"))?;
    let c = file_config(tmp.path("values"));
    let f = filter(&c);
    let t = vec![test("pkg", "clazz", "method")];
    assert!(apply(f.clone(), vec![], false)?.is_empty());
    let error = apply(f.clone(), t.clone(), false);
    let Err(OperationError::Failed(error)) = error else {
        panic!("expected read failure")
    };
    assert_eq!(error.kind, marathon_error::FailureKind::FileNotFound);
    remove_dir(tmp.0.join("values"))?;
    tmp.write("values", "pkg.clazz")?;
    assert_eq!(apply(f.clone(), t.clone(), false)?, t);
    tmp.write("values", "other")?;
    assert_eq!(apply(f, t.clone(), false)?, t);
    let mut disabled = if let C::FullyQualifiedClassname(c) = c {
        c
    } else {
        unreachable!()
    };
    disabled.enabled = false;
    disabled.file = Some(tmp.path("missing"));
    for not in [false, true] {
        assert_eq!(
            apply(filter(&C::FullyQualifiedClassname(disabled.clone())), t.clone(), not)?,
            t
        );
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut dependencies = deps(None, Arc::default());
    dependencies.values = Arc::new(Reader(Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    })));
    let mut c = config(None, Some(&[]), true);
    c.file = Some(tmp.path("missing"));
    let f = TestFilter::from_config(&C::SimpleClassname(c), &dependencies);
    assert!(apply(f, t, false)?.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn composition_laziness_child_order_and_non_short_circuit_are_observable() -> OperationResult<()> {
    let trace = Arc::new(Mutex::new(vec![]));
    let observed = trace.clone();
    let mut dependencies = deps(None, Arc::default());
    dependencies.values = Arc::new(Reader(Arc::new(move |p| {
        observed.lock().expect("trace").push(p.to_string_lossy().into_owned());
        Ok(vec![])
    })));
    let child = |name: &str| {
        let mut c = config(None, None, true);
        c.file = Some(PathBuf::from(name));
        C::SimpleClassname(c)
    };
    let t = vec![test("pkg", "Class", "method")];
    for op in [Op::Union, Op::Intersection, Op::Subtract] {
        let c = C::Composition {
            filters: vec![child("first"), child("second")],
            op,
            enabled: true,
        };
        assert!(apply(TestFilter::from_config(&c, &dependencies), t.clone(), false)?.is_empty());
    }
    // UNION/INTERSECTION evaluate every child on original input. SUBTRACT's
    // empty left result prevents its ordinary second leaf from reading a file.
    assert_eq!(
        *trace.lock().expect("trace"),
        vec!["first", "second", "first", "second", "first"]
    );
    trace.lock().expect("trace").clear();
    for filters in [vec![], vec![child("unreachable")]] {
        let c = C::Composition {
            filters,
            op: Op::Subtract,
            enabled: true,
        };
        assert_eq!(apply(TestFilter::from_config(&c, &dependencies), t.clone(), false)?, t);
    }
    let c = C::Composition {
        filters: vec![child("disabled")],
        op: Op::Union,
        enabled: false,
    };
    assert_eq!(apply(TestFilter::from_config(&c, &dependencies), t.clone(), false)?, t);
    assert!(trace.lock().expect("trace").is_empty());
    let counts = Arc::new(AtomicUsize::new(0));
    let counter = counts.clone();
    dependencies.allure = Arc::new(Supplier(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    })));
    let c = C::Composition {
        filters: vec![
            C::SimpleClassname(config(None, Some(&[]), true)),
            C::Allure { enabled: true },
        ],
        op: Op::Intersection,
        enabled: true,
    };
    assert!(apply(TestFilter::from_config(&c, &dependencies), t, false)?.is_empty());
    assert_eq!(counts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn worker_read_is_off_core_and_the_result_is_joined_with_typed_errors() -> OperationResult<()> {
    let mut dependencies = deps(None, Arc::default());
    let (started, receive) = tokio::sync::oneshot::channel();
    let started = Mutex::new(Some(started));
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    dependencies.values = Arc::new(Reader(Arc::new(move |_| {
        assert_ne!(std::thread::current().name(), Some("marathon-core"));
        if let Some(send) = started.lock().expect("signal").take() {
            let _ = send.send(());
        }
        wait.lock().expect("release").recv().map_err(|e| fail(&e.to_string()))?;
        Ok(vec!["pkg.clazz".into()])
    })));
    let f = TestFilter::from_config(&file_config(PathBuf::from("irrelevant")), &dependencies);
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let vendor = core.vendor();
            let operation = f.filter(vec![test("pkg", "clazz", "method")], vendor.as_ref());
            let unblock = async move {
                receive.await.map_err(|e| fail(&e.to_string()))?;
                assert_eq!(std::thread::current().name(), Some("marathon-core"));
                release.send(()).map_err(|e| fail(&e.to_string()))?;
                Ok::<_, OperationError>(())
            };
            let (result, released) = futures_util::future::join(operation, unblock).await;
            released?;
            assert_eq!(result?.len(), 1);
            Ok(())
        })
    })?;
    let cancellation = Cancellation::Requested(fail("cancelled"));
    let expected = cancellation.clone();
    let mut dependencies = deps(None, Arc::default());
    dependencies.allure = Arc::new(Supplier(Arc::new(move || {
        Err(OperationError::Cancelled(cancellation.clone()))
    })));
    assert_eq!(
        apply(
            TestFilter::from_config(&C::Allure { enabled: true }, &dependencies),
            vec![],
            false
        ),
        Err(OperationError::Cancelled(expected))
    );
    let inner = fail("inner");
    let mut outer = fail("outer");
    outer.cause = Some(Box::new(inner));
    let expected = outer.clone();
    dependencies.allure = Arc::new(Supplier(Arc::new(move || Err(outer.clone().into()))));
    assert_eq!(
        apply(
            TestFilter::from_config(&C::Allure { enabled: true }, &dependencies),
            vec![],
            false
        ),
        Err(OperationError::Failed(expected))
    );
    Ok(())
}

#[test]
fn allure_wrapper_cache_first_annotations_and_c7_replay_the_jvm() -> OperationResult<()> {
    for row in rows("allure").into_iter().filter(|r| r.get("wrapper").is_some()) {
        let plan = match row["wrapper"].as_str().expect("wrapper") {
            "v1" => Some(TestPlan::V1_0(vec![
                PlanTest {
                    id: Some("7".into()),
                    selector: None,
                },
                PlanTest {
                    id: None,
                    selector: Some("pkg.Class.two".into()),
                },
                PlanTest {
                    id: Some("".into()),
                    selector: None,
                },
            ])),
            "unknown" => Some(TestPlan::Unknown),
            _ => None,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut dependencies = deps(None, Arc::default());
        dependencies.allure = Arc::new(Supplier(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(plan.clone())
        })));
        let cnf = C::Allure {
            enabled: row["enabled"].as_bool().expect("enabled"),
        };
        let f = TestFilter::from_config(&cnf, &dependencies);
        let mut input = vec![
            test("pkg", "Class", "one"),
            test("pkg", "Class", "two"),
            test("pkg", "Class", "three"),
            test("pkg", "Class", "four"),
        ];
        input[0]
            .meta_properties
            .push(property("io.qameta.allure.AllureId", M::String("7".into())));
        input[2].meta_properties = vec![
            property("io.qameta.allure.AllureId", M::Int(7)),
            property("io.qameta.allure.kotlin.AllureId", M::String("7".into())),
        ];
        input[3]
            .meta_properties
            .push(property("io.qameta.allure.kotlin.AllureId", M::String("".into())));
        assert_eq!(names(&apply(f.clone(), vec![], false)?), row["empty"]);
        assert_eq!(names(&apply(f.clone(), input.clone(), false)?), row["filter"]);
        assert_eq!(names(&apply(f, input, true)?), row["filterNot"]);
        assert_eq!(
            calls.load(Ordering::SeqCst) as u64,
            row["calls"].as_u64().expect("calls")
        );
    }
    Ok(())
}
#[test]
fn allure_retries_thrown_supply_but_caches_absence_and_unknown() -> OperationResult<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut dependencies = deps(None, Arc::default());
    dependencies.allure = Arc::new(Supplier(Arc::new(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        if n == 0 { Err(fail("retry").into()) } else { Ok(None) }
    })));
    let f = TestFilter::from_config(&C::Allure { enabled: true }, &dependencies);
    assert!(apply(f.clone(), vec![], false).is_err());
    assert!(apply(f.clone(), vec![test("pkg", "Class", "method")], true)?.is_empty());
    assert_eq!(apply(f, vec![test("pkg", "Class", "method")], false)?.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}
struct Env(HashMap<String, String>);
impl PlanEnvironment for Env {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}
#[test]
fn allure_file_supplier_precedence_json_dispatch_and_p2_replay_the_jvm() -> OperationResult<()> {
    // Absolute injected paths avoid mutating the process cwd in concurrent tests;
    // the JVM table separately records process-relative paths in isolated children.
    for row in rows("allure").into_iter().filter(|r| r.get("json").is_some()) {
        let tmp = Temp::new()?;
        tmp.write("plan.json", row["json"].as_str().expect("json"))?;
        let logs = Arc::new(Logs::default());
        let env = row["env"]
            .as_object()
            .expect("env")
            .iter()
            .map(|(k, v)| {
                let text = v.as_str().expect("env text");
                let value = if text.is_empty() {
                    "".into()
                } else {
                    tmp.0.join(text).to_string_lossy().into_owned()
                };
                (k.clone(), value)
            })
            .collect();
        let supplier = FileTestPlanSupplier {
            environment: Arc::new(Env(env)),
            logger: logs.clone(),
        };
        let plan = supplier.supply()?;
        let exotic = matches!(
            row["json"].as_str().expect("json"),
            "{\"version\":\"1.0\"}"
                | "{\"version\":\"1.0\",\"tests\":null}"
                | "{\"version\":\"1.0\",\"tests\":[null]}"
                | "{\"version\":\"1.0\",\"tests\":[{\"id\":7,\"selector\":true}]}"
                | "{\"version\":1.0,\"tests\":[]}"
                | "null"
        );
        if exotic {
            assert_eq!(plan, None);
            assert_eq!(logs.0.lock().expect("logs").len(), 1);
        } else {
            let class = plan.as_ref().map(|p| match p {
                TestPlan::V1_0(_) => "TestPlanV1_0",
                TestPlan::Unknown => "TestPlanUnknown",
            });
            assert_eq!(class, row["output"]["supplied"].as_str());
            if let Some(p @ TestPlan::V1_0(_)) = &plan {
                assert_eq!(
                    json!(p.is_selected(Some("7"), "pkg.Class.method")),
                    row["output"]["selected"]
                );
            }
            assert_eq!(
                logs.0.lock().expect("logs").len(),
                row["output"]["logs"].as_array().expect("logs").len()
            );
        }
        for record in logs.0.lock().expect("logs").iter() {
            assert_eq!(record.level, Level::Warn);
            assert_eq!(record.logger, "io.qameta.allure.testfilter.FileTestPlanSupplier");
            assert_eq!(
                record.message.to_owned(),
                format!("could not read test plan file {}", tmp.0.join("plan.json").display())
            );
            assert!(record.cause.is_some());
        }
    }
    let logs = Arc::new(Logs::default());
    let supplier = FileTestPlanSupplier {
        environment: Arc::new(Env([("ALLURE_TESTPLAN_PATH".into(), "bad\0path".into())]
            .into_iter()
            .collect())),
        logger: logs.clone(),
    };
    assert_eq!(supplier.supply()?, None);
    let records = logs.0.lock().expect("logs");
    assert_eq!(
        records[0].message,
        String::from("could not read test plan file bad\0path: invalid path")
    );
    assert_eq!(records[0].cause, None);
    Ok(())
}
#[test]
fn p2_plans_jackson_accepts_warn_and_become_absent() -> OperationResult<()> {
    // Allure 2.29.0 supplies V1 for each (probe in docs/wp/3.3-findings.md, P2);
    // a duplicate `version` must never silently become Unknown.
    for json in [
        r#"{"version":"1.0","tests":[{"id":"7"}]} trailing"#,
        r#"{"version":"1.0","tests":[{"id":"\ud800x","selector":"s"}]}"#,
        r#"{"version":"1.0","tests":[{"id":"7"}],"version":"2"}"#,
        r#"{"version":"1.0","tests":[{"id":"7","id":"8"}]}"#,
    ] {
        let tmp = Temp::new()?;
        tmp.write("plan.json", json)?;
        let logs = Arc::new(Logs::default());
        let path = tmp.0.join("plan.json").to_string_lossy().into_owned();
        let supplier = FileTestPlanSupplier {
            environment: Arc::new(Env([("ALLURE_TESTPLAN_PATH".into(), path)].into_iter().collect())),
            logger: logs.clone(),
        };
        if json.contains("\\ud800") {
            assert_eq!(
                supplier.supply()?,
                Some(TestPlan::V1_0(vec![PlanTest {
                    id: Some("�x".into()),
                    selector: Some("s".into()),
                }]))
            );
            assert!(logs.0.lock().expect("logs").is_empty());
            continue;
        }
        assert_eq!(supplier.supply()?, None, "{json}");
        let records = logs.0.lock().expect("logs");
        assert_eq!(records.len(), 1, "{json}");
        assert_eq!(records[0].level, Level::Warn);
        assert!(records[0].cause.is_some());
    }
    Ok(())
}
#[test]
fn annotation_arrays_render_values_and_filters_match_nested_contents() -> OperationResult<()> {
    let rendering = AnnotationRenderer;
    let values = array(vec![
        M::String("a".into()),
        M::Decoded(D::DecodedNull),
        array(vec![M::Int(7)]),
        M::List(vec![M::Boolean(true)]),
    ]);
    assert_eq!(rendering.render(&values), "[a, null, [7], [true]]");
    assert_eq!(rendering.render(&array(vec![])), "[]");
    assert_eq!(rendering.render(&values.clone()), rendering.render(&values));
    let null_filter = C::AnnotationData {
        name_regex: regex("A"),
        value_regex: regex("null"),
        enabled: true,
    };
    let nulls = vec![annotated("A", M::Null), annotated("A", M::Decoded(D::DecodedNull))];
    let selected = apply(filter(&null_filter), nulls.clone(), false)?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].meta_properties, nulls[1].meta_properties);

    for (pattern, count) in [
        (r"\[7\]", 1),
        ("null", 1),
        (r"\[Ljava.*@.*", 0),
        (".*DecodedNull@.*", 0),
    ] {
        let c = C::AnnotationData {
            name_regex: regex("A"),
            value_regex: regex(pattern),
            enabled: true,
        };
        let input = vec![annotated("A", values.clone())];
        assert_eq!(apply(filter(&c), input.clone(), false)?.len(), count);
        assert_eq!(apply(filter(&c), input, true)?.len(), 1 - count);
    }
    Ok(())
}
#[test]
fn normalized_surrogates_match_regex_and_literal_filters() -> OperationResult<()> {
    let mut t = test("pkg", "Class", "method");
    t.clazz = String::from_utf16_lossy(&[0xd800]);
    let c = C::SimpleClassname(config(Some(".*"), None, true));
    check(c, &[t.clone()], false, &[t.clone()])?;
    let mut c = config(None, None, true);
    c.values = Some(vec![t.clazz.clone()]);
    check(C::SimpleClassname(c), &[t.clone()], false, &[t])?;
    Ok(())
}

#[test]
fn d6_lookaround_is_rejected_at_binding_with_field_and_pattern() {
    use marathon_config::{
        bind::bind_test_filter_configuration,
        binder::Binder,
        binding::{BindingContext, Profile},
        node,
    };
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: jcompat::jtime::parse_instant("2026-10-12T00:00:00Z").expect("instant"),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let row = rows("leaves")
        .into_iter()
        .find(|row| row.get("d6").is_some())
        .expect("D6 row");
    let pattern = row["d6"].as_str().expect("pattern");
    let text = format!("type: simple-class-name\nregex: '{pattern}'\n");
    let stream = node::load(&text);
    let binder = Binder::new(&context, &stream);
    let result = binder.first(&stream, bind_test_filter_configuration);
    let error = result.expect_err("D6 rejects lookaround at bind");
    let message = error.message.expect("message").to_owned();
    assert!(message.contains(pattern));
    assert!(message.contains("regex"));
}

#[test]
fn enabled_allure_children_are_supplied_even_for_empty_compositions() -> OperationResult<()> {
    for row in rows("allure")
        .into_iter()
        .filter(|row| row.get("compositionOp").is_some())
    {
        let op = match row["compositionOp"].as_str().expect("op") {
            "UNION" => Op::Union,
            "INTERSECTION" => Op::Intersection,
            _ => Op::Subtract,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut dependencies = deps(None, Arc::default());
        dependencies.allure = Arc::new(Supplier(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })));
        let config = C::Composition {
            filters: vec![C::Allure { enabled: true }; row["children"].as_u64().expect("children") as usize],
            op,
            enabled: true,
        };
        let f = TestFilter::from_config(&config, &dependencies);
        assert_eq!(names(&apply(f, vec![], false)?), row["filter"]);
        assert_eq!(
            calls.load(Ordering::SeqCst) as u64,
            row["calls"].as_u64().expect("calls")
        );
    }
    for enabled in [true, false] {
        let c = C::Composition {
            filters: vec![C::Allure { enabled }],
            op: Op::Union,
            enabled: true,
        };
        assert!(apply(filter(&c), vec![test("pkg", "Class", "method")], true)?.is_empty());
    }
    Ok(())
}

#[test]
fn allure_process_child() -> OperationResult<()> {
    if std::env::var_os("MRS_FILTER_PROCESS_CHILD").is_none() {
        return Ok(());
    }
    let supplier = FileTestPlanSupplier {
        environment: Arc::new(marathon_core::filter::allure::ProcessEnvironment),
        logger: Arc::new(Logs::default()),
    };
    let plan = supplier.supply()?.expect("relative plan exists");
    assert!(plan.is_selected(Some("7"), "anything"));
    Ok(())
}
#[test]
fn allure_relative_paths_use_process_cwd_and_environment_fallback() -> OperationResult<()> {
    let temp = Temp::new()?;
    temp.write("plan.json", r#"{"version":"1.0","tests":[{"id":"7"}]}"#)?;
    let executable = std::env::current_exe().map_err(|e| fail(&e.to_string()))?;
    let output = std::process::Command::new(executable)
        .args(["--exact", "allure_process_child", "--nocapture"])
        .current_dir(&temp.0)
        .env_remove("ALLURE_TESTPLAN_PATH")
        .env("AS_TESTPLAN_PATH", "plan.json")
        .env("MRS_FILTER_PROCESS_CHILD", "1")
        .output()
        .map_err(|e| fail(&e.to_string()))?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Ok(())
}

#[test]
fn annotation_data_disabled_null_and_scalar_types_keep_kotlin_semantics() -> OperationResult<()> {
    let input = vec![
        annotated("A", M::Null),
        annotated("A", M::Int(7)),
        annotated("A", M::List(vec![M::Int(7)])),
        annotated("A", M::String("7".into())),
    ];
    let c = C::AnnotationData {
        name_regex: regex("A"),
        value_regex: regex("7"),
        enabled: true,
    };
    let selected = apply(filter(&c), input.clone(), false)?;
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].meta_properties, input[1].meta_properties);
    assert_eq!(selected[1].meta_properties, input[3].meta_properties);
    for not in [false, true] {
        let c = C::AnnotationData {
            name_regex: regex("A"),
            value_regex: regex("7"),
            enabled: false,
        };
        let selected = apply(filter(&c), input.clone(), not)?;
        assert_eq!(selected.len(), input.len());
        for (a, b) in selected.iter().zip(&input) {
            assert_eq!(a.meta_properties, b.meta_properties);
        }
    }
    Ok(())
}

#[test]
fn annotation_arrays_match_elements_with_decoded_null() -> OperationResult<()> {
    let rendering = Arc::new(AnnotationRenderer);
    let row = rows("annotation")
        .into_iter()
        .find(|row| row.get("eagerRendering").is_some())
        .expect("eager row");
    assert_eq!(row["eagerRendering"], json!(["match", "later"]));
    let mut dependencies = deps(None, Arc::default());
    dependencies.rendering = rendering.clone();
    let c = C::AnnotationData {
        name_regex: regex("A"),
        value_regex: regex("match"),
        enabled: true,
    };
    let f = TestFilter::from_config(&c, &dependencies);
    let selected = apply(
        f,
        vec![annotated(
            "A",
            array(vec![M::String("match".into()), M::Decoded(D::DecodedNull)]),
        )],
        false,
    )?;
    assert_eq!(selected.len() as u64, row["selected"].as_u64().expect("selected"));
    Ok(())
}
