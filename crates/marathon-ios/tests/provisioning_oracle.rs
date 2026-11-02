//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContext.kt
//! and DeviceTracker.kt: differential assertions against actual-source JVM probe output.
mod support;
use marathon_config::devices::*;
use marathon_ios::device::{device_tracker::*, worker_provisioning_context::*};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use support::*;
fn witness(name: &str) -> Value {
    include_str!("../../../fixtures/oracle/provisioning/probes.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["case"] == name)
        .unwrap()["value"]
        .clone()
}
#[tokio::test]
async fn cache_boundaries_and_tracker_quirks_match_the_frozen_jvm() -> TestResult {
    for age in [9_999_999_999, 10_000_000_000, 10_000_000_001] {
        let h = Harness::new();
        let clock = Arc::new(NanoClock(AtomicI64::new(0)));
        let c = WorkerProvisioningContext::new(Transport::Local, clock.clone());
        c.refresh_with(h.worker.clone(), || async { Ok(inventory("old")) })
            .await?;
        clock.0.store(age, Ordering::SeqCst);
        let got = c
            .inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("new")) })
            .await?;
        assert_eq!(json!(got.devices.devices[0].udid), witness(&format!("ttl:{age}")));
        if age == 9_999_999_999 {
            h.worker.connected.store(false, Ordering::SeqCst);
            assert_eq!(
                json!(
                    c.inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("new")) })
                        .await?
                        .devices
                        .devices[0]
                        .udid
                ),
                witness("same-disconnected")
            );
        }
    }
    let mut tracker = DeviceTracker::new(logging().0);
    let original = worker("worker");
    let p = AppleTarget::SimulatorProfile(profile());
    tracker.update(&MarathonDevices {
        workers: vec![Worker {
            transport: original.clone(),
            devices: vec![p.clone(); 3],
        }],
    })?;
    tracker.update(&MarathonDevices { workers: vec![] })?;
    let mut changed = original.clone();
    if let Transport::Ssh { check_reachability, .. } = &mut changed {
        *check_reachability = false;
    }
    let updates = tracker.update(&MarathonDevices {
        workers: vec![Worker {
            transport: changed,
            devices: vec![p],
        }],
    })?;
    assert_eq!(
        json!(updates.keys().next() == Some(&original)),
        witness("tracker-first-transport")
    );
    assert_eq!(json!(updates[&original].len()), witness("tracker-retained-high-water"));
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn bounded_existing_and_profile_counts_match_jvm_tests() -> TestResult {
    for (profiles, method) in [
        (
            false,
            "existing simulators overlap and register exactly once after setup",
        ),
        (true, "new profiles overlap and are bounded"),
    ] {
        let h = Harness::new();
        if profiles {
            h.profiles(4).await?;
        } else {
            h.existing(&["existing-1", "existing-2", "existing-3", "existing-4"])
                .await?;
        }
        let expected = witness(&format!("test:SimulatorProvisionerTest:{method}"));
        assert_eq!(json!(h.creating.peak.load(Ordering::SeqCst)), expected["peak"]);
        assert_eq!(
            json!(
                h.registrations
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, _, _)| id.to_string_lossy())
                    .collect::<Vec<_>>()
            ),
            expected["registered"]
        );
        assert_eq!(json!(h.created.load(Ordering::SeqCst)), expected["created"]);
        assert_eq!(
            json!(h.connections.released.load(Ordering::SeqCst)),
            expected["released"]
        );
    }
    Ok(())
}
#[test]
fn approved_s3_witnesses_are_rejected_by_rust_without_mutating_the_tracker() {
    for targets in [
        vec![AppleTarget::Host, AppleTarget::Physical { udid: "host".into() }],
        vec![
            AppleTarget::Physical {
                udid: "com.apple.CoreSimulator.SimDeviceType.iPhone-X-".into(),
            },
            AppleTarget::SimulatorProfile(SimulatorProfile {
                device_type_id: "iPhone X".into(),
                runtime_id: None,
                new_name_prefix: "original".into(),
            }),
        ],
    ] {
        let mut tracker = WorkerTracker::new(worker("worker"), logging().0);
        assert!(tracker.update(&targets).is_err());
        assert_eq!(tracker.update(&[AppleTarget::Host]).unwrap().len(), 1);
    }
    assert_eq!(witness("S3-approved-divergence:host-physical"), json!({"updates":1}));
    assert_eq!(
        witness("S3-approved-divergence:profile-physical"),
        json!({"error":"java.lang.ClassCastException"})
    );
}

