//! New Rust tests (no Kotlin counterpart): the delegate flavours, the byte-exact writer
//! and the parse errors of WP 4.5a, against the JVM probe outcomes in
//! `tools/oracle/plist-probes/expected.txt`.

use marathon_apple::plist::bundle::BundleInfo;
use marathon_apple::plist::writer::write_value_xml;
use marathon_apple::plist::{PlistDict, PlistError, PlistValue, PropertyList, dict_from_bytes};
use marathon_apple::xctestrun::v0::{TestTarget, TestTargetParams, Xctestrun as V0Run};
use marathon_apple::xctestrun::v1::Xctestrun as V1Run;
use marathon_apple::xctestrun::v2::{ArtifactReuseParams, Metadata, TestTarget as V2Target, Xctestrun as V2Run};

fn xml_of(dict: &PlistDict) -> String {
    let info = BundleInfo::wrap(dict.clone());
    String::from_utf8(info.to_xml_bytes()).expect("utf8 xml")
}

fn dict_with(entries: Vec<(&str, PlistValue)>) -> PlistDict {
    let dict = PlistDict::new();
    for (k, v) in entries {
        dict.insert(k, v);
    }
    dict
}

#[test]
fn writer_scalars_match_the_jvm_probe() {
    let dict = PlistDict::new();
    dict.insert("EmptyString", PlistValue::String(String::new()));
    dict.insert("EmptyDict", PlistValue::Dict(PlistDict::new()));
    dict.insert("EmptyArray", PlistValue::Array(Vec::new()));
    dict.insert(
        "Nested",
        PlistValue::Dict(dict_with(vec![
            ("InnerEmptyArray", PlistValue::Array(Vec::new())),
            ("InnerEmptyDict", PlistValue::Dict(PlistDict::new())),
        ])),
    );
    dict.insert("Escapes", PlistValue::String("a&b<c>d\"e'f\ng\th\ri".to_owned()));
    dict.insert("NonAscii", PlistValue::String("café — üñïçødé 😀".to_owned()));
    dict.insert("Key<&", PlistValue::String("special key".to_owned()));
    dict.insert("NegInt", PlistValue::Integer(-42));
    dict.insert("BigInt", PlistValue::Integer(3_000_000_000));
    dict.insert("LongMax", PlistValue::Integer(i128::from(i64::MAX)));
    dict.insert("LongMin", PlistValue::Integer(i128::from(i64::MIN)));
    dict.insert("Real", PlistValue::Real(1.5));
    dict.insert("RealWhole", PlistValue::Real(2.0));
    dict.insert("RealNegZero", PlistValue::Real(-0.0));
    dict.insert("RealSmall", PlistValue::Real(0.000_001));
    dict.insert("RealBig", PlistValue::Real(1.0e20));
    dict.insert("True", PlistValue::Bool(true));
    dict.insert("False", PlistValue::Bool(false));
    let date = plist::Date::from_xml_format("2024-01-02T03:04:05Z").expect("date");
    dict.insert("Date", PlistValue::Date(date));
    dict.insert("Data", PlistValue::Data(vec![0x00, 0x01, 0x02, 0xfd, 0xfe, 0xff]));
    dict.insert("LongData", PlistValue::Data((0..80).map(|i| i as u8).collect()));
    dict.insert("Uid", PlistValue::Uid(258));

    // Byte-for-byte the document dd-plist wrote in probe `writer-scalars` (1434 bytes).
    let expected = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n<dict>\n",
        "\t<key>EmptyString</key>\n\t<string></string>\n",
        "\t<key>EmptyDict</key>\n\t<dict>\n\t</dict>\n",
        "\t<key>EmptyArray</key>\n\t<array>\n\t</array>\n",
        "\t<key>Nested</key>\n\t<dict>\n\t\t<key>InnerEmptyArray</key>\n\t\t<array>\n\t\t</array>\n",
        "\t\t<key>InnerEmptyDict</key>\n\t\t<dict>\n\t\t</dict>\n\t</dict>\n",
        "\t<key>Escapes</key>\n\t<string><![CDATA[a&b<c>d\"e'f\ng\th\ri]]></string>\n",
        "\t<key>NonAscii</key>\n\t<string>café — üñïçødé 😀</string>\n",
        "\t<key><![CDATA[Key<&]]></key>\n\t<string>special key</string>\n",
        "\t<key>NegInt</key>\n\t<integer>-42</integer>\n",
        "\t<key>BigInt</key>\n\t<integer>3000000000</integer>\n",
        "\t<key>LongMax</key>\n\t<integer>9223372036854775807</integer>\n",
        "\t<key>LongMin</key>\n\t<integer>-9223372036854775808</integer>\n",
        "\t<key>Real</key>\n\t<real>1.5</real>\n",
        "\t<key>RealWhole</key>\n\t<real>2.0</real>\n",
        "\t<key>RealNegZero</key>\n\t<real>-0.0</real>\n",
        "\t<key>RealSmall</key>\n\t<real>1.0E-6</real>\n",
        "\t<key>RealBig</key>\n\t<real>1.0E20</real>\n",
        "\t<key>True</key>\n\t<true/>\n",
        "\t<key>False</key>\n\t<false/>\n",
        "\t<key>Date</key>\n\t<date>2024-01-02T03:04:05Z</date>\n",
        "\t<key>Data</key>\n\t<data>\n\t\tAAEC/f7/\n\t</data>\n",
        "\t<key>LongData</key>\n\t<data>\n\t\tAAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BBQkNERUZHSElKS0xNTk8=\n\t</data>\n",
        "\t<key>Uid</key>\n\t<dict>\n\t\t<key>CF$UID</key>\n\t\t<integer>258</integer>\n\t</dict>\n",
        "</dict>\n</plist>",
    );
    let actual = xml_of(&dict);
    assert_eq!(actual.len(), 1434);
    assert_eq!(actual, expected);
}

