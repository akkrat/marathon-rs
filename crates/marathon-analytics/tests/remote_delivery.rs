//! D86 witnesses using the production report pipeline, scheduler and backend trackers.
mod support;

use marathon_analytics::{
    graphite::{BasicGraphiteClient, DataSender, GraphiteTracker},
    influx::{BATCH_ACTIONS, InfluxClient, InfluxTracker},
};
use marathon_config::*;
use marathon_core::{
    analytics::*,
    error::*,
    executor::{CoreExecutor, Signal, bridge::call_vendor},
    model::{ExecutionCommand, TestStatus},
    runtime::*,
    time::ReplayDriver,
    vendor::DeviceEvent,
};
use marathon_test_support::{
    factory::MarathonFactory,
    scenario,
    stub_device::{Recorder, StubDevice, StubDeviceOptions},
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{FixedBackend, Mono, Peer, Reply, defaults, v1};
use tokio::sync::{Notify, watch};

const WATCHDOG: Duration = Duration::from_secs(60);

struct HeldCarbon {
    entered: Notify,
    ended: Notify,
    attempts: AtomicUsize,
    release: watch::Receiver<bool>,
    sent: Mutex<Vec<String>>,
}

struct SendEnded<'a>(&'a Notify);
impl Drop for SendEnded<'_> {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl DataSender for HeldCarbon {
    fn send_data<'a>(
        &'a self,
        _: &'a str,
        _: u16,
        data: String,
        _: Arc<dyn CancellationSignal>,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            // A small Carbon payload fits a loopback TCP buffer even if its peer never reads.
            // Hold the real tracker's send at the socket adapter boundary instead.
            let _ended = SendEnded(&self.ended);
            self.attempts.fetch_add(1, Ordering::Relaxed);
            self.entered.notify_one();
            let _ = self.release.clone().wait_for(|ready| *ready).await;
            self.sent.lock().unwrap().push(data);
            Ok(())
        })
    }
}

type BackendSetup<T> = (AnalyticsConfiguration, Arc<dyn TrackerInternal>, T);

#[derive(Clone, Copy)]
struct Batches {
    results: i32,
    size: i32,
    starts_while_held: usize,
    send_limit: Option<Duration>,
}
const SMALL_BATCHES: Batches = Batches {
    results: 2,
    size: 1,
    starts_while_held: 2,
    send_limit: None,
};

