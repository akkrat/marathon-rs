//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorFactoryTest.kt
use marathon_error::{FailureKind, InjectedKind};
mod support;
use marathon_apple::{
    cmd::{CommandExecutor, cancelled, shared_transfers::SharedTransfers},
    model::Sdk,
};
use marathon_core::{
    analytics::Track, error::VendorError, runtime::VendorFuture, time::SystemTimer, vendor::VendorContext,
};
use marathon_ios::device::{error::*, simulator_factory::*, timing::ProvisioningTiming};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use support::*;
struct Setup {
    result: Mutex<Option<VendorError>>,
    records: Arc<Mutex<Vec<String>>>,
}
impl SimulatorSetup for Setup {
    fn setup<'a>(
        &'a self,
        record: &'a marathon_apple::bin::xcrun::simctl::model::SimctlDevice,
        _: Arc<marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.records.lock().unwrap().push(record.udid.clone());
            self.result.lock().unwrap().clone().map_or(Ok(()), Err)
        })
    }
}
struct Constructor {
    device: Arc<TestDevice>,
    setup: Arc<Setup>,
    constructs: AtomicUsize,
    transfers: Mutex<Vec<Arc<SharedTransfers>>>,
}
impl SimulatorConstructor for Constructor {
    type Handle = Arc<TestDevice>;
    fn construct(&self, input: SimulatorConstruction) -> ProvisioningResult<ConstructedSimulator<Self::Handle>> {
        self.constructs.fetch_add(1, Ordering::SeqCst);
        self.transfers.lock().unwrap().push(input.shared_transfers);
        assert_eq!(input.sdk, Sdk::IphoneSimulator);
        Ok(ConstructedSimulator {
            handle: self.device.clone(),
            device: self.device.device(),
            setup: self.setup.clone(),
        })
    }
}
fn factory(
    h: &Harness,
    outcome: Option<VendorError>,
    cleanup: Option<VendorError>,
) -> (SimulatorFactory<Arc<TestDevice>>, Arc<Constructor>) {
    let (logging, _) = logging();
    let device = TestDevice::new("udid");
    *device.dispose_error.lock().unwrap() = cleanup;
    let constructor = Arc::new(Constructor {
        device,
        setup: Arc::new(Setup {
            result: Mutex::new(outcome),
            records: Arc::default(),
        }),
        constructs: AtomicUsize::new(0),
        transfers: Mutex::default(),
    });
    let make_bin = logging.clone();
    let f = SimulatorFactory::new(
        Arc::new(move |executor| bin(executor, make_bin.clone())),
        constructor.clone(),
        Arc::new(SharedTransfers::new(Arc::new(Runtime), logging.clone())),
        Track::new(Arc::new(Runtime), h.clock.clone()),
        ProvisioningTiming::new(Arc::new(SystemTimer::new(h.clock.clone()).unwrap()), logging),
    );
    (f, constructor)
}
#[tokio::test]
async fn invalid_simulator_does_not_directly_close_the_shared_executor() -> TestResult {
    let h = Harness::new();
    let (factory, constructor) = factory(&h, None, None);
    let failure = factory
        .create(
            worker("worker"),
            h.worker.clone(),
            Arc::new(BridgeMock),
            "missing".into(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        failure.failure().message.unwrap().to_owned(),
        "udid missing is missing at local"
    );
    assert_eq!(constructor.constructs.load(Ordering::SeqCst), 0);
    assert_eq!(constructor.device.disposed.load(Ordering::SeqCst), 0);
    assert!(h.worker.connected());
    Ok(())
}
#[tokio::test]
async fn failed_setup_disposes_the_partially_initialized_device_without_closing_its_executor() -> TestResult {
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (factory, c) = factory(&h, Some(error("setup failed")), None);
    let failure = factory
        .create(
            worker("worker"),
            h.worker.clone(),
            Arc::new(BridgeMock),
            "udid".into(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.failure().message.unwrap().to_owned(), "setup failed");
    assert_eq!(c.device.disposed.load(Ordering::SeqCst), 1);
    assert!(h.worker.connected());
    Ok(())
}
#[tokio::test]
async fn cancelled_setup_disposes_the_partially_initialized_device_and_propagates_cancellation() -> TestResult {
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (factory, c) = factory(&h, Some(VendorError::Cancelled(cancelled("cancelled setup"))), None);
    assert!(
        factory
            .create(
                worker("worker"),
                h.worker.clone(),
                Arc::new(BridgeMock),
                "udid".into(),
                h.cycle.clone(),
                h.context.clone()
            )
            .await
            .unwrap_err()
            .is_cancelled()
    );
    assert_eq!(c.device.disposed.load(Ordering::SeqCst), 1);
    assert!(h.worker.connected());
    Ok(())
}
#[tokio::test]
async fn disposal_failure_is_suppressed_under_the_original_setup_failure() -> TestResult {
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (factory, _) = factory(&h, Some(error("setup")), Some(error("dispose")));
    let failure = factory
        .create(
            worker("worker"),
            h.worker.clone(),
            Arc::new(BridgeMock),
            "udid".into(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        serde_json::json!({"original":failure.failure().message.as_ref().unwrap().to_owned(), "suppressed":failure.suppressed.iter().map(|e| e.failure().message.as_ref().unwrap().to_owned()).collect::<Vec<_>>()}),
        witness("factory-suppressed")
    );
    assert_eq!(failure.failure().message.unwrap().to_owned(), "setup");
    assert_eq!(
        failure.suppressed[0].failure().message.as_ref().unwrap().to_owned(),
        "dispose"
    );
    assert!(
        failure
            .output_failure()
            .stack_trace()
            .to_owned()
            .contains("\tSuppressed: invalid state: dispose")
    );
    Ok(())
}
#[tokio::test]
async fn success_keeps_the_device_allocation_and_factory_shared_transfers() -> TestResult {
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (factory, c) = factory(&h, None, None);
    for _ in 0..2 {
        let built = factory
            .create(
                worker("worker"),
                h.worker.clone(),
                Arc::new(BridgeMock),
                "udid".into(),
                h.cycle.clone(),
                h.context.clone(),
            )
            .await?;
        assert_eq!(built.device, c.device.device());
        assert!(Arc::ptr_eq(&built.handle, &c.device));
    }
    let transfers = c.transfers.lock().unwrap();
    assert!(Arc::ptr_eq(&transfers[0], &transfers[1]));
    assert_eq!(c.device.disposed.load(Ordering::SeqCst), 0);
    assert_eq!(*c.setup.records.lock().unwrap(), vec!["udid", "udid"]);
    Ok(())
}
#[test]
fn simulator_sdk_matches_all_source_branches() {
    for (kind, sdk) in [
        (None, Sdk::IphoneSimulator),
        (Some("iPhone-X"), Sdk::IphoneSimulator),
        (Some("iPad-Pro"), Sdk::IphoneSimulator),
        (Some("Apple-TV-4K"), Sdk::TvSimulator),
        (Some("Apple-Watch"), Sdk::WatchSimulator),
        (Some("Apple-Vision-Pro"), Sdk::VisionSimulator),
        (Some("Other"), Sdk::IphoneSimulator),
    ] {
        assert_eq!(simulator_sdk(kind), sdk);
        if let Some(kind) = kind {
            assert_eq!(
                simulator_sdk(Some(&format!("com.apple.CoreSimulator.SimDeviceType.{kind}"))),
                sdk
            );
        }
    }
}

#[tokio::test]
async fn post_setup_timing_failure_still_disposes_the_device() -> TestResult {
    struct FailedTimer(AtomicUsize);
    impl marathon_core::time::Timer for FailedTimer {
        fn start_time_millis(&self) -> i64 {
            0
        }
        fn elapsed_time_millis(&self) -> marathon_core::time::ClockResult<i64> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(0)
            } else {
                Err(marathon_core::time::ReplayError {
                    label: "core.di.Clock".into(),
                    message: "clock exhausted".into(),
                })
            }
        }
        fn current_time_millis(&self) -> marathon_core::time::ClockResult<i64> {
            self.elapsed_time_millis()
        }
        fn measure(&self, _: &mut dyn FnMut()) -> marathon_core::time::ClockResult<i64> {
            unreachable!()
        }
    }
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (logging, _) = logging();
    let device = TestDevice::new("udid");
    let c = Arc::new(Constructor {
        device: device.clone(),
        setup: Arc::new(Setup {
            result: Mutex::new(None),
            records: Arc::default(),
        }),
        constructs: AtomicUsize::new(0),
        transfers: Mutex::default(),
    });
    let make_bin = logging.clone();
    let factory = SimulatorFactory::new(
        Arc::new(move |executor| bin(executor, make_bin.clone())),
        c,
        Arc::new(SharedTransfers::new(Arc::new(Runtime), logging.clone())),
        Track::new(Arc::new(Runtime), h.clock.clone()),
        ProvisioningTiming::new(Arc::new(FailedTimer(AtomicUsize::new(0))), logging),
    );
    let failure = factory
        .create(
            worker("worker"),
            h.worker.clone(),
            Arc::new(BridgeMock),
            "udid".into(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.failure().message.unwrap().to_owned(), "clock exhausted");
    assert_eq!(device.disposed.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn bridge_error_conversion_preserves_typed_cancellation_and_suppressed_disposal() {
    let reason = marathon_core::error::Cancellation::Timeout {
        scope: 42,
        detail: marathon_apple::cmd::failure(FailureKind::TimeoutCancelled, "setup timeout"),
    };
    let mut error: ProvisioningError = VendorError::Cancelled(reason).into();
    error.suppressed.push(support::error("dispose").into());
    let converted = error.into_vendor();
    assert!(matches!(
        converted,
        VendorError::Cancelled(marathon_core::error::Cancellation::Timeout { scope: 42, .. })
    ));
    assert!(
        converted
            .failure()
            .stack_trace()
            .to_owned()
            .contains("Suppressed: invalid state: dispose")
    );
}

#[test]
fn leave_pool_keeps_its_classification_and_supplies_sibling_cancellation() {
    let error: ProvisioningError = VendorError::LeavePool {
        reason: marathon_core::error::LeavePoolReason::NotAdmitted,
        detail: marathon_apple::cmd::failure(FailureKind::Injected(InjectedKind::NotAdmitted), "not admitted"),
    }
    .into();
    assert!(error.is_cancelled());
    assert!(error.cancellation().is_some());
    assert!(matches!(
        error.into_vendor(),
        VendorError::LeavePool {
            reason: marathon_core::error::LeavePoolReason::NotAdmitted,
            ..
        }
    ));
}

#[tokio::test]
async fn factory_timing_read_order_matches_the_jvm() -> TestResult {
    struct Tape(Mutex<std::collections::VecDeque<i64>>);
    impl marathon_core::time::WallClock for Tape {
        fn millis(&self, _: &'static str) -> marathon_core::time::ClockResult<i64> {
            Ok(self.0.lock().unwrap().pop_front().unwrap())
        }
        fn instant(&self, _: &'static str) -> marathon_core::time::ClockResult<jcompat::jtime::Instant> {
            unreachable!()
        }
    }
    let h = Harness::new();
    h.worker.add(["udid"]);
    let (_, template) = factory(&h, None, None);
    let constructor = Arc::new(Constructor {
        device: TestDevice::new("udid@local"),
        setup: template.setup.clone(),
        constructs: AtomicUsize::new(0),
        transfers: Mutex::default(),
    });
    let (logging, logs) = logging();
    let make_bin = logging.clone();
    let clock = Arc::new(Tape(Mutex::new([1000, 1010, 1020, 1030].into())));
    let factory = SimulatorFactory::new(
        Arc::new(move |executor| bin(executor, make_bin.clone())),
        constructor.clone(),
        Arc::new(SharedTransfers::new(Arc::new(Runtime), logging.clone())),
        Track::new(Arc::new(Runtime), h.clock.clone()),
        ProvisioningTiming::new(Arc::new(SystemTimer::new(clock.clone()).unwrap()), logging),
    );
    factory
        .create(
            worker("worker"),
            h.worker.clone(),
            Arc::new(BridgeMock),
            "udid".into(),
            h.cycle.clone(),
            h.context.clone(),
        )
        .await?;
    let message = logs
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _, _, _)| name == "Timings")
        .unwrap()
        .2
        .clone();
    assert_eq!(serde_json::json!(message), witness("factory-timing-read-order"));
    assert!(clock.0.lock().unwrap().is_empty());
    assert_eq!(constructor.constructs.load(Ordering::SeqCst), 1);
    Ok(())
}