#[test]
fn writer_escape_and_real_rules_match_the_jvm_probe() {
    for (input, rendered) in [
        ("amp&only", "<string><![CDATA[amp&only]]></string>"),
        ("lt<only", "<string><![CDATA[lt<only]]></string>"),
        ("gt>only", "<string><![CDATA[gt>only]]></string>"),
        ("quot\"only", "<string>quot\"only</string>"),
        (
            "cdata]]>inside",
            "<string><![CDATA[cdata]]]]><![CDATA[>inside]]></string>",
        ),
        ("ctrl\u{1}char", "<string>ctrlchar</string>"),
        ("del\u{7f}char", "<string>del\u{7f}char</string>"),
        ("a&b\u{1}c", "<string><![CDATA[a&bc]]></string>"),
        (" bad\u{fffe}c", "<string> badc</string>"),
    ] {
        let dict = dict_with(vec![("K", PlistValue::String(input.to_owned()))]);
        assert!(xml_of(&dict).contains(rendered), "input {input:?}");
    }
    // R1-claude-3 (probe `writer-key-control`): keys are never filtered — a control
    // character in a key writes raw, in plain and CDATA-wrapped keys alike, while the
    // same characters in a value are dropped.
    let keyed = dict_with(vec![
        ("k\u{1}ey", PlistValue::String("plain control key".to_owned())),
        ("k\u{1}&<ey", PlistValue::String("cdata control key".to_owned())),
        ("bad\u{fffe}key", PlistValue::String("fffe key".to_owned())),
        ("V", PlistValue::String("v\u{1}\u{fffe}w".to_owned())),
    ]);
    let keyed_xml = xml_of(&keyed);
    assert!(keyed_xml.contains("<key>k\u{1}ey</key>"), "{keyed_xml:?}");
    assert!(keyed_xml.contains("<key><![CDATA[k\u{1}&<ey]]></key>"));
    assert!(keyed_xml.contains("<key>bad\u{fffe}key</key>"));
    assert!(keyed_xml.contains("<string>vw</string>"));

    // R1-claude-4 (probe `writer-date-truncate`): dates write whole seconds only.
    for (epoch_ms, rendered) in [
        (1_700_000_000_123u64, "<date>2023-11-14T22:13:20Z</date>"),
        (1_704_164_645_123, "<date>2024-01-02T03:04:05Z</date>"),
        (1_704_164_645_000, "<date>2024-01-02T03:04:05Z</date>"),
    ] {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(epoch_ms);
        let dict = dict_with(vec![("D", PlistValue::Date(plist::Date::from(time)))]);
        assert!(xml_of(&dict).contains(rendered), "epoch-ms {epoch_ms}");
    }

    for (value, rendered) in [
        (f64::NAN, "<real>nan</real>"),
        (f64::INFINITY, "<real>+infinity</real>"),
        (f64::NEG_INFINITY, "<real>-infinity</real>"),
        (0.1, "<real>0.1</real>"),
        (2.5e-5, "<real>2.5E-5</real>"),
        (10_000_000.0, "<real>1.0E7</real>"),
        (f64::MIN_POSITIVE * f64::EPSILON, "<real>4.9E-324</real>"),
    ] {
        let dict = dict_with(vec![("R", PlistValue::Real(value))]);
        assert!(xml_of(&dict).contains(rendered), "value {value}");
    }
    // Scalar root (probe `writer-root-string`).
    let mut out = Vec::new();
    write_value_xml(&PlistValue::String("hello".to_owned()), &mut out).expect("write");
    assert_eq!(out.len(), 195);
    // No trailing newline after </plist>, ever.
    assert!(!xml_of(&PlistDict::new()).ends_with('\n'));
    assert_eq!(
        xml_of(&PlistDict::new()),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n</dict>\n</plist>"
    );
}

