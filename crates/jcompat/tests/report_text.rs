//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/{html/HtmlSummaryReporter,allure/AllureReporter}.kt.
use jcompat::{
    file_manager::{JavaPath, child_file, relative_file},
    jnum::kotlin_round_to_long,
    report_text::*,
    writers::WriterValue,
};
#[test]
fn jvm_paths_hash_xml_and_rounding() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../fixtures/oracle/html-allure-gif/probe.json")).unwrap();
    let oracle = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap();
    let root = JavaPath::from_text(include_str!(
        "../../../fixtures/oracle/html-allure-gif/runtime-root.txt"
    ));
    let html = child_file(&root, &"/html".encode_utf16().collect::<Vec<_>>());
    assert_eq!(
        String::from_utf16_lossy(&html.0.clone()).to_owned(),
        oracle("path")["html"]
    );
    assert_eq!(
        relative_file(
            &html,
            &child_file(
                &html,
                &"pools/raw # pool/device/logs".encode_utf16().collect::<Vec<_>>()
            ),
            &root
        )
        .to_owned(),
        "../../../.."
    );
    assert_eq!(allure_history_id("Clazz", "test1"), oracle("history")["hash"]);
    for (i, v) in [-1.5, -0.5, 0.5, 4503599627370497.0].into_iter().enumerate() {
        assert_eq!(kotlin_round_to_long(v), oracle("rounding")["outputs"][i].as_i64())
    }
    assert_eq!(kotlin_round_to_long(f64::NAN), None);
    assert_eq!(kotlin_round_to_long(f64::INFINITY), Some(i64::MAX));
    assert_eq!(form_encode("/a #😀").to_owned(), "%2Fa+%23%F0%9F%98%80");
}
#[test]
fn jackson_keeps_nulls_arrays_and_utf16_escapes() {
    let actual = jackson_json(&WriterValue::Object(vec![
        ("x".into(), WriterValue::Null),
        ("empty".into(), WriterValue::Array(vec![])),
        (
            "utf16".into(),
            WriterValue::String(String::from_utf16_lossy(&[0xd800, 0xd83d, 0xde00])),
        ),
    ]));
    assert_eq!(
        actual.as_bytes().to_vec(),
        br#"{"x":null,"empty":[],"utf16":"\uD800\uD83D\uDE00"}"#
    );
    // Jackson's byte[] default: padded standard base64 without line feeds.
    for (bytes, encoded) in [
        (&b""[..], "\"\""),
        (b"\xff", "\"/w==\""),
        (b"\x01\x02\x03\x04", "\"AQIDBA==\""),
    ] {
        assert_eq!(jackson_json(&WriterValue::Binary(bytes.to_vec())).to_owned(), encoded);
    }
}
#[test]
fn instant_epoch_millis_overflow_and_negative_boundary() {
    use jcompat::jtime::instant_of;
    assert_eq!(instant_of(-1, 1).unwrap().to_epoch_millis(), Some(-1000));
    assert_eq!(
        instant_of(i64::MIN.div_euclid(1000), i64::MIN.rem_euclid(1000) * 1_000_000)
            .unwrap()
            .to_epoch_millis(),
        Some(i64::MIN)
    );
    assert_eq!(
        instant_of(jcompat::jtime::INSTANT_MAX_SECONDS, 0)
            .unwrap()
            .to_epoch_millis(),
        None
    );
}
#[test]
fn awt_nearest_index_matches_jvm_axis_maps() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../fixtures/oracle/html-allure-gif/probe.json")).unwrap();
    let oracle = |id: &str| {
        rows.iter().find(|r| r["id"] == id).unwrap()["maps"]
            .as_array()
            .unwrap()
            .clone()
    };
    let map = |s: u32, t: u32| (0..t).map(|d| awt_nearest_index(d, s, t)).collect::<Vec<_>>();
    let small = oracle("resize-axis-small");
    assert_eq!(small.len(), 256);
    for row in small {
        let (s, t) = (row[0].as_u64().unwrap() as u32, row[1].as_u64().unwrap() as u32);
        let expected: Vec<u32> = row[2]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        assert_eq!(map(s, t), expected, "{s}->{t}");
    }
    // Large pairs include realistic screen sizes and ones only 32-bit fixed point reproduces.
    for row in oracle("resize-axis-large") {
        let (s, t) = (row[0].as_u64().unwrap() as u32, row[1].as_u64().unwrap() as u32);
        let joined = map(s, t).iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        assert_eq!(format!("{:x}", md5::compute(joined)), row[2], "{s}->{t}");
    }
    // 2→1 ties go up, 2→3 ties go down.
    assert_eq!((map(2, 1), map(2, 3)), (vec![1], vec![0, 0, 1]));
}
