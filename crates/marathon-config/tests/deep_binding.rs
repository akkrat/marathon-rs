//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! A subprocess keeps stack-overflow regressions from aborting the test suite.
use jcompat::file_manager::JavaPath;
use marathon_config::{
    binder::{Field, Result, Value},
    binding::{BindingContext, Profile},
    pipeline::{BIND_STACK_SIZE, bind_bytes},
};
use serde_json::{Value as Json, json};
use std::sync::Mutex;

fn context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: JavaPath::from_text("/Marathonfile"),
        marathonfile_dir: JavaPath::from_text("/"),
        process_dir: JavaPath::from_text("/"),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}
// Use the composition subtype fields from the review consumer; returning a
// depth avoids recursive output serialization.
#[inline(never)]
fn deep_filter(v: Value<'_>, frames: &Mutex<Vec<usize>>) -> Result<Option<usize>> {
    let marker = 0u8;
    frames.lock().unwrap().push(std::ptr::from_ref(&marker) as usize);
    let result = v.polymorphic(|id, v| {
        if !matches!(id, "composition" | "allure") {
            return None;
        }
        Some((|| {
            let mut enabled = Field::primitive(&v, true);
            let mut filters = Field::reference(&v, None);
            let mut op = Field::reference(&v, None);
            let mut o = v.object()?;
            while let Some((name, child)) = o.next_field()? {
                match name.as_str() {
                    "enabled" => {
                        let x = child.boolean()?;
                        enabled.set(child, Some(x));
                    }
                    "op" if id == "composition" => {
                        let x = child.enumeration(&["UNION", "INTERSECTION", "SUBTRACT"])?;
                        op.set(child, x);
                    }
                    "filters" if id == "composition" => {
                        let x = child.list(|v| deep_filter(v, frames))?;
                        filters.set(child, x);
                    }
                    _ => child.unknown(false)?,
                }
            }
            enabled.required("enabled")?;
            if id == "allure" {
                return Ok(0);
            }
            op.required("op")?;
            Ok(1 + filters.required("filters")?.into_iter().flatten().max().unwrap_or(0))
        })())
    });
    std::hint::black_box(&marker);
    result
}
#[test]
fn maximum_depth_binding_in_subprocess() -> std::result::Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "MRS_BIND_DEPTH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "maximum_depth_binding_in_subprocess", "--nocapture"])
            .env(CHILD, "1")
            .output()?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        return Ok(());
    }
    let row: Json = include_str!("../../../fixtures/oracle/yaml-binding-review-corpus.jsonl")
        .lines()
        .skip(1)
        .map(serde_json::from_str::<Json>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .find(|r| r["target"] == "DeepFilters")
        .ok_or("depth witness")?;
    let count = row["input"].as_str().ok_or("depth")?.parse::<usize>()?;
    assert_eq!(2 + 2 * count, marathon_config::tokens::MAX_DEPTH);
    let filter = format!(
        "{}{{type: allure}}{}",
        "{type: composition, op: UNION, filters: [".repeat(count),
        "]}".repeat(count)
    );
    let frames = Mutex::new(Vec::new());
    // This caller has the normal small stack of a worker; bind_bytes owns the larger stack.
    let got = bind_bytes(format!("[{filter}]").as_bytes(), &context(), |v| {
        v.list(|v| deep_filter(v, &frames))
    })
    .map_err(|e| format!("{e:?}"))?
    .ok_or("list")?;
    assert_eq!(json!(got[0]), row["result"]["ok"]);
    let frames = frames.lock().unwrap();
    let max_frame = frames.windows(2).map(|w| w[0].abs_diff(w[1])).max().ok_or("frames")?;
    let measured = frames[0].abs_diff(*frames.last().ok_or("leaf frame")?);
    println!(
        "debug={} max per composition level={max_frame} bytes, depth={} measured={measured} bytes, worker stack={BIND_STACK_SIZE} bytes",
        cfg!(debug_assertions),
        count
    );
    assert!(
        max_frame * marathon_config::tokens::MAX_DEPTH * 4 < BIND_STACK_SIZE,
        "binding worker needs at least 4x measured debug frame margin"
    );
    Ok(())
}