#[test]
fn delegate_flavours_missing_wrong_and_lazy() {
    let empty = BundleInfo::wrap(PlistDict::new());
    // Construction over an empty dictionary succeeds; only field access fails.
    assert!(matches!(
        empty.identification().bundle_identifier(),
        Err(PlistError::MissingKey { .. })
    ));
    assert!(matches!(
        empty.operating_system_version().ls_minimum_system_version(),
        Err(PlistError::MissingKey { .. })
    ));
    // Optional-array flavour: absent is empty, never an error.
    assert_eq!(
        empty.undocumented().bundle_supported_platforms().expect("platforms"),
        Vec::<String>::new()
    );
    assert_eq!(
        empty.undocumented().ui_device_family().expect("family"),
        Vec::<i32>::new()
    );

    let wrong = BundleInfo::wrap(dict_with(vec![("CFBundleIdentifier", PlistValue::Integer(7))]));
    assert!(matches!(
        wrong.identification().bundle_identifier(),
        Err(PlistError::WrongType { .. })
    ));
    // A malformed value under an untouched key never fails a read of another key.
    let mixed = BundleInfo::wrap(dict_with(vec![
        ("CFBundleIdentifier", PlistValue::Integer(7)),
        ("CFBundleName", PlistValue::String("App".to_owned())),
    ]));
    assert_eq!(mixed.naming().bundle_name().expect("name"), "App");

    // Optional scalar: absent reads None.
    let target = TestTarget::with_params(TestTargetParams::new(Vec::new()));
    assert_eq!(target.baseline_path().expect("baseline"), None);
    assert!(matches!(target.test_bundle_path(), Err(PlistError::MissingKey { .. })));
}

