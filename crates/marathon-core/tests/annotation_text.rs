//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/filter/AnnotationDataFilter.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/DexTestParser.kt.

mod filter_support;
use filter_support::{annotated, apply, array, filter, regex};
use marathon_config::TestFilterConfiguration;
use marathon_core::{
    error::OperationResult,
    filter::{annotation_text, array_element_value, decoded_text},
    model::{DecodedWrapper, MetaValue as M},
    report::projection::metadata_text,
};
use serde_json::Value;
use std::sync::Arc;

#[test]
fn decoded_array_elements_keep_pinned_wrapper_rendering() {
    use DecodedWrapper::*;
    let cases = [
        (DecodedByte(1), "DecodedByte(value=1)", "1"),
        (DecodedShort(2), "DecodedShort(value=2)", "2"),
        (DecodedInt(7), "DecodedInt(value=7)", "7"),
        (DecodedLong(8), "DecodedLong(value=8)", "8"),
        (DecodedFloat(-0.0), "DecodedFloat(value=-0.0)", "-0.0"),
        (DecodedDouble(2.5), "DecodedDouble(value=2.5)", "2.5"),
        (DecodedChar(b'x' as u16), "DecodedChar(value=x)", "x"),
        (DecodedBoolean(true), "DecodedBoolean(value=true)", "true"),
        (DecodedString("a)b".into()), "DecodedString(value=a)b)", "a"),
        (
            DecodedType("Ltest/C;".into()),
            "DecodedType(value=Ltest/C;)",
            "Ltest/C;",
        ),
        (DecodedEnum("CHOICE".into()), "DecodedEnum(value=CHOICE)", "CHOICE"),
        (DecodedNull, "null", "null"),
        (
            DecodedArrayValue(Arc::from([DecodedInt(7), DecodedInt(8)])),
            "DecodedArrayValue(values=[DecodedInt(value=7), DecodedInt(value=8)])",
            "7",
        ),
        (
            DecodedArrayValue(Arc::from([])),
            "DecodedArrayValue(values=[])",
            "DecodedArrayValue(values=[]",
        ),
    ];
    for (wrapper, expected, filter_value) in cases {
        let rendered = decoded_text(&wrapper);
        assert_eq!(rendered, String::from(expected));
        // Witness the filter's first-delimiter extraction; WP 3.3 owns it.
        let after = expected.split_once("(value=").map_or(expected, |(_, after)| after);
        assert_eq!(after.split_once(')').map_or(after, |(before, _)| before), filter_value);
        let element = M::Decoded(wrapper);
        assert_eq!(element, element.clone());
    }
    assert_eq!(decoded_text(&DecodedChar(0xd800)), {
        let mut text = String::from("DecodedChar(value=");
        text.push('\u{fffd}');
        text.push(')');
        text
    });
}