fn run_batches<T: Send + 'static>(
    batches: Batches,
    setup: impl FnOnce(MetricsContext) -> ServiceFuture<'static, BackendSetup<T>> + Send + 'static,
    release: impl for<'a> FnOnce(&'a T) -> ServiceFuture<'a, ()> + Send + 'static,
    finish: impl FnOnce(T) -> ServiceFuture<'static, ()> + Send + 'static,
) {
    let (_temp, output) = CoreExecutor::run_virtual_with_watchdog(WATCHDOG, move |core, replay| {
        Box::pin(async move {
            let mut factory = MarathonFactory::new();
            let output = factory.configuration.output_dir().join("run");
            factory.configuration.configuration.output_dir = output.clone();
            factory.configuration.configuration.batching_strategy = BatchingStrategyConfiguration::FixedSize {
                size: batches.size,
                duration_millis: None,
                percentile: None,
                time_limit: None,
                last_mile_length: 0,
            };
            let tests = (0..batches.results)
                .map(|i| scenario::test("pkg", "Clazz", &format!("test{i}")))
                .collect::<Vec<_>>();
            factory.tests = tests.clone();
            factory.analytics = Some(Box::new(|context| {
                Ok(Analytics::noop(NoOpMetricsProvider::new(false, context)?))
            }));
            let device = StubDevice::new(StubDeviceOptions {
                prepare_time_millis: 10,
                test_time_millis: 100,
                ..StubDeviceOptions::default()
            });
            device.set_execution_results(tests.into_iter().map(|test| (test, vec![TestStatus::Passed])).collect());
            let calls = Arc::new(Mutex::new(Vec::new()));
            device.record_to(Recorder {
                log: calls.clone(),
                now: core.clock().now_source(),
            });
            let io = core.vendor();
            let context = MetricsContext {
                clock: core.vendor_clock(),
                vendor: io.clone(),
                monotonic: Arc::new(Mono),
                cancellation: Signal::new(),
                registry: core.logging(),
                logs: core.log_queue().sender(),
                stamp: Arc::new(|| (0, 0)),
            };
            let (configuration, tracker, evidence) = call_vendor(io.as_ref(), Signal::new(), move |_| {
                Box::pin(async move {
                    setup(context).await.map_err(|error| match error {
                        OperationError::Cancelled(reason) => VendorError::Cancelled(reason),
                        OperationError::Failed(failure) => VendorError::Other(failure),
                    })
                })
            })
            .await
            .map_err(|e| e.failure().clone())?;
            factory.configuration.configuration.analytics_configuration = configuration;
            let mut limits = BackendLimits::default();
            if let Some(send) = batches.send_limit {
                limits.send = send;
            }
            factory.backend = Some(Arc::new(FixedBackend(tracker, limits)));
            let built = factory.build(&core).await?;
            let job = core.job(JobKind::Ordinary, None);
            let run = core.spawn(
                job,
                Box::new(move |task| {
                    Box::pin(async move {
                        assert!(built.marathon.run_under(ExecutionCommand::Run, Some(task.job)).await?);
                        Ok(())
                    })
                }),
            );
            replay.advance_to(1).await.unwrap();
            assert!(built.provider.send(DeviceEvent::Connected(device.reference())));
            replay.advance_to(150).await.unwrap();
            let started = || {
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|entry| entry["call"] == "execute")
                    .count()
            };
            assert_eq!(
                started(),
                batches.starts_while_held,
                "batches started while the backend is held"
            );
            release(&evidence).await?;
            replay.drain_current().await.unwrap();
            assert_eq!(started(), 2, "the next batch starts once the first wait ends");
            replay.advance_to(500).await.unwrap();
            tokio::time::timeout(WATCHDOG, run.join()).await.unwrap()?;
            assert_eq!(run.state(), JobState::Completed(Completion::Success));
            finish(evidence).await?;
            Ok((factory.configuration.output.take(), output))
        })
    })
    .unwrap();
    let raw = std::fs::read(output.join("test_result/raw.json")).unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&raw).unwrap();
    assert_eq!(rows.len(), batches.results as usize);
    assert!(rows.iter().all(|row| row["status"] == "PASSED"));
    assert_eq!(
        rows.iter()
            .map(|row| row["method"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>(),
        (0..batches.results).map(|i| format!("test{i}")).collect(),
    );
}

fn held_carbon() -> (Arc<HeldCarbon>, watch::Sender<bool>) {
    let (release, released) = watch::channel(false);
    let carbon = Arc::new(HeldCarbon {
        entered: Notify::new(),
        ended: Notify::new(),
        attempts: AtomicUsize::new(0),
        release: released,
        sent: Mutex::default(),
    });
    (carbon, release)
}

fn carbon_tracker(
    context: MetricsContext,
    sender: Arc<HeldCarbon>,
) -> (AnalyticsConfiguration, Arc<dyn TrackerInternal>) {
    let config = GraphiteConfiguration {
        host: "carbon.invalid".into(),
        port: None,
        prefix: None,
        defaults: defaults(),
        read_only: false,
    };
    let tracker = Arc::new(GraphiteTracker {
        client: BasicGraphiteClient {
            host: config.host.clone(),
            port: 2003,
            prefix: None,
            sender,
            cancellation: context.cancellation,
        },
        read_only: false,
    });
    (AnalyticsConfiguration::Graphite(config), tracker)
}

#[test]
fn a_held_carbon_send_cannot_hold_the_next_batch_d86() {
    let (carbon, release) = held_carbon();
    let sender = carbon.clone();
    run_batches(
        SMALL_BATCHES,
        move |context| {
            Box::pin(async move {
                let (config, tracker) = carbon_tracker(context, sender.clone());
                Ok((config, tracker, (sender, release)))
            })
        },
        |(carbon, release)| {
            Box::pin(async move {
                carbon.entered.notified().await;
                assert!(carbon.sent.lock().unwrap().is_empty());
                release.send_replace(true);
                Ok(())
            })
        },
        |_| Box::pin(async { Ok(()) }),
    );
    assert_eq!(carbon.sent.lock().unwrap().len(), 2);
}

#[test]
fn a_full_remote_queue_waits_for_one_stalled_send_then_cools_off_d86() {
    let (carbon, release) = held_carbon();
    let sender = carbon.clone();
    // Each of the first two batches could fill the queue. The third proves the second releases
    // during cool-off, while Carbon has yet to return a response.
    run_batches(
        Batches {
            results: 2005,
            size: 1002,
            starts_while_held: 1,
            send_limit: Some(Duration::from_secs(1)),
        },
        move |context| {
            Box::pin(async move {
                let (config, tracker) = carbon_tracker(context, sender.clone());
                Ok((config, tracker, (sender, release)))
            })
        },
        |(carbon, _)| {
            Box::pin(async move {
                carbon.ended.notified().await;
                Ok(())
            })
        },
        |_| Box::pin(async { Ok(()) }),
    );
    assert_eq!(carbon.attempts.load(Ordering::Relaxed), 1);
    assert!(carbon.sent.lock().unwrap().is_empty());
}

#[test]
fn a_full_influx_writer_cannot_hold_the_next_batch_d86() {
    run_batches(
        SMALL_BATCHES,
        |context| {
            Box::pin(async move {
                const DATABASES: &str =
                    r#"{"results":[{"series":[{"name":"databases","columns":["name"],"values":[["db"]]}]}]}"#;
                const PAD: &str = "padding value=1i";
                let (release, released) = watch::channel(false);
                let peer = Peer::with_async_replies(move |request| {
                    let writing = request.path.starts_with("/write?");
                    assert!(writing || request.path.starts_with("/query"));
                    let mut released = released.clone();
                    async move {
                        if writing {
                            let _ = released.wait_for(|ready| *ready).await;
                            Reply {
                                status: 204,
                                body: String::new(),
                                headers: vec![],
                            }
                        } else {
                            Reply::ok(DATABASES)
                        }
                    }
                })
                .await?;
                let config = v1(&peer.url);
                let client = InfluxClient::connect(&config, false, context.clone(), context.vendor.clone()).await?;
                for _ in 0..BATCH_ACTIONS {
                    client.write(PAD.to_owned()).await?;
                }
                // Hold the worker in its first flush before filling every slot in its point channel.
                peer.wait_requests(2).await;
                for _ in 0..BATCH_ACTIONS {
                    client.write(PAD.to_owned()).await?;
                }
                let tracker: Arc<dyn TrackerInternal> = Arc::new(InfluxTracker {
                    client,
                    read_only: false,
                });
                Ok((AnalyticsConfiguration::InfluxDb(config), tracker, (peer, release)))
            })
        },
        |(peer, release)| {
            Box::pin(async move {
                assert_eq!(peer.requests.lock().unwrap().len(), 2);
                release.send_replace(true);
                Ok(())
            })
        },
        |(peer, _)| {
            Box::pin(async move {
                let requests = peer.shutdown().await?;
                let points = requests
                    .iter()
                    .filter(|request| request.path.starts_with("/write?"))
                    .flat_map(|request| request.body.lines())
                    .filter(|line| line.starts_with("tests,"))
                    .collect::<Vec<_>>();
                assert_eq!(points.len(), 2, "{requests:?}");
                Ok(())
            })
        },
    );
}