#[test]
fn null_writes_store_nothing_and_never_clear() {
    // Probe `null-writes`: the reuse-path v0 constructor produces no TestBundlePath /
    // TestHostPath keys at all, and an existing value survives a later null write.
    let target = TestTarget::with_params(TestTargetParams::new(vec![("A".to_owned(), "1".to_owned())]));
    assert_eq!(target.dict().keys(), vec!["TestingEnvironmentVariables".to_owned()]);
    target.set_baseline_path(Some("somewhere".to_owned()));
    target.set_baseline_path(None);
    assert_eq!(target.baseline_path().expect("baseline"), Some("somewhere".to_owned()));
    target.set_test_bundle_path(Some("x".to_owned()));
    target.set_test_bundle_path(None);
    assert_eq!(target.test_bundle_path().expect("bundle path"), "x");

    // optionalPlistDelegateFor: set_test_plan(None) cannot clear TestPlan.
    let run = V2Run::wrap(PlistDict::new());
    let plan = marathon_apple::xctestrun::v2::TestPlan::new("P".to_owned(), true);
    run.set_test_plan(Some(&plan));
    run.set_test_plan(None);
    assert!(run.test_plan().expect("plan").is_some());

    // Artifact reuse end to end: no bundle/host path keys in the written bytes.
    let reuse = V2Target::with_artifact_reuse(ArtifactReuseParams::new(
        "Tests".to_owned(),
        "com.example.host".to_owned(),
        "Tests.xctest".to_owned(),
        "com.example.app".to_owned(),
        vec![("A".to_owned(), "1".to_owned())],
    ));
    let xml = String::from_utf8(reuse.to_xml_bytes()).expect("utf8");
    assert!(!xml.contains("TestBundlePath"));
    assert!(!xml.contains("<key>TestHostPath</key>"));
    assert!(xml.contains("<key>UseDestinationArtifacts</key>\n\t<true/>"));
}

#[test]
fn conversions_match_the_jvm_probe() {
    let wide = Metadata::wrap(dict_with(vec![("FormatVersion", PlistValue::Integer(3_000_000_000))]));
    assert_eq!(wide.format_version().expect("wide"), -1_294_967_296);
    let real = Metadata::wrap(dict_with(vec![("FormatVersion", PlistValue::Real(2.9))]));
    assert_eq!(real.format_version().expect("real"), 2);
    let boolean = Metadata::wrap(dict_with(vec![("FormatVersion", PlistValue::Bool(true))]));
    assert!(matches!(boolean.format_version(), Err(PlistError::WrongType { .. })));

    let family = |value: PlistValue| {
        BundleInfo::wrap(dict_with(vec![("UIDeviceFamily", PlistValue::Array(vec![value]))]))
            .undocumented()
            .ui_device_family()
    };
    assert_eq!(
        family(PlistValue::Integer(3_000_000_000)).expect("wide elem"),
        vec![-1_294_967_296]
    );
    assert!(matches!(
        family(PlistValue::Real(2.0)),
        Err(PlistError::WrongType { .. })
    ));
    assert!(matches!(
        family(PlistValue::String("5".to_owned())),
        Err(PlistError::WrongType { .. })
    ));

    // P9: a non-string env value fails at access (Kotlin lets it through the unchecked
    // cast and fails later, elsewhere).
    let env = dict_with(vec![
        ("S", PlistValue::String("ok".to_owned())),
        ("N", PlistValue::Integer(3)),
    ]);
    let target = TestTarget::wrap(dict_with(vec![("TestingEnvironmentVariables", PlistValue::Dict(env))]));
    assert!(matches!(
        target.testing_environment_variables(),
        Err(PlistError::WrongType { .. })
    ));
}