#[test]
fn annotation_text_grammar_preserves_scalars_collections_and_order() {
    let cases = [
        (M::Null, "null"),
        (M::Boolean(true), "true"),
        (M::Boolean(false), "false"),
        (M::Byte(i8::MIN), "-128"),
        (M::Short(i16::MIN), "-32768"),
        (M::Int(i32::MIN), "-2147483648"),
        (M::Long(i64::MIN), "-9223372036854775808"),
        (M::Char('x' as u16), "x"),
        (M::Char(0xd800), "\u{fffd}"),
        (M::Char(0xdc00), "\u{fffd}"),
        (M::String("a\nb, c=(d) [x] {k=v}\0".into()), "a\nb, c=(d) [x] {k=v}\0"),
        (M::List(vec![]), "[]"),
        (array(vec![]), "[]"),
        (M::Map(Default::default()), "{}"),
        (
            M::Map(
                [("k".into(), M::List(vec![M::List(vec![M::Int(1)])]))]
                    .into_iter()
                    .collect(),
            ),
            "{k=[[1]]}",
        ),
        (
            M::List(vec![
                M::String("a".into()),
                M::Null,
                M::List(vec![M::Int(7)]),
                array(vec![M::Boolean(true)]),
            ]),
            "[a, null, [7], [true]]",
        ),
        (
            M::Map(
                [
                    ("z".into(), M::Int(1)),
                    ("a=b".into(), M::String("x, y".into())),
                    ("z".into(), M::Null),
                ]
                .into_iter()
                .collect(),
            ),
            "{z=null, a=b=x, y}",
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(annotation_text(&value), expected);
        assert_eq!(metadata_text(&value), expected);
    }
    assert_eq!(
        annotation_text(&array(vec![M::Decoded(DecodedWrapper::DecodedNull)])),
        "[null]"
    );
    assert_eq!(
        decoded_text(&DecodedWrapper::DecodedChar(0xdc00)),
        "DecodedChar(value=\u{fffd})"
    );
}

#[test]
fn annotation_float_spellings_are_input_syntax() {
    for (value, expected) in [
        (0.0, "0.0"),
        (-0.0, "-0.0"),
        (1.0, "1.0"),
        (1e7, "1.0E7"),
        (0.001, "0.001"),
        (0.0001, "1.0E-4"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ] {
        assert_eq!(annotation_text(&M::Double(value)), expected);
        assert_eq!(annotation_text(&M::Float(value as f32)), expected);
        assert_eq!(
            decoded_text(&DecodedWrapper::DecodedDouble(value)),
            format!("DecodedDouble(value={expected})")
        );
        assert_eq!(
            decoded_text(&DecodedWrapper::DecodedFloat(value as f32)),
            format!("DecodedFloat(value={expected})")
        );
    }
}

#[test]
fn annotation_filter_keeps_null_and_outer_array_extraction_rules() -> OperationResult<()> {
    let make_filter = |value| {
        filter(&TestFilterConfiguration::AnnotationData {
            name_regex: regex("A"),
            value_regex: regex(value),
            enabled: true,
        })
    };
    let input = vec![
        annotated("A", M::Null),
        annotated("A", M::Decoded(DecodedWrapper::DecodedNull)),
        annotated("A", array(vec![M::Null])),
        annotated("A", M::List(vec![M::Null])),
    ];
    let selected = apply(make_filter("null"), input.clone(), false)?;
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].meta_properties, input[1].meta_properties);
    assert_eq!(selected[1].meta_properties, input[2].meta_properties);
    let value = M::Decoded(DecodedWrapper::DecodedString("a)b".into()));
    assert_eq!(annotation_text(&value), "DecodedString(value=a)b)");
    assert_eq!(array_element_value(&annotation_text(&value)), "a");
    let input = vec![
        annotated("A", array(vec![value.clone()])),
        annotated("A", M::List(vec![value.clone()])),
        annotated("A", value),
    ];
    let selected = apply(make_filter("a"), input.clone(), false)?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].meta_properties, input[0].meta_properties);
    let selected = apply(make_filter(r"\[DecodedString\(value=a\)b\)\]"), input.clone(), false)?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].meta_properties, input[1].meta_properties);
    Ok(())
}

fn units(value: &Value) -> String {
    String::from_utf16_lossy(
        &value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u16)
            .collect::<Vec<_>>(),
    )
}

// Moved from jcompat's writer test. Only the fixed probe shell is assembled
// here; every scalar, ordered map and list goes through the input renderer.
#[test]
fn kotlin_utf16_edges() -> Result<(), Box<dyn std::error::Error>> {
    for line in include_str!("../../../fixtures/oracle/jcompat/kotlin_writer_edges.jsonl")
        .lines()
        .skip(1)
    {
        let row: Value = serde_json::from_str(line)?;
        let input = &row["input"]["fields"][0][1];
        let value = if input["kind"] == "null" {
            M::Null
        } else {
            M::String(units(&input["units"]))
        };
        let text = annotation_text(&value);
        let map = M::Map([("a".into(), value), ("null".into(), M::Null)].into_iter().collect());
        let nested = M::List(vec![
            map,
            M::String(format!("({text}, x)")),
            M::String("(1.0, 1.0E7, -0.0)".into()),
        ]);
        let actual = format!("WriterProbe(text={text}, nested={})", annotation_text(&nested));
        assert_eq!(actual, units(&row["expected"]), "{}", row["id"]);
    }
    Ok(())
}
