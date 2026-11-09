//! Corpus-only projections. Every binding operation calls the production binder.
use jcompat::jtime::instant_of;
use marathon_config::{
    binder::{Binder, Field, Result, Value},
    binding::{BindingContext, Profile},
};
use serde_json::Value as Json;
use snakeyaml::node::Stream;
use std::path::PathBuf;
/// The thirteen simple targets of the spike rows, in corpus order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    StringOpt,
    String,
    Int,
    Long,
    Double,
    Boolean,
    Enum,
    ListOpt,
    List,
    Map,
    Instant,
    Duration,
    File,
}

impl Target {
    pub const ALL: [(&'static str, Target); 13] = [
        ("String?", Target::StringOpt),
        ("String", Target::String),
        ("Int", Target::Int),
        ("Long", Target::Long),
        ("Double", Target::Double),
        ("Boolean", Target::Boolean),
        ("Enum?", Target::Enum),
        ("List<String>?", Target::ListOpt),
        ("List<String>", Target::List),
        ("Map<String,String>?", Target::Map),
        ("Instant?", Target::Instant),
        ("Duration?", Target::Duration),
        ("File?", Target::File),
    ];

    /// The value when `k` is absent: the bean defaults (see `YamlCorpus.kt`).
    fn default(self) -> serde_json::Value {
        match self {
            Target::StringOpt => "default".into(),
            Target::String => "default".into(),
            Target::Int => 7.into(),
            Target::Long => 7.into(),
            Target::Double => json_float(7.0),
            Target::Boolean => true.into(),
            Target::Enum => serde_json::Value::Null,
            Target::ListOpt => serde_json::Value::Null,
            Target::List => serde_json::Value::Array(vec!["default".into()]),
            Target::Map => serde_json::Value::Null,
            Target::Instant => serde_json::Value::Null,
            Target::Duration => serde_json::Value::Null,
            Target::File => serde_json::Value::Null,
        }
    }
}

fn json_float(f: f64) -> serde_json::Value {
    if f.is_finite() {
        serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, serde_json::Value::Number)
    } else if f.is_nan() {
        "NaN".into()
    } else if f > 0.0 {
        "Infinity".into()
    } else {
        "-Infinity".into()
    }
}

/// The binding outcome for one target: the canonical `ok` JSON value, the
/// null document, or the exception class.
#[derive(Debug)]
pub enum Outcome {
    Ok(serde_json::Value),
    NullDocument,
    Err(Box<marathon_config::binding::ConfigurationError>),
}

/// The oracle's `shorten`: strings over 256 UTF-16 units become
/// `<N chars, sha256 HEX>`, applied recursively to `ok` values.
fn shorten(v: serde_json::Value, target: Target) -> serde_json::Value {
    // The target supplies the value's role: a user map is never a string envelope.
    match (target, v) {
        (Target::Map, serde_json::Value::Object(map)) => {
            serde_json::Value::Object(map.into_iter().map(|(k, v)| (k, shorten_string(v))).collect())
        }
        (Target::List | Target::ListOpt, serde_json::Value::Array(items)) => {
            serde_json::Value::Array(items.into_iter().map(shorten_string).collect())
        }
        (_, v) => shorten_string(v),
    }
}

fn shorten_string(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            let len = s.encode_utf16().count();
            if len <= 256 {
                return serde_json::Value::String(s);
            }
            let hex: String = {
                use sha2::Digest;
                let mut h = sha2::Sha256::new();
                h.update(s.as_bytes());
                h.finalize().iter().map(|b| format!("{b:02x}")).collect()
            };
            serde_json::Value::String(format!("<{len} chars, sha256 {hex}>"))
        }
        serde_json::Value::Object(ref map) => {
            let units: Option<Vec<u16>> = map.get("$utf16").and_then(|v| v.as_array()).and_then(|a| {
                a.iter()
                    .map(|v| v.as_u64().and_then(|n| u16::try_from(n).ok()))
                    .collect()
            });
            match units.filter(|_| map.len() == 1) {
                Some(units) => super::text::short_json(&String::from_utf16_lossy(&units), 256),
                None => v,
            }
        }
        other => other,
    }
}