#[test]
fn equality_key_order_and_duplicate_keys() {
    let a = dict_with(vec![
        ("X", PlistValue::String("1".to_owned())),
        ("Y", PlistValue::Integer(2)),
    ]);
    let b = dict_with(vec![
        ("Y", PlistValue::Integer(2)),
        ("X", PlistValue::String("1".to_owned())),
    ]);
    assert_eq!(a, b);
    // Integer never equals real, bool never equals integer.
    assert_ne!(PlistValue::Integer(2), PlistValue::Real(2.0));
    assert_ne!(PlistValue::Bool(true), PlistValue::Integer(1));

    // Replacing a key keeps its position, a new key appends.
    a.insert("X", PlistValue::String("replaced".to_owned()));
    a.insert("W", PlistValue::String("appended".to_owned()));
    assert_eq!(a.keys(), vec!["X".to_owned(), "Y".to_owned(), "W".to_owned()]);

    // R1-sol-3 (probe `equality-shared`): comparing a dictionary with one it contains
    // must answer, not deadlock on the shared node's lock. dd-plist answers false.
    let c = dict_with(vec![("k", PlistValue::String("x".to_owned()))]);
    let b = dict_with(vec![("k", PlistValue::Dict(c.clone()))]);
    let a = dict_with(vec![("k", PlistValue::Dict(b.clone()))]);
    assert_ne!(a, b);
    assert_ne!(b, c);
    let c2 = dict_with(vec![("k", PlistValue::String("x".to_owned()))]);
    let b2 = dict_with(vec![("k", PlistValue::Dict(c2))]);
    let a2 = dict_with(vec![("k", PlistValue::Dict(b2))]);
    assert_eq!(a, a2);

    // Duplicate key on parse: last value, first position (probe `duplicate-keys`).
    let parsed = dict_from_bytes(
        b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>A</key><string>first</string><key>B</key><string>bee</string><key>A</key><string>second</string></dict></plist>",
    )
    .expect("parse");
    assert_eq!(parsed.keys(), vec!["A".to_owned(), "B".to_owned()]);
    assert_eq!(parsed.get_cloned("A"), Some(PlistValue::String("second".to_owned())));
}

#[test]
fn targets_lazy_and_version_differences() {
    // v0 casts every value to a dictionary; v1 filters non-dictionaries out.
    let dict = dict_with(vec![("version", PlistValue::Integer(1))]);
    assert!(matches!(
        V0Run::wrap(dict.clone()).targets(),
        Err(PlistError::WrongType { .. })
    ));
    assert!(V1Run::wrap(dict).targets().is_empty());

    // `by lazy`: a root mutation after the first targets() call is not seen.
    let inner = PlistDict::new();
    let root = dict_with(vec![("One", PlistValue::Dict(inner))]);
    let run = V0Run::wrap(root.clone());
    assert_eq!(run.targets().expect("targets").len(), 1);
    root.insert("Two", PlistValue::Dict(PlistDict::new()));
    assert_eq!(run.targets().expect("targets again").len(), 1);
}

#[test]
fn parse_errors_are_errors_not_panics() {
    assert!(matches!(
        dict_from_bytes(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><string>x</string></plist>"),
        Err(PlistError::RootNotDictionary { .. })
    ));
    assert!(matches!(
        dict_from_bytes(b"not a plist"),
        Err(PlistError::UnsupportedFormat)
    ));
    assert!(matches!(
        dict_from_bytes(b"<plist><dict><key>A</key>"),
        Err(PlistError::NotAPlist { .. })
    ));
    // P1: an OpenStep plist is rejected with the dedicated error.
    assert!(matches!(
        dict_from_bytes(b"{ CFBundleName = TestApp; }"),
        Err(PlistError::UnsupportedFormat)
    ));
    // Missing file is distinguishable from a parse failure.
    let missing = BundleInfo::from_path(std::path::Path::new("/nonexistent/Info.plist"));
    assert!(matches!(missing, Err(PlistError::Io { is_not_found: true, .. })));
}

/// Review round 1 (R1-sol-1/R1-claude-1, probe `cdata-read`): our own writer's CDATA must
/// survive our reader — the `plist` crate's XML reader dropped CDATA sections, emptying
/// every value containing `&`, `<` or `>`. CR and CRLF normalise to LF, as in dd-plist.
#[test]
fn cdata_round_trip_and_handwritten_cdata() {
    let dict = dict_with(vec![
        ("URL", PlistValue::String("https://h/?a=1&b=2".to_owned())),
        ("K<&]]>", PlistValue::String("x]]>y & <z>".to_owned())),
        ("Lines", PlistValue::String("a\rb\r\nc\nd".to_owned())),
    ]);
    // The exact document dd-plist wrote in probe `cdata-read` (its bytes=396 counts the
    // probe's display form, with each CR shown as the two characters `\r`).
    let xml = xml_of(&dict);
    assert_eq!(xml.replace('\r', "\\r").len(), 396);
    assert!(xml.contains("<string><![CDATA[https://h/?a=1&b=2]]></string>"));

    let back = dict_from_bytes(xml.as_bytes()).expect("reparse");
    assert_eq!(
        back.get_cloned("URL"),
        Some(PlistValue::String("https://h/?a=1&b=2".to_owned()))
    );
    assert_eq!(
        back.get_cloned("K<&]]>"),
        Some(PlistValue::String("x]]>y & <z>".to_owned()))
    );
    // CR/CRLF -> LF on read, inside CDATA and out.
    assert_eq!(
        back.get_cloned("Lines"),
        Some(PlistValue::String("a\nb\nc\nd".to_owned()))
    );
    let crlf = dict_from_bytes(
        b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>K</key><string>a\r\nb\rc</string></dict></plist>",
    )
    .expect("crlf parse");
    assert_eq!(crlf.get_cloned("K"), Some(PlistValue::String("a\nb\nc".to_owned())));

    // Hand-written CDATA: split `]]>` and mixed text+CDATA concatenate.
    let hand = dict_from_bytes(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key><![CDATA[A&B]]></key><string><![CDATA[cdata]]]]><![CDATA[>inside]]></string><key>Mixed</key><string>pre<![CDATA[<&]]>post</string></dict></plist>",
    )
    .expect("hand cdata");
    assert_eq!(
        hand.get_cloned("A&B"),
        Some(PlistValue::String("cdata]]>inside".to_owned()))
    );
    assert_eq!(
        hand.get_cloned("Mixed"),
        Some(PlistValue::String("pre<&post".to_owned()))
    );
}

