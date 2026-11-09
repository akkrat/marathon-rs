//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! Test-only schemas exercise the binding API without taking WP 2.2's schema ownership.
use jcompat::JavaRegex;
use marathon_config::{
    binder::{self, Binder, Field, Result, Value},
    binding::{BindingCause, BindingContext, PathPart, Profile},
    node,
    pipeline::{LoadError, bind_bytes},
};
use serde_json::{Value as Json, json};
use std::path::PathBuf;

fn context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/config/Marathonfile"),
        marathonfile_dir: PathBuf::from("/config"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}
fn bind<T>(text: &str, schema: impl FnOnce(Value<'_>) -> Result<T>) -> Result<Option<T>> {
    let stream = node::load(text);
    let context = context();
    let binder = Binder::new(&context, &stream);
    binder.first(&stream, schema)
}
fn reference_schema(v: Value<'_>, ignore: bool) -> Result<String> {
    let mut k = Field::reference(&v, Some("default".into()));
    let mut object = v.object()?;
    while let Some((name, value)) = object.next_field()? {
        match name.as_str() {
            "k" => {
                let text = value.string()?;
                k.set(value, text);
            }
            _ => value.unknown(ignore)?,
        }
    }
    k.required("k")
}
#[test]
fn kotlin_creator_oracle_and_regex_exception_chain() -> std::result::Result<(), Box<dyn std::error::Error>> {
    for line in include_str!("../../../fixtures/oracle/yaml-binding-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: Json = serde_json::from_str(line)?;
        let target = row["target"].as_str().ok_or("target")?;
        if target.starts_with("Tree") {
            continue;
        }
        let got = bind(row["input"].as_str().ok_or("input")?, |v| {
            let mut field = Field::reference(&v, if target == "RequiredInt" { Some(json!(0)) } else { None });
            let mut object = v.object()?;
            while let Some((name, value)) = object.next_field()? {
                if name != "k" {
                    value.unknown(false)?;
                    continue;
                }
                let item = match target {
                    "RequiredInt" => Some(json!(value.int()?)),
                    "RequiredString" => value.string()?.map(|s| json!(s)),
                    "RequiredList" => value.list(|v| v.string())?.map(|v| json!(v)),
                    "RequiredMap" => value
                        .map(|v| v.string())?
                        .map(|v| Json::Object(v.into_iter().map(|(k, v)| (k, json!(v))).collect())),
                    "Regex" => value.regex()?.map(|r| json!(r.pattern())),
                    _ => unreachable!(),
                };
                field.set(value, item);
            }
            let k = if target == "Regex" {
                field.optional().unwrap_or(Json::Null)
            } else {
                field.required("k")?
            };
            Ok(json!({"k":k}))
        });
        match got {
            Ok(v) => assert_eq!(json!({"ok":v}), row["result"], "{row}"),
            Err(e) => {
                let classes: Vec<_> = std::iter::once(&e.class)
                    .chain(e.causes.iter().map(|c| &c.class))
                    .map(String::clone)
                    .collect();
                assert_eq!(json!({"error":classes}), row["result"], "{row}");
            }
        }
    }
    Ok(())
}
#[test]
fn defaults_nullability_duplicates_and_unknown_keys() {
    assert_eq!(
        bind("{}", |v| reference_schema(v, false)).unwrap(),
        Some("default".into())
    );
    assert_eq!(
        bind("k: null\nk: x", |v| reference_schema(v, false)).unwrap(),
        Some("x".into())
    );
    let e = bind("k: x\nk: null", |v| reference_schema(v, false)).unwrap_err();
    assert_eq!(e.class, binder::MISSING_KOTLIN.to_owned());
    assert!(matches!(&e.path[..], [PathPart::Key(k)] if k == &"k".to_owned()));
    assert_eq!(e.span.unwrap().start.line, 2);
    assert_eq!(
        bind("<<: *a", |v| reference_schema(v, false)).unwrap_err().class,
        binder::UNKNOWN_PROPERTY.to_owned()
    );
    assert!(bind("unknown: [", |v| reference_schema(v, true)).is_err());
    assert_eq!(
        bind("k: *missing", |v| reference_schema(v, false)).unwrap(),
        Some("missing".into())
    );
}
#[test]
fn custom_deserializer_reads_tree_prefers_direct_child_and_binds_subtree() {
    let got = bind("first: {nested: {k: 7}}\nk: 99\nunknown: x", |v| {
        let tree = v.read_tree()?;
        let found = tree
            .find_value("k")?
            .ok_or_else(|| v.error(binder::MISSING_KOTLIN, "k"))?;
        found.bind(|v| v.int())
    })
    .unwrap();
    assert_eq!(got, Some(99));
    assert!(bind("first: {k: 7}\nunknown: [", |v| v.read_tree().map(|_| ())).is_err());
}
#[test]
fn nested_paths_and_stream_causes_survive() {
    let e = bind("outer: {items: [1, {bad: 2}]}", |v| {
        let mut o = v.object()?;
        let (_, child) = o.next_field()?.unwrap();
        let mut inner = child.object()?;
        let (_, items) = inner.next_field()?.unwrap();
        items.list(|v| v.string())
    })
    .unwrap_err();
    assert!(
        matches!(&e.path[..], [PathPart::Key(a), PathPart::Key(b), PathPart::Index(1)] if a == &"outer".to_owned() && b == &"items".to_owned())
    );
    let e = bind("k: [1,\t2]", |v| {
        let mut o = v.object()?;
        let (_, items) = o.next_field()?.unwrap();
        items.list(|v| v.string())
    })
    .unwrap_err();
    assert!(matches!(e.cause, BindingCause::Stream(_)));
    assert_eq!(e.class, binder::JSON_MAPPING.to_owned());
    assert_eq!(e.causes.len(), 2);
    let e = bind("k: {a:\tb}", |v| {
        let mut object = v.object()?;
        let (_, child) = object.next_field()?.unwrap();
        child.map(|v| v.string())
    })
    .unwrap_err();
    assert_eq!(e.class, binder::JSON_MAPPING.to_owned());
    assert_eq!(e.causes.len(), 2);
}
#[test]
fn first_document_and_substitution_failure_stages() {
    let mut ctx = context();
    ctx.environment = vec![("N".into(), "hello".into())];
    let got = bind_bytes(b"k: ${N}\n---\nbroken: [", &ctx, |v| reference_schema(v, false)).unwrap();
    assert_eq!(got, "hello".to_owned());
    assert!(matches!(
        bind_bytes(b"---\n---\nk: x", &ctx, |v| reference_schema(v, false)),
        Err(LoadError::NullDocument)
    ));
    for bytes in [b"".as_slice(), b"# empty", b"k: ["] {
        let Err(LoadError::Configuration(e)) = bind_bytes(bytes, &ctx, |v| reference_schema(v, false)) else {
            panic!("wrapped error");
        };
        assert_eq!(
            e.class,
            "com.malinskiy.marathon.config.exceptions.ConfigurationException".to_owned()
        );
        assert!(e.message.unwrap().to_owned().contains("/config/Marathonfile"));
        assert!(!e.causes.is_empty());
    }
    ctx.environment = vec![("A".into(), "${A}".into())];
    assert!(matches!(
        bind_bytes(b"k: ${A}", &ctx, |v| reference_schema(v, false)),
        Err(LoadError::Substitution(_))
    ));
}
#[test]
fn duplicate_environment_names_substitute_the_first_occurrence() {
    // JVM `System.getenv` resolves a repeated name to its first occurrence.
    let mut ctx = context();
    ctx.environment = vec![("N".into(), "first".into()), ("N".into(), "second".into())];
    let got = bind_bytes(b"k: ${N}", &ctx, |v| reference_schema(v, false)).unwrap();
    assert_eq!(got, "first".to_owned());
}
#[test]
fn absolute_paths_preserve_dot_segments_relative_paths_resolve_symlink_prefixes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir_all(temp.path().join("real/sub"))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.path().join("real/sub"), temp.path().join("link"))?;
    let mut ctx = context();
    ctx.marathonfile_dir = PathBuf::from(temp.path().to_str().ok_or("temp UTF8")?);
    let file = |text: &str| {
        let stream = node::load(text);
        let b = Binder::new(&ctx, &stream);
        b.first(&stream, |v| v.file()).map(|p| p.flatten())
    };
    assert_eq!(file("'/a/../b'").unwrap(), Some(PathBuf::from("/a/../b")));
    #[cfg(unix)]
    {
        let expected = std::fs::canonicalize(temp.path().join("real"))?.join("missing/leaf");
        assert_eq!(file("'link/../missing/leaf'").unwrap().unwrap(), expected);
    }
    assert!(file("'\0'").is_err());
    Ok(())
}
#[test]
fn p2_registry_has_no_java_time_or_marathonfile_path_resolver() {
    let mut ctx = context();
    ctx.profile = Profile::Marathondevices;
    for text in ["1", "'PT1S'"] {
        let s = node::load(text);
        let b = Binder::new(&ctx, &s);
        assert!(b.first(&s, |v| v.duration()).is_err());
    }
    let s = node::load("'relative/key'");
    let b = Binder::new(&ctx, &s);
    assert_eq!(
        b.first(&s, |v| v.file()).unwrap().flatten(),
        Some(PathBuf::from("relative/key"))
    );
}
#[test]
fn native_strings_decode_lossily_before_binding() {
    let got = bind("k: \"\\uD800x\\uDC00\"", |v| reference_schema(v, false))
        .unwrap()
        .unwrap();
    assert_eq!(got, "�x�");
    assert!(bind("\"\\uD800\": x", |v| reference_schema(v, true)).is_ok());
    assert!(bind("\"\\uD800\"", |v| v.regex()).is_ok());
}

