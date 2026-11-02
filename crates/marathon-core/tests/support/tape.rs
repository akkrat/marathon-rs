//! Port of tools/oracle/hooks/src/main/kotlin/oracle/hooks/Tape.kt (test-only subset).
use marathon_core::executor::hooks::{ClockMode, Ids, Tape};
pub fn scenario_tape() -> Tape {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("../../../../tools/oracle/scenario-tape.json")).unwrap();
    assert_eq!(json["format"], "marathon-replay/tape@1");
    let instant = jcompat::jtime::parse_instant(json["clocks"]["wall"]["start"].as_str().unwrap()).unwrap();
    let mut tape = Tape {
        wall: ClockMode::Scheduler(instant.seconds() * 1000),
        monotonic: ClockMode::Scheduler(json["clocks"]["monotonic"]["startNanos"].as_i64().unwrap()),
        ..Tape::default()
    };
    for (label, spec) in json["ids"].as_object().unwrap() {
        let ids = if let Some(values) = spec["values"].as_array() {
            Ids::Values(values.iter().map(|v| v.as_str().unwrap().into()).collect())
        } else {
            let pattern = spec["pattern"].as_str().unwrap();
            let (prefix, format) = pattern.split_once('%').unwrap();
            let position = format.find(['d', 'x']).unwrap();
            Ids::Pattern {
                prefix: prefix.into(),
                width: format[..position].parse().unwrap(),
                hex: format.as_bytes()[position] == b'x',
                suffix: format[position + 1..].into(),
                start: spec["start"].as_i64().unwrap_or(1),
            }
        };
        tape.ids.insert(label.clone(), ids);
    }
    tape
}