pub fn context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}
fn string(v: Option<String>) -> Json {
    v.map_or(Json::Null, |s| super::text::json(&s))
}
fn schema(v: Value<'_>, target: Target) -> Result<Json> {
    let default = target.default();
    let mut field = Field::reference(&v, (!default.is_null()).then_some(default));
    let mut object = v.object()?;
    while let Some((name, value)) = object.next_field()? {
        if name != "k" {
            value.unknown(true)?;
            continue;
        }
        let bound = match target {
            Target::String | Target::StringOpt => string(value.string()?),
            Target::Int => value.int()?.into(),
            Target::Long => value.long()?.into(),
            Target::Double => json_float(value.double()?),
            Target::Boolean => value.boolean()?.into(),
            Target::Enum => value
                .enumeration(&["RED", "GREEN", "BLUE"])?
                .map_or(Json::Null, |i| ["RED", "GREEN", "BLUE"][i].into()),
            Target::Instant => value.instant()?.map_or(Json::Null, |i| i.to_string().into()),
            Target::Duration => value.duration()?.map_or(Json::Null, |i| i.to_string().into()),
            Target::File => value
                .file()?
                .map_or(Json::Null, |p| p.to_string_lossy().into_owned().into()),
            Target::List | Target::ListOpt => value.list(|v| Ok(string(v.string()?)))?.map_or(Json::Null, Json::Array),
            Target::Map => value
                .map(|v| Ok(string(v.string()?)))?
                .map_or(Json::Null, |pairs| Json::Object(pairs.into_iter().collect())),
        };
        field.set(value, (!bound.is_null()).then_some(bound));
    }
    if matches!(target, Target::String | Target::List) {
        field.required("k")
    } else {
        Ok(field.optional().unwrap_or(Json::Null))
    }
}
pub fn bind_doc(stream: &Stream, target: Target) -> Outcome {
    let context = context();
    let binder = Binder::new(&context, stream);
    match binder.first(stream, |v| schema(v, target)) {
        Ok(Some(v)) => Outcome::Ok(shorten(v, target)),
        Ok(None) => Outcome::NullDocument,
        Err(e) => Outcome::Err(e),
    }
}
pub fn compare_row(stream: &Stream, expected: &Json, overrides: &Json) -> Vec<(String, String)> {
    let mut diffs = Vec::new();
    for (name, target) in Target::ALL {
        let got = bind_doc(stream, target);
        let exp = &expected[name];
        let ok = if let Some(rule) = overrides.get(name).and_then(Json::as_str) {
            match rule {
                "reject" => {
                    matches!(got, Outcome::Err(ref e) if e.stage == marathon_config::binding::ValidationStage::Parse)
                }
                "error-class" => matches!(got, Outcome::Err(_)) && exp.get("error").is_some(),
                "last-list" => matches!(got, Outcome::Ok(ref v) if v == &serde_json::json!(["x"])),
                "last-null" => {
                    matches!(got, Outcome::Err(ref e) if e.kind.legacy_class() == marathon_config::binder::MISSING_KOTLIN.legacy_class())
                }
                "last-string" => matches!(got, Outcome::Ok(ref v) if v == "x"),
                "syntax-before-null" => {
                    matches!(got, Outcome::Err(ref e) if matches!(e.cause, marathon_config::binding::BindingCause::Stream(_)))
                }
                _ => false,
            }
        } else {
            match &got {
                Outcome::Ok(v) => exp.get("ok") == Some(v),
                Outcome::NullDocument => exp["nullDocument"] == true,
                Outcome::Err(e) => exp["error"]["class"]
                    .as_str()
                    .is_some_and(|s| e.kind.legacy_class() == s),
            }
        };
        if !ok {
            diffs.push((
                name.to_owned(),
                format!(
                    "expected {exp} got {}",
                    match &got {
                        Outcome::Err(e) => format!("error {} {:?}", e.kind.legacy_class().to_owned(), e.path),
                        _ => format!("{got:?}"),
                    }
                ),
            ));
        }
    }
    diffs
}
