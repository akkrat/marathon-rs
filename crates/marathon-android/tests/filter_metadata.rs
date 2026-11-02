//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/DexTestParser.kt:47–67
//! through core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AnnotationDataFilter.kt.

#[path = "../../marathon-core/tests/filter_support/mod.rs"]
mod filter_support;
use filter_support::*;
use jcompat::writers::JavaString;
use marathon_android::dex_test_parser::to_meta_property;
use marathon_config::schema::TestFilterConfiguration as C;
use marathon_core::{
    error::OperationResult,
    executor::{
        CoreExecutor,
        hooks::{Ids, RuntimeHooks, Tape},
    },
    filter::{AnnotationRenderer, TestFilter},
    model::MetaValue,
    time::ObjectIdentity,
};
use marathon_dex::{DecodedValue as D, TestAnnotation};
use std::sync::Arc;

#[test]
fn c28_real_metadata_conversion_and_filtering_replay_the_jvm() -> OperationResult<()> {
    let tape = Tape {
        ids: [(
            "metadata.decoded_null.identity".into(),
            Ids::Values(vec!["<identity>".into()]),
        )]
        .into_iter()
        .collect(),
        ..Tape::default()
    };
    let rendering = CoreExecutor::run(move |core| {
        Box::pin(async move {
            Ok(Arc::new(AnnotationRenderer::new(
                RuntimeHooks::taped(tape, &core.clock()),
                ObjectIdentity(0),
                Arc::new(|| ObjectIdentity(1)),
            )))
        })
    })?;
    for line in include_str!("../../../fixtures/oracle/filters/annotation.jsonl")
        .split('\n')
        .skip(1)
        .filter(|s| !s.is_empty())
    {
        let row: serde_json::Value = serde_json::from_str(line).map_err(|e| fail(&e.to_string()))?;
        let Some(kind) = row["producer"].as_str() else {
            continue;
        };
        let value = match kind {
            "byte" => D::Byte(-1),
            "short" => D::Short(-2),
            "int" => D::Int(-3),
            "long" => D::Long(-4),
            "float" => D::Float(-0.0),
            "double" => D::Double(f64::NAN),
            "char" => D::Char(b')'.into()),
            "boolean" => D::Boolean(true),
            "decodedString" => D::String("a)b".into()),
            "type" => D::Type("Ltype;".into()),
            "enum" => D::Enum("ENUM".into()),
            "decodedNull" => D::Null,
            "nested" => D::Array(vec![D::String("a)b".into()), D::Null].into()),
            "empty" => D::Array(vec![].into()),
            _ => panic!("producer"),
        };
        let annotation = TestAnnotation {
            name: "A".into(),
            values: vec![("value".into(), D::Array(vec![value].into()))],
            inherited: false,
        };
        let meta = to_meta_property(&annotation);
        let MetaValue::Array(array) = &meta.values.entries()[0].1 else {
            panic!("retained array")
        };
        let rendered = rendering.render(&array.elements[0])?;
        let extracted = marathon_core::filter::array_element_value(&rendered);
        assert_eq!(extracted.to_string_lossy(), row["extracted"]);
        let mut pattern = String::new();
        for c in extracted.as_utf8().expect("oracle UTF-16").chars() {
            if "\\.+*?()|[]{}^$".contains(c) {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        let c = C::AnnotationData {
            name_regex: regex("A"),
            value_regex: regex(&pattern),
            enabled: true,
        };
        let mut dependencies = deps(None, Arc::default());
        dependencies.rendering = rendering.clone();
        let f = TestFilter::from_config(&c, &dependencies);
        let mut t = test("pkg", "Class", "method");
        t.meta_properties.push(meta.clone());
        let selected = apply(f, vec![t], false)?;
        assert_eq!(selected.len() as u64, row["selected"].as_u64().expect("selected"));
        assert_eq!(selected[0].meta_properties, vec![meta]);
    }
    rendering
        .hooks
        .verify_consumed()
        .map_err(|e| fail(&e.message.to_string_lossy()))?;
    // Top-level null is unboxed; the array element above retains DecodedNull.
    let meta = to_meta_property(&TestAnnotation {
        name: "A".into(),
        values: vec![("value".into(), D::Null)],
        inherited: false,
    });
    assert_eq!(meta.values.entries()[0].1, MetaValue::Null);
    assert_eq!(meta.name, JavaString::from("A"));
    Ok(())
}
