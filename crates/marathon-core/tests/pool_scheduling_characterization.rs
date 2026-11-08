//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/DevicePoolActorSchedulingCharacterizationTest.kt
#[path = "support/pool.rs"]
mod support;
use marathon_core::model::TestStatus;
use std::sync::{Arc, Mutex};
use support::*;
#[test]
fn devices_stay_busy_while_the_queue_has_work() {
    println!("SCHEDULING_CHARACTERIZATION devices tests firstWave makespanMs idleDeviceMs batchRequests wakeUps");
    for count in [1, 8, 32, 64, 128] {
        scheduling_test(move |h| {
            Box::pin(async move {
                let prepared = Gate::default();
                let spans = Arc::new(Mutex::new(Vec::new()));
                let devices = (1..=count)
                    .map(|i| {
                        let spans = spans.clone();
                        TestDevice::new(
                            &format!("device-{i}"),
                            None,
                            Some(prepare_on(&prepared)),
                            on_execute(move |e| {
                                let spans = spans.clone();
                                async move {
                                    let start = e.context.hooks.millis("test.start").unwrap();
                                    e.context
                                        .clock
                                        .delay(1000 * e.batch.tests.len() as i64, e.context.cancellation.clone())
                                        .await?;
                                    lock(&spans).push((start, e.context.hooks.millis("test.end").unwrap()));
                                    e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
                                    Ok(())
                                }
                            }),
                        )
                    })
                    .collect::<Vec<_>>();
                let pool = h.start_pool(
                    tests(count * 4, "Characterization"),
                    PoolOptions {
                        pool: format!("characterization-{count}"),
                        ..Default::default()
                    },
                );
                for d in &devices {
                    pool.add(d);
                }
                h.run_current().await;
                let start = h.now();
                prepared.open();
                h.run_current().await;
                let first_wave = devices.iter().filter(|d| !d.batches().is_empty()).count();
                h.await_pool_termination().await;
                let makespan = lock(&spans).iter().map(|(_, end)| *end).max().unwrap() - start;
                let idle = count as i64 * makespan - (count * 4) as i64 * 1000;
                let logs = h.logs();
                let requests = logs
                    .iter()
                    .filter(|r| r.message.to_owned().starts_with("request next batch for device"))
                    .count();
                let wakes = logs.iter().filter(|r| r.message.to_owned() == "Notify devices").count() * count;
                println!(
                    "SCHEDULING_CHARACTERIZATION {count} {} {first_wave} {makespan} {idle} {requests} {wakes}",
                    count * 4
                );
                let row: serde_json::Value = include_str!("../../../fixtures/oracle/pool/tests.jsonl")
                    .lines()
                    .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
                    .find(|v| v["name"] == "devices_stay_busy_while_the_queue_has_work")
                    .unwrap();
                let expected = row["characterization"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r[0] == count)
                    .unwrap();
                assert_eq!(
                    &serde_json::json!([count, count * 4, first_wave, makespan, idle]),
                    expected
                );
                assert_eq!(first_wave, count);
                assert_eq!(makespan, 4000);
                assert_eq!(idle, 0);
                assert!(pool.terminated());
                assert!(pool.aggregate_result().await.unwrap());
            })
        });
    }
}