// A real 2.2-shaped section with nested polymorphism. Representation remains test-only.
#[derive(Debug)]
struct Filtering {
    allowlist: Vec<Option<Filter>>,
    blocklist: Vec<Option<Filter>>,
}
#[derive(Debug)]
enum Filter {
    Method { regex: Option<JavaRegex>, enabled: bool },
    Allure,
}
fn filter(v: Value<'_>) -> Result<Option<Filter>> {
    v.polymorphic(|id, v| match id {
        "method" => Some((|| {
            let mut regex = Field::nullable(&v, None);
            let mut enabled = Field::primitive(&v, true);
            let mut o = v.object()?;
            while let Some((name, value)) = o.next_field()? {
                match name.as_str() {
                    "regex" => {
                        let bound = value.regex()?;
                        regex.set(value, bound);
                    }
                    "enabled" => {
                        let bound = value.boolean()?;
                        enabled.set(value, Some(bound));
                    }
                    _ => value.unknown(false)?,
                }
            }
            Ok(Filter::Method {
                regex: regex.optional(),
                enabled: enabled.required("enabled")?,
            })
        })()),
        // A schema may return the same object for singleton types.
        "allure" => Some((|| {
            let mut o = v.object()?;
            while let Some((_, value)) = o.next_field()? {
                value.unknown(false)?;
            }
            Ok(Filter::Allure)
        })()),
        _ => None,
    })
}
fn filtering(v: Value<'_>) -> Result<Option<Filtering>> {
    v.nullable(|v| {
        let mut allowlist = Field::reference(&v, Some(Vec::new()));
        let mut blocklist = Field::reference(&v, Some(Vec::new()));
        let mut o = v.object()?;
        while let Some((name, value)) = o.next_field()? {
            match name.as_str() {
                "allowlist" => {
                    let bound = value.list(filter)?;
                    allowlist.set(value, bound);
                }
                "blocklist" => {
                    let bound = value.list(filter)?;
                    blocklist.set(value, bound);
                }
                _ => value.unknown(false)?,
            }
        }
        Ok(Filtering {
            allowlist: allowlist.required("allowlist")?,
            blocklist: blocklist.required("blocklist")?,
        })
    })
}
#[test]
fn filtering_section_late_type_and_singleton() {
    let got = bind(
        "allowlist: [{regex: 'test.*', enabled: null, type: method}, {type: allure}]",
        filtering,
    )
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(got.blocklist.is_empty());
    assert!(matches!(&got.allowlist[0],Some(Filter::Method {regex:Some(r),enabled:false}) if r.pattern()=="test.*"));
    assert!(matches!(&got.allowlist[1], Some(Filter::Allure)));
    assert_eq!(
        bind("{type: mystery}", filter).unwrap_err().class,
        binder::INVALID_TYPE.to_owned()
    );
    assert_eq!(bind("{}", filter).unwrap_err().class, binder::INVALID_TYPE.to_owned());
}
#[test]
fn builder_and_enum_creator_use_shared_primitives() {
    #[derive(Default)]
    struct Builder {
        count: i32,
    }
    impl Builder {
        fn build(self) -> i32 {
            self.count
        }
    }
    let got = bind("count: '12'", |v| {
        let mut builder = Builder::default();
        let mut o = v.object()?;
        while let Some((name, value)) = o.next_field()? {
            match name.as_str() {
                "count" => builder.count = value.int()?,
                _ => value.unknown(false)?,
            }
        }
        Ok(builder.build())
    })
    .unwrap();
    assert_eq!(got, Some(12));
    assert_eq!(
        bind("'green'", |v| v
            .enum_creator(|s| s.eq_ignore_ascii_case("GREEN").then_some(1)))
        .unwrap(),
        Some(Some(1))
    );
}