#[tokio::test]
async fn remaining_context_rows_match_actual_rust_cache_results() -> TestResult {
    let h = Harness::new();
    let clock = Arc::new(NanoClock(AtomicI64::new(i64::MAX - 2)));
    let c = WorkerProvisioningContext::new(Transport::Local, clock.clone());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("wrapped")) })
        .await?;
    clock.0.store(i64::MIN + 2, Ordering::SeqCst);
    let got = c
        .inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("fresh")) })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("wrapping-age"));

    let c = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("old")) })
        .await?;
    h.worker.connected.store(false, Ordering::SeqCst);
    let replacement = Harness::new();
    let got = c
        .inventory_with(replacement.worker.clone(), &h.context, || async {
            Ok(inventory("new"))
        })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("replacement-disconnected"));
    let throwing = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    throwing
        .refresh_with(h.worker.clone(), || async { Ok(inventory("loaded")) })
        .await?;
    *h.worker.on_connected_check.lock().unwrap() = Arc::new(|| panic!("closing"));
    let got = throwing
        .inventory_with(replacement.worker.clone(), &h.context, || async {
            Ok(inventory("after-throw"))
        })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("throwing-old-getter"));

    let c = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    let loads = std::sync::atomic::AtomicUsize::new(0);
    for shareable in [Some(false), None, Some(true), Some(true)] {
        let result = c
            .worker_facts(h.worker.clone(), &h.context, || async {
                loads.fetch_add(1, Ordering::SeqCst);
                shareable.map(facts).ok_or_else(|| error("partial").into())
            })
            .await;
        assert_eq!(result.is_ok(), shareable.is_some());
    }
    assert_eq!(
        json!(loads.load(Ordering::SeqCst)),
        witness("facts-unshareable-partial-complete")
    );
    let mut loads = 0;
    let original = worker("worker");
    let mut changed = original.clone();
    if let Transport::Ssh { check_reachability, .. } = &mut changed {
        *check_reachability = false;
    }
    for transport in [original, changed] {
        WorkerProvisioningContext::new(transport, h.clock.clone())
            .worker_facts(h.worker.clone(), &h.context, || async {
                loads += 1;
                Ok(facts(true))
            })
            .await?;
    }
    assert_eq!(json!(loads), witness("fresh-cycles-facts"));

    let c = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    let (send, receive) = tokio::sync::oneshot::channel();
    let earlier = c.refresh_with(h.worker.clone(), || async {
        receive.await.map_err(|e| error(&e.to_string()))?;
        Ok(inventory("old"))
    });
    tokio::pin!(earlier);
    assert!(futures_util::poll!(&mut earlier).is_pending());
    c.refresh_with(h.worker.clone(), || async { Ok(inventory("new")) })
        .await?;
    send.send(()).map_err(|_| "refresh receiver dropped")?;
    assert_eq!(
        json!(earlier.await?.devices.devices[0].udid),
        witness("refresh-earlier-own-output")
    );
    let got = c
        .inventory_with(h.worker.clone(), &h.context, || async {
            panic!("successful later refresh must survive")
        })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("refresh-publication"));
    let c = WorkerProvisioningContext::new(Transport::Local, h.clock.clone());
    let (send, receive) = tokio::sync::oneshot::channel();
    let earlier = c.refresh_with(h.worker.clone(), || async {
        receive.await.map_err(|e| error(&e.to_string()))?;
        Ok(inventory("older-success"))
    });
    tokio::pin!(earlier);
    assert!(futures_util::poll!(&mut earlier).is_pending());
    assert!(
        c.refresh_with(h.worker.clone(), || async { Err(error("later failure").into()) })
            .await
            .is_err()
    );
    send.send(()).map_err(|_| "refresh receiver dropped")?;
    earlier.await?;
    let got = c
        .inventory_with(h.worker.clone(), &h.context, || async {
            panic!("later failure must not hide earlier success")
        })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("refresh-later-failure"));

    let clock = Arc::new(NanoClock(AtomicI64::new(0)));
    let c = WorkerProvisioningContext::new(Transport::Local, clock.clone());
    c.refresh_with(h.worker.clone(), || async {
        clock.0.store(10_000_000_001, Ordering::SeqCst);
        Ok(inventory("slow"))
    })
    .await?;
    let got = c
        .inventory_with(h.worker.clone(), &h.context, || async { Ok(inventory("fresh")) })
        .await?;
    assert_eq!(json!(got.devices.devices[0].udid), witness("request-start-age"));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cleanup_trace_and_failures_match_the_jvm() -> TestResult {
    // Both probes use one permit, so command-runtime scheduling cannot reorder independent profiles.
    let h = Harness::with_concurrency(1);
    h.creating.fail_first.store(true, Ordering::SeqCst);
    let failures = h
        .provisioner
        .provision(
            marathon_ios::device::provisioning_plan::ProvisioningPlan {
                needs_provisioning: vec![profile(); 2],
                ..Default::default()
            },
            worker("worker"),
            h.bin.clone(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await?;
    assert_eq!(json!(*h.trace.lock().unwrap()), witness("cleanup-order"));
    let actual = failures
        .iter()
        .map(|f| match f {
            marathon_ios::device::simulator_provisioner::FailedSetup::Created { unreachable, .. } => {
                json!({"created":true,"unreachable":unreachable})
            }
            marathon_ios::device::simulator_provisioner::FailedSetup::Existing { unreachable, .. } => {
                json!({"created":false,"unreachable":unreachable})
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(json!(actual), witness("cleanup-failures"));
    Ok(())
}
