//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/DeviceTrackerTest.kt
mod support;
use jcompat::{file_manager::JavaPath, jtime::instant_of};
use marathon_config::{
    binding::{BindingContext, Profile},
    devices::*,
    pipeline::bind_devices_bytes,
};
use marathon_ios::device::device_tracker::*;
use std::path::Path;
use support::*;
fn fixture(name: &str) -> Result<MarathonDevices, Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vendor-apple-ios/fixtures/marathondevices")
        .join(name);
    let bytes = std::fs::read(path)?;
    let context = BindingContext {
        profile: Profile::Marathondevices,
        marathonfile: JavaPath::from_text("/tmp/Marathonfile"),
        marathonfile_dir: JavaPath::from_text("/tmp"),
        process_dir: JavaPath::from_text("/tmp"),
        environment: vec![],
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    bind_devices_bytes(&bytes, &context).map_err(|e| format!("P2 binding failed: {e:?}").into())
}
#[test]
fn test_worker_simple() -> TestResult {
    let data = fixture("all.yaml")?;
    let worker = &data.workers[0];
    let mut tracker = WorkerTracker::new(worker.transport.clone(), logging().0);
    let updates = tracker.update(&worker.devices)?;
    assert_eq!(updates.len(), 4);
    for target in &worker.devices {
        assert!(updates.contains(&(target.clone(), TrackingUpdate::Connected(target.clone()))));
    }
    assert!(tracker.update(&worker.devices)?.is_empty());
    let data2 = fixture("allx2.yaml")?;
    let added = tracker.update(&data2.workers[0].devices)?;
    assert_eq!(added.len(), 3);
    assert!(added.iter().all(|(_, u)| matches!(u, TrackingUpdate::Connected(_))));
    Ok(())
}
#[test]
fn test_simulator_profile_scaling() -> TestResult {
    let data = fixture("simulatorprofile.yaml")?;
    let mut tracker = WorkerTracker::new(data.workers[0].transport.clone(), logging().0);
    for (name, count) in [
        ("simulatorprofile.yaml", 1),
        ("simulatorprofilex3.yaml", 2),
        ("simulatorprofilex2.yaml", 0),
        ("simulatorprofilex4.yaml", 1),
    ] {
        let data = fixture(name)?;
        let updates = tracker.update(&data.workers[0].devices)?;
        assert_eq!(updates.len(), count);
        assert!(updates.iter().all(|(t, u)| *u == TrackingUpdate::Connected(t.clone())));
    }
    Ok(())
}
#[test]
fn first_transport_and_high_water_count_survive_worker_removal_and_reappearance() -> TestResult {
    let mut tracker = DeviceTracker::new(logging().0);
    let first = worker("worker");
    let target = AppleTarget::SimulatorProfile(profile());
    let mut snapshot = MarathonDevices {
        workers: vec![Worker {
            transport: first.clone(),
            devices: vec![target.clone(); 3],
        }],
    };
    assert_eq!(tracker.update(&snapshot)?[&first].len(), 3);
    let empty = tracker.update(&MarathonDevices { workers: vec![] })?;
    assert!(empty[&first].is_empty());
    if let Transport::Ssh { check_reachability, .. } = &mut snapshot.workers[0].transport {
        *check_reachability = false;
    }
    snapshot.workers[0].devices = vec![target];
    let updates = tracker.update(&snapshot)?;
    assert!(updates[&first].is_empty());
    assert!(!updates.contains_key(&snapshot.workers[0].transport));
    Ok(())
}
#[test]
fn removals_precede_stable_targets_then_profiles_and_cross_kind_collisions_are_rejected() -> TestResult {
    let mut tracker = WorkerTracker::new(worker("worker"), logging().0);
    let a = AppleTarget::Simulator { udid: "a".into() };
    let b = AppleTarget::Physical { udid: "b".into() };
    let p = AppleTarget::SimulatorProfile(profile());
    tracker.update(std::slice::from_ref(&a))?;
    let updates = tracker.update(&[p.clone(), b.clone(), b.clone()])?;
    assert_eq!(
        updates,
        vec![
            (a.clone(), TrackingUpdate::Disconnected(a)),
            (b.clone(), TrackingUpdate::Connected(b)),
            (p.clone(), TrackingUpdate::Connected(p))
        ]
    );
    assert!(
        tracker
            .update(&[AppleTarget::Host, AppleTarget::Physical { udid: "host".into() }])
            .is_err()
    );
    let id = target_id(&AppleTarget::SimulatorProfile(profile()));
    assert!(tracker.update(&[AppleTarget::Physical { udid: id }]).is_err());
    Ok(())
}
