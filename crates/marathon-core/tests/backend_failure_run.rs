//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/TrackerFactory.kt
//! and execution/progress/PoolProgressAccumulator.kt (D51: whole runs with a failing analytics backend).
//!
//! Each runs the production run graph, including the real scheduler, with the Stub vendor. Without
//! `BackendGuard` the first test result ends the queue actor, and the run never ends.
use marathon_config::{AnalyticsConfiguration, Defaults, GraphiteConfiguration};
use marathon_core::{
    analytics::{
        Analytics, BackendFactory, BackendRequest, NoOpMetricsProvider, TrackerInternal, TrackerInternalAdapter,
    },
    error::*,
    executor::shared::failure,
    report::TestEvent,
    runtime::*,
};
use marathon_error::{AnalyticsKind, FailureKind};
use marathon_logging::Level;
use marathon_test_support::{
    console::Line,
    factory::MarathonFactory,
    scenario::{self, Outcome},
};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

const WARNING: &str =
    "Cannot send test metrics to the analytics backend; the run carries on. Each kind of failure is logged once";

struct Failing(Arc<AtomicU32>);
impl TrackerInternalAdapter for Failing {
    fn test<'a>(&'a self, _: &'a TestEvent) -> VendorFuture<'a, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(VendorError::Other(failure(
                FailureKind::Analytics(AnalyticsKind::Graphite),
                "Error while writing data to graphite: Connection refused",
            )))
        })
    }
}
struct Factory(Arc<AtomicU32>);
impl BackendFactory for Factory {
    fn create(&self, _: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>> {
        Box::pin(async { Ok(Ok(Some(Arc::new(Failing(self.0.clone())) as Arc<dyn TrackerInternal>))) })
    }
}
fn graphite(factory: &mut MarathonFactory) {
    factory.analytics = Some(Box::new(|context| {
        Ok(Analytics::noop(NoOpMetricsProvider::new(false, context)?))
    }));
    factory.configuration.configuration.analytics_configuration =
        AnalyticsConfiguration::Graphite(GraphiteConfiguration {
            host: "carbon.invalid".into(),
            port: None,
            prefix: None,
            defaults: Defaults {
                success_rate: 0.0,
                duration: jcompat::jtime::duration_of(0, 0).unwrap(),
                observed_duration_min_tests: None,
            },
            read_only: false,
        });
}
fn warnings(outcome: &Outcome) -> usize {
    outcome
        .lines
        .iter()
        .filter(|line| matches!(line, Line::Log { level: Level::WARN, message, .. } if message == WARNING))
        .count()
}

#[test]
fn a_failing_backend_does_not_end_the_queue_and_the_run_finishes_with_the_same_results() {
    for scenario_name in [
        "success.json",
        "filtering-2.json",
        "disconnect.json",
        "uncompleted-1.json",
    ] {
        let control = scenario::run(scenario_name, |_| {});
        let sends = Arc::new(AtomicU32::new(0));
        let backend = sends.clone();
        let failed = scenario::run(scenario_name, move |factory| {
            graphite(factory);
            factory.backend = Some(Arc::new(Factory(backend)));
        });
        assert!(control.completed, "{scenario_name}: control");
        assert!(failed.completed, "{scenario_name}: the run did not end");
        assert!(
            sends.load(Ordering::SeqCst) > 0,
            "{scenario_name}: never reached the backend"
        );
        assert_eq!(warnings(&failed), 1, "{scenario_name}: {:?}", failed.lines);
        assert_eq!(warnings(&control), 0, "{scenario_name}");
        assert_eq!(
            failed.value.as_ref().map(|v| v.as_ref().ok().copied()),
            control.value.as_ref().map(|v| v.as_ref().ok().copied()),
            "{scenario_name}: outcome"
        );
        assert_eq!(
            failed.completed_at, control.completed_at,
            "{scenario_name}: finish time"
        );
        assert_eq!(
            failed.read("test_result/raw.json"),
            control.read("test_result/raw.json"),
            "{scenario_name}: results"
        );
    }
}