/// R1-sol-2 (probe `encodings`): documents decode per BOM / declaration before parsing.
#[test]
fn xml_encodings_decode_like_dd_plist() {
    fn utf16(doc: &str, big_endian: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend_from_slice(if big_endian { &[0xfe, 0xff] } else { &[0xff, 0xfe] });
        }
        for unit in doc.encode_utf16() {
            out.extend_from_slice(&if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        out
    }
    let doc = |enc: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"{enc}\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>café</string></dict></plist>"
        )
    };
    let cafe = Some(PlistValue::String("café".to_owned()));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("utf-8", doc("UTF-8").into_bytes()),
        (
            "utf-8-bom",
            [[0xef, 0xbb, 0xbf].as_slice(), doc("UTF-8").as_bytes()].concat(),
        ),
        ("utf-16-bom-le", utf16(&doc("UTF-16"), false, true)),
        ("utf-16-bom-be", utf16(&doc("UTF-16"), true, true)),
        ("utf-16be-nobom", utf16(&doc("UTF-16BE"), true, false)),
        ("utf-16le-nobom", utf16(&doc("UTF-16LE"), false, false)),
        ("iso-8859-1", doc("ISO-8859-1").chars().map(|c| c as u8).collect()),
    ];
    for (name, bytes) in cases {
        let parsed = dict_from_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(parsed.get_cloned("CFBundleName"), cafe, "case {name}");
    }
    // UTF-32 is rejected by dd-plist's SAX front end too.
    let mut utf32 = vec![0xff, 0xfe, 0x00, 0x00];
    for c in doc("UTF-32").chars() {
        utf32.extend_from_slice(&(c as u32).to_le_bytes());
    }
    assert!(matches!(dict_from_bytes(&utf32), Err(PlistError::NotAPlist { .. })));
}

/// R1-sol-4 (probe `control-on-parse`): forbidden XML characters, literal or by
/// character reference, are a parse error, as dd-plist's SAX parser reports them.
#[test]
fn forbidden_xml_characters_are_not_a_plist() {
    let literal = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>x</key><string>a\u{1}b</string></dict></plist>";
    assert!(matches!(
        dict_from_bytes(literal.as_bytes()),
        Err(PlistError::NotAPlist { .. })
    ));
    let reference = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>a&#x1;b</string></dict></plist>";
    assert!(matches!(dict_from_bytes(reference), Err(PlistError::NotAPlist { .. })));
    // The same byte inside a *written* value never reaches the reader: the writer drops
    // it, so this stays a reader-only rejection.
    let dict = dict_with(vec![("K", PlistValue::String("a\u{1}b".to_owned()))]);
    let back = dict_from_bytes(xml_of(&dict).as_bytes()).expect("writer output parses");
    assert_eq!(back.get_cloned("K"), Some(PlistValue::String("ab".to_owned())));
}