#[test]
fn read_tree_materialization_matches_jvm() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in include_str!("../../../fixtures/oracle/yaml-binding-corpus.jsonl")
        .lines()
        .skip(1)
    {
        let row: Json = serde_json::from_str(line)?;
        let target = row["target"].as_str().ok_or("target")?;
        if !target.starts_with("Tree") {
            continue;
        }
        count += 1;
        let got = bind(row["input"].as_str().ok_or("input")?, |v| {
            let tree = v.read_tree()?;
            tree.bind(|v| {
                let mut o = v.object()?;
                let mut k = Json::Null;
                while let Some((name, value)) = o.next_field()? {
                    if name != "k" {
                        value.unknown(true)?;
                        continue;
                    }
                    k = match target {
                        "TreeString" => value.string()?.map_or(Json::Null, |s| json!(s)),
                        "TreeDouble" => json!(value.double()?),
                        "TreeMap" => value.map(|v| v.string())?.map_or(Json::Null, |pairs| {
                            Json::Object(pairs.into_iter().map(|(k, v)| (k, json!(v))).collect())
                        }),
                        _ => unreachable!(),
                    };
                }
                Ok(json!({"k":k}))
            })
        });
        match got {
            Ok(v) => assert_eq!(json!({"ok":v}), row["result"], "{row}"),
            Err(e) => assert_eq!(
                e.class.to_owned(),
                row["result"]["error"][0].as_str().ok_or("error")?,
                "{row}"
            ),
        }
    }
    assert_eq!(count, 12);
    Ok(())
}
