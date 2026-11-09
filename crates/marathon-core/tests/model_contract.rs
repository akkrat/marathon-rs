//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt
//! Contract regressions supplement, but do not replace, the parity matrix.

use marathon_core::model::{DecodedWrapper, MetaArray, MetaMap, MetaProperty, MetaValue, Test, TestBatch};
use std::{collections::HashSet, sync::Arc};

fn test_with_metadata(value: &str) -> Test {
    Test {
        pkg: "p".into(),
        clazz: "C".into(),
        method: "m".into(),
        meta_properties: vec![MetaProperty {
            name: "annotation".into(),
            values: [("value".into(), MetaValue::String(value.into()))]
                .into_iter()
                .collect(),
        }],
    }
}
#[test]
fn metadata_changes_neither_test_keys_nor_batch_value_equality() {
    let a = test_with_metadata("a");
    let b = test_with_metadata("b");
    assert_ne!(a.meta_properties, b.meta_properties);
    assert_eq!(HashSet::from([a.clone(), b.clone()]).len(), 1);
    let first = Arc::new(TestBatch {
        tests: vec![a],
        id: "same".into(),
    });
    let second = Arc::new(TestBatch {
        tests: vec![b],
        id: "same".into(),
    });
    assert_eq!(first, second);
    assert!(!Arc::ptr_eq(&first, &second));
}
#[test]
fn annotation_arrays_retain_identity_and_maps_ignore_entry_order() {
    let array = Arc::new(MetaArray {
        jvm_class: "[I".into(),
        elements: vec![MetaValue::Int(1)],
    });
    let same = MetaValue::Array(array.clone());
    let fresh = MetaValue::Array(Arc::new(MetaArray {
        jvm_class: "[I".into(),
        elements: vec![MetaValue::Int(1)],
    }));
    assert_eq!(same, MetaValue::Array(array));
    assert_ne!(same, fresh);
    assert_eq!(
        MetaValue::Map(
            [("a".into(), same.clone()), ("b".into(), MetaValue::Null)]
                .into_iter()
                .collect()
        ),
        MetaValue::Map(
            [("b".into(), MetaValue::Null), ("a".into(), same)]
                .into_iter()
                .collect()
        )
    );
}
#[test]
fn java_boxed_values_and_native_string_ordering() {
    assert_ne!(MetaValue::Int(1), MetaValue::Long(1));
    assert_ne!(MetaValue::Double(0.0), MetaValue::Double(-0.0));
    assert_eq!(
        MetaValue::Double(f64::NAN),
        MetaValue::Double(f64::from_bits(0x7ff8_0000_0000_0001))
    );
    assert!("😀" > "ｚ");
    assert_eq!(String::from_utf16_lossy(&[0xd800]), "\u{fffd}");
}

#[test]
fn duplicate_metadata_keys_replace_values_and_preserve_lawful_equality() {
    let map = |entries: &[(&str, i32)]| -> MetaMap {
        entries
            .iter()
            .map(|(key, value)| ((*key).into(), MetaValue::Int(*value)))
            .collect()
    };
    let a = map(&[("x", 1), ("x", 1)]);
    let b = map(&[("x", 1), ("y", 2)]);
    let c = map(&[("x", 1), ("x", 2)]);
    for left in [&a, &b, &c] {
        assert_eq!(left, left);
        for right in [&a, &b, &c] {
            assert_eq!(left == right, right == left);
            let property = |values: &MetaMap| MetaProperty {
                name: "a".into(),
                values: values.clone(),
            };
            assert_eq!(property(left) == property(right), property(right) == property(left));
            assert_eq!(
                MetaValue::Map(left.clone()) == MetaValue::Map(right.clone()),
                MetaValue::Map(right.clone()) == MetaValue::Map(left.clone())
            );
        }
    }
    assert_ne!(a, b);
    assert_eq!(c, map(&[("x", 2)]));
    let mut ordered = map(&[("x", 1), ("y", 2)]);
    assert_eq!(ordered.insert("x".into(), MetaValue::Int(3)), Some(MetaValue::Int(1)));
    assert_eq!(
        ordered.entries(),
        &[("x".into(), MetaValue::Int(3)), ("y".into(), MetaValue::Int(2))]
    );
}

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
        let rendered = wrapper.kotlin_value().render();
        assert_eq!(rendered, String::from(expected));
        // Witness the filter's first-delimiter extraction; WP 3.3 owns it.
        let after = expected.split_once("(value=").map_or(expected, |(_, after)| after);
        assert_eq!(after.split_once(')').map_or(after, |(before, _)| before), filter_value);
        let element = MetaValue::Decoded(wrapper);
        assert_eq!(element, element.clone());
    }
    assert_eq!(DecodedChar(0xd800).kotlin_value().render(), {
        let mut text = String::from("DecodedChar(value=");
        text.push('\u{fffd}');
        text.push(')');
        text
    });
}

#[test]
fn decoded_wrapper_equality_keeps_kind_singleton_and_array_aliases() {
    use DecodedWrapper::*;
    assert_ne!(DecodedInt(7), DecodedLong(7));
    assert_ne!(DecodedString("x".into()), DecodedType("x".into()));
    assert_ne!(DecodedType("x".into()), DecodedEnum("x".into()));
    assert_ne!(MetaValue::Decoded(DecodedNull), MetaValue::Null);
    assert_ne!(MetaValue::Decoded(DecodedInt(7)), MetaValue::Int(7));
    assert_eq!(DecodedNull, DecodedNull);
    assert_ne!(DecodedFloat(-0.0), DecodedFloat(0.0));
    assert_ne!(DecodedDouble(-0.0), DecodedDouble(0.0));
    assert_eq!(DecodedFloat(f32::NAN), DecodedFloat(f32::from_bits(0x7fc0_0001)));
    assert_eq!(
        DecodedDouble(f64::NAN),
        DecodedDouble(f64::from_bits(0x7ff8_0000_0000_0001))
    );
    let values: Arc<[DecodedWrapper]> = Arc::from([DecodedInt(7)]);
    assert_eq!(DecodedArrayValue(values.clone()), DecodedArrayValue(values.clone()));
    assert_ne!(DecodedArrayValue(values), DecodedArrayValue(Arc::from([DecodedInt(7)])));
}