fn value_of(body: &str) -> Result<PlistValue, PlistError> {
    let doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>V</key>{body}</dict></plist>"
    );
    dict_from_bytes(doc.as_bytes()).map(|d| d.get_cloned("V").expect("V present"))
}

/// R2-sol-1 (probe `number-text`): surrounding whitespace parses, as dd-plist's
/// `Double.parseDouble` fallback trims; the type stays the tag's (approved simplification:
/// dd-plist infers it from the text, so its ` 42 ` is a real 42.0).
#[test]
fn number_text_whitespace_parses_and_keeps_the_tag_type() {
    let cases: &[(&str, Option<PlistValue>)] = &[
        ("<integer> 42 </integer>", Some(PlistValue::Integer(42))),
        ("<integer>\n42\n</integer>", Some(PlistValue::Integer(42))),
        ("<integer>\t-7\t</integer>", Some(PlistValue::Integer(-7))),
        ("<real> 2.5 </real>", Some(PlistValue::Real(2.5))),
        ("<real>\n2.5\n</real>", Some(PlistValue::Real(2.5))),
        ("<integer>0x10</integer>", Some(PlistValue::Integer(16))),
        ("<integer> 0x10 </integer>", None),
        ("<integer> </integer>", None),
    ];
    for (body, expected) in cases {
        match expected {
            Some(value) => assert_eq!(value_of(body).ok().as_ref(), Some(value), "{body:?}"),
            None => assert!(matches!(value_of(body), Err(PlistError::NotAPlist { .. })), "{body:?}"),
        }
    }
}

/// The `=> OK ...`/`=> THROW` outcome of every line of a probe section in `expected.txt`,
/// keyed by the text between `prefix` and ` => `.
fn probe_outcomes(section: &str, prefix: &str) -> Vec<(String, Option<String>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/oracle/plist-probes/expected.txt"
    );
    let text = std::fs::read_to_string(path).expect("probe output readable");
    let start = text.find(&format!("PROBE {section}\n")).expect("probe section present");
    text[start..]
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("PROBE "))
        .map(|line| {
            let (label, outcome) = line.split_once(" => ").expect("probe line");
            let label = label.strip_prefix(prefix).expect("probe label prefix").to_owned();
            let ok = outcome
                .strip_prefix("OK java.lang.String(")
                .map(|v| v.strip_suffix(')').expect("closing paren").to_owned());
            (label, ok)
        })
        .collect()
}

/// R2-sol-2 (probe `boolean-content`): content is ignored, nested elements included, but
/// still parsed: a forbidden character reference or an undeclared entity fails.
#[test]
fn boolean_content_is_ignored_but_validated() {
    let outcomes = probe_outcomes("boolean-content", "bool ");
    assert_eq!(outcomes.len(), 10);
    for (body, java) in outcomes {
        let rust = value_of(&body);
        match java.as_deref() {
            Some("boolean(true)") => assert_eq!(rust.ok(), Some(PlistValue::Bool(true)), "{body}"),
            Some("boolean(false)") => assert_eq!(rust.ok(), Some(PlistValue::Bool(false)), "{body}"),
            None => assert!(matches!(rust, Err(PlistError::NotAPlist { .. })), "{body}"),
            Some(other) => panic!("unexpected probe outcome {other}"),
        }
    }
}

