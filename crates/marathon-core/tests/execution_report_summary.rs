//! U263: `ExecutionReport::summary()` in linear time with the same output. No Kotlin counterpart; the pinned
//! output follows the Kotlin marathon's ExecutionReport.kt (distinct pools and devices in first-seen order,
//! `toSet()` name lists in first-seen order, `toMap()` retries keeping the first key's position and the last value).
mod event_support;
use event_support::*;
use jcompat::jtime::instant_of;
use marathon_core::{model::*, report::*};
use std::{fmt::Write, sync::Arc, time::Duration};

fn device_named(serial: &str) -> DeviceInfo {
    DeviceInfo {
        serial_number: serial.into(),
        ..device()
    }
}
fn connected(pool_name: &str, serial: &str) -> DeviceConnectedEvent {
    DeviceConnectedEvent {
        instant: instant_of(0, 0).unwrap(),
        pool_id: pool(pool_name),
        device: device_named(serial),
    }
}
fn timed(method: &str, status: TestStatus, start: i64, end: i64) -> Arc<TestResult> {
    Arc::new(TestResult {
        start_time: start,
        end_time: end,
        ..result(method, status)
    })
}
fn in_pool(pool_name: &str, result: &Arc<TestResult>, final_result: bool) -> TestEvent {
    TestEvent {
        pool_id: pool(pool_name),
        ..event(result.clone(), final_result, 0)
    }
}

/// Renders everything a reporter can read from the summary; result identity is the position in `all`.
fn render(summary: &Summary, all: &[Arc<TestResult>]) -> String {
    let id = |r: &Arc<TestResult>| match all.iter().position(|a| Arc::ptr_eq(a, r)) {
        Some(i) => format!("r{i}"),
        None => "r?".into(),
    };
    let mut out = String::new();
    for p in &summary.pools {
        let serials: Vec<_> = p.devices.iter().map(|d| d.serial_number.as_str()).collect();
        writeln!(out, "pool {} devices {serials:?}", p.pool_id.name).unwrap();
        let tests: Vec<_> = p.tests.iter().map(id).collect();
        writeln!(out, " tests {tests:?}").unwrap();
        for (key, attempts) in &p.retries {
            let attempts: Vec<_> = attempts
                .iter()
                .map(|e| format!("{}{}", id(&e.test_result), if e.final_result { "*" } else { "" }))
                .collect();
            writeln!(out, " retry {} -> {attempts:?}", id(key)).unwrap();
        }
        writeln!(out, " passed {:?}", p.passed).unwrap();
        writeln!(out, " ignored {:?}", p.ignored).unwrap();
        writeln!(out, " failed {:?}", p.failed).unwrap();
        writeln!(out, " raw_passed {:?}", p.raw_passed).unwrap();
        writeln!(out, " raw_ignored {:?}", p.raw_ignored).unwrap();
        writeln!(out, " raw_failed {:?}", p.raw_failed).unwrap();
        writeln!(out, " raw_incomplete {:?}", p.raw_incomplete).unwrap();
        writeln!(
            out,
            " duration {} raw {} flaky {}",
            p.duration_millis, p.raw_duration_millis, p.flaky
        )
        .unwrap();
    }
    out
}

#[test]
fn summary_keeps_first_seen_order_and_map_replacement_semantics() {
    // r2 and r3 are equal by value but distinct allocations: one retries key at r2's position, r3's attempts.
    // r5 is final twice through the same allocation. r9 belongs to a pool with no connected device.
    let all = vec![
        timed("b", TestStatus::Failure, 10, 20),         // r0
        timed("a", TestStatus::Passed, 1, 4),            // r1
        timed("b", TestStatus::Passed, 30, 45),          // r2
        timed("b", TestStatus::Passed, 30, 45),          // r3
        timed("c", TestStatus::Incomplete, 50, 0),       // r4
        timed("d", TestStatus::AssumptionFailure, 5, 6), // r5
        timed("e", TestStatus::Ignored, 0, 0),           // r6
        timed("a", TestStatus::Failure, 2, 9),           // r7
        timed("f", TestStatus::Failure, 100, 107),       // r8
        timed("g", TestStatus::Passed, 1, 2),            // r9
        timed("c", TestStatus::Failure, 60, 61),         // r10
        timed("a", TestStatus::Passed, 11, 13),          // r11
    ];
    let r = |i: usize| &all[i];
    let report = ExecutionReport::new(
        vec![
            connected("second", "s1"),
            connected("first", "f1"),
            connected("second", "s2"),
            connected("second", "s1"),
            connected("first", "f2"),
        ],
        vec![],
        vec![],
        vec![],
        vec![
            in_pool("first", r(0), false),
            in_pool("second", r(8), true),
            in_pool("first", r(7), true),
            in_pool("first", r(2), true),
            in_pool("first", r(4), true),
            in_pool("orphan", r(9), true),
            in_pool("first", r(5), true),
            in_pool("first", r(3), true),
            in_pool("first", r(1), true),
            in_pool("first", r(6), true),
            in_pool("first", r(5), true),
            in_pool("first", r(10), false),
            in_pool("second", r(7), false),
            in_pool("first", r(11), false),
        ],
    );
    let expected = r#"pool second devices ["s1", "s2"]
 tests ["r8"]
 retry r8 -> []
 passed []
 ignored []
 failed ["com.example#f"]
 raw_passed []
 raw_ignored []
 raw_failed ["com.example#f", "com.example#a"]
 raw_incomplete []
 duration 7 raw 14 flaky 0
pool first devices ["f1", "f2"]
 tests ["r7", "r2", "r5", "r3", "r1", "r6", "r5"]
 retry r7 -> ["r1*", "r11"]
 retry r2 -> ["r0", "r2*"]
 retry r5 -> []
 retry r1 -> ["r7*", "r11"]
 retry r6 -> []
 passed ["com.example#b", "com.example#a"]
 ignored ["com.example#d", "com.example#e"]
 failed ["com.example#a"]
 raw_passed ["com.example#b", "com.example#b", "com.example#a", "com.example#a"]
 raw_ignored ["com.example#d", "com.example#e", "com.example#d"]
 raw_failed ["com.example#b", "com.example#a", "com.example#c"]
 raw_incomplete ["com.example#c"]
 duration 42 raw 55 flaky 0
"#;
    assert_eq!(render(report.summary(), &all), expected);
}

/// Hang guard only (docs/testing.md): this takes well under a second in a debug build; the quadratic summary
/// took about 34 s.
#[test]
fn summary_of_fifty_thousand_results_finishes() {
    let pools = ["p0", "p1"];
    let mut events = Vec::new();
    for i in 0..50_000 {
        let method = format!("test{:05}", i % 40_000);
        let status = match i % 7 {
            0 => TestStatus::Failure,
            1 => TestStatus::Ignored,
            _ => TestStatus::Passed,
        };
        let result = timed(&method, status, 1, 2);
        events.push(in_pool(pools[i % 2], &result, i % 5 != 0));
    }
    let report = ExecutionReport::new(
        vec![connected("p0", "d0"), connected("p1", "d1")],
        vec![],
        vec![],
        vec![],
        events,
    );
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let tests: usize = report.summary().pools.iter().map(|p| p.tests.len()).sum();
        let _ = done.send(tests);
    });
    assert_eq!(finished.recv_timeout(Duration::from_secs(10)), Ok(40_000));
}
