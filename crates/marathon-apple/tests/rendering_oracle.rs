//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt and
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/test/TestEvent.kt.

mod support;
use marathon_apple::test::TestEvent;
use marathon_core::model::{MetaProperty, MetaValue, Test};
use serde_json::Value;
use support::rendering::{event_text, test_text};

fn rows() -> Result<Vec<Value>, serde_json::Error> {
    include_str!("../../../fixtures/oracle/jcompat/kotlin_to_string.jsonl")
        .lines()
        .skip(1)
        .map(serde_json::from_str)
        .collect()
}

// Moved from jcompat: the same event witnesses now exercise the typed adapter.
#[test]
fn kotlin_events_oracle() -> Result<(), Box<dyn std::error::Error>> {
    let test = Test {
        pkg: "sample_appUITests".into(),
        clazz: "FlakyTests".into(),
        method: "testTextFlaky1".into(),
        meta_properties: vec![],
    };
    let rows = rows()?;
    for (id, event) in [
        ("TestStarted", TestEvent::TestStarted { id: test.clone() }),
        (
            "TestPassed",
            TestEvent::TestPassed {
                id: test.clone(),
                start_time: 1537187691585,
                end_time: 1537187696000,
            },
        ),
        (
            "TestFailed",
            TestEvent::TestFailed {
                id: test.clone(),
                start_time: 1,
                end_time: 2,
                trace: Some("a\nb, c=(d)".into()),
            },
        ),
        (
            "TestFailed-null",
            TestEvent::TestFailed {
                id: test.clone(),
                start_time: 1,
                end_time: 2,
                trace: None,
            },
        ),
        (
            "TestIgnored",
            TestEvent::TestIgnored {
                id: test,
                start_time: 0,
                end_time: 0,
            },
        ),
    ] {
        assert_eq!(
            event_text(&event),
            rows.iter().find(|r| r["id"] == id).unwrap()["toString"]
                .as_str()
                .unwrap()
        );
    }
    Ok(())
}

#[test]
fn recorded_test_metadata_keeps_its_historical_text() -> Result<(), Box<dyn std::error::Error>> {
    let test = Test {
        pkg: "p".into(),
        clazz: "C".into(),
        method: "m".into(),
        meta_properties: vec![
            MetaProperty {
                name: "A".into(),
                values: Default::default(),
            },
            MetaProperty {
                name: "B".into(),
                values: [
                    ("v".into(), MetaValue::Int(1)),
                    ("n".into(), MetaValue::Null),
                    (
                        "l".into(),
                        MetaValue::List(vec![MetaValue::Double(1.0), MetaValue::String("x".into())]),
                    ),
                ]
                .into_iter()
                .collect(),
            },
        ],
    };
    let rows = rows()?;
    assert_eq!(
        test_text(&test),
        rows.iter().find(|r| r["id"] == "Test-meta").unwrap()["toString"]
            .as_str()
            .unwrap()
    );
    Ok(())
}