/// R2-sol-3 (probe `encoding-matrix`): every BOM / byte-pattern × declaration combination
/// the probe records, read back exactly as Java does (incl. a UTF-8 BOM with an ISO-8859-1
/// declaration giving `cafÃ©`, and UTF-16 without a matching declaration failing).
#[test]
fn bom_and_declaration_resolve_like_the_jvm() {
    fn encode(text: &str, family: &str) -> Vec<u8> {
        match family {
            "UTF-16LE" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "UTF-16BE" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            "ISO-8859-1" => text.chars().map(|c| c as u8).collect(),
            _ => text.as_bytes().to_vec(),
        }
    }
    let outcomes = probe_outcomes("encoding-matrix", "enc ");
    assert_eq!(outcomes.len(), 92);
    for (label, java) in outcomes {
        let (prefix, rest) = label.split_once(" decl=").expect("matrix label");
        let (decl, content) = match rest.split_once(' ') {
            Some((decl, "ascii-only")) => (decl, "cafe"),
            Some((decl, "latin1-byte")) => (decl, "café"),
            _ => (rest, "café"),
        };
        let (family, bom): (&str, &[u8]) = match prefix {
            "none" => ("UTF-8", &[]),
            "bom-utf8" => ("UTF-8", &[0xef, 0xbb, 0xbf]),
            "bom-utf16le" => ("UTF-16LE", &[0xff, 0xfe]),
            "bom-utf16be" => ("UTF-16BE", &[0xfe, 0xff]),
            "pattern-utf16le" => ("UTF-16LE", &[]),
            "pattern-utf16be" => ("UTF-16BE", &[]),
            other => panic!("unknown prefix {other}"),
        };
        let family = if rest.ends_with("latin1-byte") {
            "ISO-8859-1"
        } else {
            family
        };
        let declaration = if decl == "null" {
            String::new()
        } else {
            format!("<?xml version=\"1.0\" encoding=\"{decl}\"?>")
        };
        let doc = format!(
            "{declaration}<plist version=\"1.0\"><dict><key>CFBundleName</key><string>{content}</string></dict></plist>"
        );
        let bytes = [bom, encode(&doc, family).as_slice()].concat();
        let rust = dict_from_bytes(&bytes).map(|d| d.get_cloned("CFBundleName"));
        match java {
            Some(name) => assert_eq!(rust.ok(), Some(Some(PlistValue::String(name))), "{label}"),
            None => assert!(matches!(rust, Err(PlistError::NotAPlist { .. })), "{label}: {rust:?}"),
        }
    }
}

/// Probe `date-text`: only the whole-second UTC form parses, as dd-plist's
/// `SimpleDateFormat` pattern requires.
#[test]
fn fractional_second_and_offset_dates_are_not_a_plist() {
    let outcomes = probe_outcomes("date-text", "date ");
    assert_eq!(outcomes.len(), 5);
    for (text, java) in outcomes {
        let rust = value_of(&format!("<date>{text}</date>"));
        match java {
            Some(_) => assert!(matches!(rust, Ok(PlistValue::Date(_))), "{text}"),
            None => assert!(matches!(rust, Err(PlistError::NotAPlist { .. })), "{text}"),
        }
    }
}

/// Probe `lenient-java`: Java leniency deliberately not copied (approved simplifications in
/// the WP 4.5a findings, review round 2); every case is `NotAPlist` here.
#[test]
fn java_date_and_charset_leniency_is_not_copied() {
    let outcomes = probe_outcomes("lenient-java", "");
    assert_eq!(outcomes.len(), 4);
    for (label, java) in outcomes {
        assert!(java.is_some(), "{label}: Java accepts it");
        let rust = if let Some(date) = label.strip_prefix("date ") {
            value_of(&format!("<date>{date}</date>")).map(|_| ())
        } else {
            let doc = "<?xml version=\"1.0\" encoding=\"LATIN1\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>caf\u{e9}</string></dict></plist>";
            let bytes: Vec<u8> = doc.chars().map(|c| c as u8).collect();
            dict_from_bytes(&bytes).map(|_| ())
        };
        assert!(matches!(rust, Err(PlistError::NotAPlist { .. })), "{label}");
    }
}
