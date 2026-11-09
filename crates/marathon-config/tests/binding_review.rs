//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! Test-only consumers of the shared binder, backed by the review JVM corpus.
use marathon_config::{
    binder::{Binder, Field, MISSING_KOTLIN, Result, Value},
    binding::{BindingContext, Profile},
    node,
};
use serde_json::{Value as Json, json};
use std::path::PathBuf;

fn context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}
fn float(value: f64) -> Json {
    if value.is_finite() {
        json!(value)
    } else {
        json!(jcompat::jnum::double_to_string(value))
    }
}
fn string(value: String) -> Json {
    json!(value.to_owned())
}

fn bean(v: Value<'_>) -> Result<Option<Json>> {
    v.nullable(|v| {
        let mut k = Field::reference(&v, Some("default".into()));
        let mut o = v.object()?;
        while let Some((name, child)) = o.next_field()? {
            match name.as_str() {
                "k" => {
                    let x = child.string()?;
                    k.set(child, x);
                }
                _ => child.unknown(false)?,
            }
        }
        Ok(json!({"k":string(k.required("k")?)}))
    })
}
fn defaults(v: Value<'_>) -> Result<Json> {
    let mut primitive = Field::primitive(&v, 7);
    let mut reference = Field::reference(&v, Some("default".into()));
    let mut nullable = Field::nullable(&v, Some(9));
    let mut o = v.object()?;
    while let Some((name, child)) = o.next_field()? {
        match name.as_str() {
            "primitive" => {
                let x = child.nullable(|v| v.int())?;
                primitive.set(child, x);
            }
            "reference" => {
                let x = child.string()?;
                reference.set(child, x);
            }
            "nullable" => {
                let x = child.nullable(|v| v.int())?;
                nullable.set(child, x);
            }
            _ => child.unknown(false)?,
        }
    }
    Ok(
        json!({"primitive":primitive.required("primitive")?, "reference":string(reference.required("reference")?), "nullable":nullable.optional()}),
    )
}
fn simple_polymorphic(v: Value<'_>, target: &str) -> Result<Option<Json>> {
    v.polymorphic(|id, v| {
        let accepted = match target {
            "Vendor" | "VendorField" => matches!(id, "stub" | "empty"),
            "Parser" => matches!(id, "local" | "remote"),
            "Filtering" => matches!(id, "method" | "allure"),
            _ => false,
        };
        if !accepted {
            return None;
        }
        Some((|| {
            let mut regex = Field::nullable(&v, None);
            let mut args = Field::reference(&v, Some(Vec::new()));
            let mut o = v.object()?;
            while let Some((name, child)) = o.next_field()? {
                match (id, name.as_str()) {
                    ("method", "regex") => {
                        let x = child.regex()?;
                        regex.set(child, x);
                    }
                    ("remote", "instrumentationArgs") => {
                        let x = child.map(|v| v.string())?;
                        args.set(child, x);
                    }
                    _ => child.unknown(false)?,
                }
            }
            let mut out = json!({"type":id});
            if id == "method" {
                out["regex"] = json!(regex.optional().map(|r| r.pattern().to_owned()));
                out["enabled"] = json!(true);
                out["values"] = Json::Null;
                out["file"] = Json::Null;
            }
            if id == "remote" {
                out["instrumentationArgs"] = Json::Object(
                    args.required("instrumentationArgs")?
                        .into_iter()
                        .map(|(k, v)| (k, v.map_or(Json::Null, string)))
                        .collect(),
                );
            }
            Ok(out)
        })())
    })
}
fn nested(v: Value<'_>, target: &str) -> Result<Json> {
    let name = match target {
        "Nested" => "bean",
        "VendorField" => "vendorConfiguration",
        _ => "executionStrategy",
    };
    let mut field = Field::reference(&v, None);
    let mut o = v.object()?;
    while let Some((key, child)) = o.next_field()? {
        if key == name {
            let value = if target == "Nested" {
                bean(child.clone())?
            } else {
                simple_polymorphic(child.clone(), target)?
            };
            field.set(child, value);
        } else {
            child.unknown(false)?;
        }
    }
    Ok(json!({name:field.required(name)?}))
}
fn filtering(v: Value<'_>) -> Result<Json> {
    let mut allowlist = Field::reference(&v, Some(Vec::new()));
    let mut blocklist = Field::reference(&v, Some(Vec::new()));
    let mut o = v.object()?;
    while let Some((key, child)) = o.next_field()? {
        match key.as_str() {
            "allowlist" | "blocklist" => {
                let value = child.list(|item| simple_polymorphic(item, "Filtering"))?;
                if key == "allowlist" {
                    allowlist.set(child, value);
                } else {
                    blocklist.set(child, value);
                }
            }
            _ => child.unknown(false)?,
        }
    }
    Ok(json!({"allowlist":allowlist.required("allowlist")?,"blocklist":blocklist.required("blocklist")?}))
}
fn batching(v: Value<'_>) -> Result<Option<Json>> {
    v.polymorphic(|id, v| (id == "fixed-size").then(|| {
        let tree = v.read_tree()?;
        let size = tree.find_value("size")?.ok_or_else(|| v.error(MISSING_KOTLIN, "size"))?.as_int();
        let percentile = tree.find_value("percentile")?.map(|v| v.as_double());
        Ok(json!({"type":id,"size":size,"percentile":percentile,"durationMillis":null,"timeLimit":null,"lastMileLength":0}))
    }))
}
fn schema(v: Value<'_>, target: &str) -> Result<Json> {
    match target {
        "Accessors" => {
            let tree = v.read_tree()?;
            let child = tree.get("k")?.ok_or_else(|| v.error(MISSING_KOTLIN, "k"))?;
            Ok(
                json!({"text":string(child.as_text()),"int":child.as_int(),"long":child.as_long(),"double":float(child.as_double()),"boolean":child.as_boolean()}),
            )
        }
        "Lookup" => {
            let tree = v.read_tree()?;
            Ok(
                json!({"get":tree.get("k")?.map(|v|string(v.as_text())), "find":tree.find_value("k")?.map(|v|string(v.as_text()))}),
            )
        }
        "SubtypeTree" => Ok(json!(v.polymorphic(|id, v| (id == "tree").then(|| {
            let tree = v.read_tree()?;
            let mut object = tree.value().object()?;
            let mut properties = Vec::new();
            while let Some((key, child)) = object.next_field()? {
                properties.push(format!("{key}={}", child.as_text().to_owned()));
            }
            assert_eq!(
                v.get("type")?.map(|v| v.as_text()),
                tree.get("type")?.map(|v| v.as_text())
            );
            assert_eq!(
                v.find_value("type")?.map(|v| v.as_text()),
                tree.find_value("type")?.map(|v| v.as_text())
            );
            Ok(json!({"type":id,"properties":properties}))
        }))?)),
        "Defaults" => defaults(v),
        "Nested" | "VendorField" | "ExecutionField" => nested(v, target),
        "Filtering" => filtering(v),
        "Vendor" | "Parser" => Ok(json!(simple_polymorphic(v, target)?)),
        "Regex" => {
            let mut field = Field::nullable(&v, None);
            let mut o = v.object()?;
            while let Some((key, child)) = o.next_field()? {
                if key == "k" {
                    let x = child.regex()?;
                    field.set(child, x);
                } else {
                    child.unknown(false)?;
                }
            }
            Ok(json!({"k":field.optional().map(|r|r.pattern().to_owned())}))
        }
        "P2File" => Ok(json!(v.file()?.map(|p| p.to_string_lossy().into_owned()))),
        "SerialStrategy" | "AndroidRecorder" | "IosRecorder" => Ok(json!(v.enum_creator(|s| {
            let names: &[&str] = if target == "SerialStrategy" {
                &["AUTOMATIC", "MARATHON_PROPERTY", "BOOT_PROPERTY", "HOSTNAME", "DDMS"]
            } else {
                &["VIDEO", "SCREENSHOT"]
            };
            names.iter().find(|n| n.eq_ignore_ascii_case(s)).map(|n| n.to_string())
        })?)),
        "Batching" => Ok(json!(batching(v)?)),
        _ => panic!("unhandled corpus target: {target}"),
    }
}
#[test]
fn review_round_one_matches_jvm() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut rows = 0;
    for line in include_str!("../../../fixtures/oracle/yaml-binding-review-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: Json = serde_json::from_str(line)?;
        let target = row["target"].as_str().ok_or("target")?;
        if target == "DeepFilters" {
            continue;
        }
        rows += 1;
        let input = row["input"].as_str().ok_or("input")?;
        let stream = node::load(input);
        let mut context = context();
        if target == "P2File" {
            context.profile = Profile::Marathondevices;
        }
        let b = Binder::new(&context, &stream);
        let got = b.first(&stream, |v| schema(v, target));
        let got = match got {
            Ok(value) => json!({"ok":value}),
            Err(error) => {
                json!({"error": std::iter::once(&error.class).chain(error.causes.iter().map(|c|&c.class)).map(String::clone).collect::<Vec<_>>()})
            }
        };
        assert_eq!(got, row["result"], "{row}");
    }
    println!("review binding corpus: {rows}/{rows} plus maximum-depth subprocess");
    Ok(())
}
