//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt
//! D80 / U201: a slow video pull does not hold back the batch's next test.
mod listener_support;
mod simulator_support;
mod support;
use marathon_apple::cmd::{FileBridge, failure};
use marathon_config::{schema::ScreenRecordingPolicy, vendor::IosRecorderType};
use marathon_core::{
    error::VendorError,
    executor::shared::{SharedDeferred, Signal},
    model::{AttachmentType, TestResult as Outcome},
    runtime::VendorFuture,
    vendor::{Device, VendorContext},
};
use marathon_error::FailureKind;
use marathon_ios::apple_simulator_device::AppleSimulatorDevice;
use marathon_test_support::apple::fake_simulator_worker::ok;
use simulator_support::*;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const PIDFILE: &str = "/tmp/marathon/a/a.pid";
/// How long the second test runs on the worker, on the test's clock.
const SECOND_TEST_MILLIS: u64 = 7_700;

/// The first video pull waits for `release`; the others are immediate.
#[derive(Default)]
struct SlowFirstPull {
    started: AtomicUsize,
    entered: Arc<SharedDeferred<()>>,
    release: Arc<SharedDeferred<()>>,
    pulled: Mutex<Vec<String>>,
}
impl FileBridge for SlowFirstPull {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            if self.started.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.complete(Ok(()));
                self.release.wait().await?;
            }
            tokio::fs::write(dst, b"video")
                .await
                .map_err(|e| VendorError::Other(failure(FailureKind::Io, e.to_string())))?;
            self.pulled.lock().unwrap().push(src.into());
            Ok(true)
        })
    }
}

/// The worker's recorder: `nohup` starts it and writes the pidfile, a stop removes it. Records when each test's
/// recording started and when each stop removed the pidfile. With `slow_stop`, the first stop's INT waits for it.
#[derive(Default)]
struct Worker {
    pid: Mutex<Option<String>>,
    recordings: Mutex<Vec<(String, i64)>>,
    stops: Mutex<Vec<Arc<SharedDeferred<()>>>>,
    stopped_at: Mutex<Vec<i64>>,
    slow_stop: Mutex<Option<Arc<SharedDeferred<()>>>>,
    stop_entered: Arc<SharedDeferred<()>>,
}

struct Setup {
    h: Harness,
    d: Arc<AppleSimulatorDevice>,
    script: Arc<BatchScript>,
    bridge: Arc<SlowFirstPull>,
    worker: Arc<Worker>,
}
async fn setup() -> TestResult<Setup> {
    let mut h = listener_support::harness(|v| {
        v.screen_record_configuration.preferable_recorder_type = Some(IosRecorderType::Video);
        v.screen_record_configuration.video_configuration.enabled = true;
    })?;
    Arc::make_mut(&mut h.services.configuration).screen_recording_policy = ScreenRecordingPolicy::OnAny;
    let worker = Arc::new(Worker {
        stops: Mutex::new(vec![SharedDeferred::new(), SharedDeferred::new()]),
        ..Worker::default()
    });
    let (model, boot, clock) = (worker.clone(), h.boot.clone(), h.clock.clone());
    *h.worker.before.lock().unwrap() = Arc::new(move |cmd| {
        let mut gate = None;
        let answer = if cmd == ["cat", PIDFILE] {
            Some(model.pid.lock().unwrap().clone().map_or_else(failing, |pid| ok([pid])))
        } else if let Some(nohup) = cmd.last().filter(|a| a.contains("nohup")) {
            let test = ["test1", "test2"]
                .into_iter()
                .find(|t| nohup.contains(&format!("-{t}-")));
            model
                .recordings
                .lock()
                .unwrap()
                .push((test.unwrap_or("?").into(), clock.now()));
            *model.pid.lock().unwrap() = Some("100".into());
            Some(ok(Vec::<String>::new()))
        } else if cmd.last().is_some_and(|a| a.starts_with("kill -s INT")) {
            gate = model.slow_stop.lock().unwrap().take();
            if gate.is_some() {
                model.stop_entered.complete(Ok(()));
            }
            Some(ok(Vec::<String>::new()))
        } else if cmd == ["rm", "-rf", PIDFILE] {
            *model.pid.lock().unwrap() = None;
            model.stopped_at.lock().unwrap().push(clock.now());
            let mut stops = model.stops.lock().unwrap();
            if !stops.is_empty() {
                stops.remove(0).complete(Ok(()));
            }
            Some(ok(Vec::<String>::new()))
        } else {
            boot.answer(&cmd)
        };
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await?;
            }
            Ok(answer)
        })
    });
    let bridge = Arc::new(SlowFirstPull::default());
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(
                marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                    marathon_config::devices::Transport::Local,
                    h.clock.clone(),
                ),
            ),
            bridge.clone(),
        )
        .await?;
    h.prepare(&d).await.map_err(|e| format!("{e:?}"))?;
    let script = Arc::new(BatchScript::default());
    script.attach(&h.worker);
    Ok(Setup {
        h,
        d,
        script,
        bridge,
        worker,
    })
}
fn lines() -> Vec<String> {
    failed(1).into_iter().chain(passed(2)).collect()
}
fn video(result: &Outcome) -> bool {
    result.attachments.iter().any(|a| a.kind == AttachmentType::Video)
}

/// Run 06 of the 0.14.0-rc.1 remote iOS check: the first test's video took 14.5 s to pull, and the listener handled
/// the second test's start only after that test had passed, so it got no recording and a JUnit time of 0.0. Here the
/// first pull is held until the second test has run for 7.7 s and passed.
#[tokio::test(start_paused = true)]
async fn a_slow_video_pull_does_not_hold_back_the_next_test_d80() -> TestResult {
    let Setup {
        h,
        d,
        script,
        bridge,
        worker,
    } = setup().await?;
    let passed_at = Arc::new(Mutex::new(None));
    let (entered, release, clock, at) = (
        bridge.entered.clone(),
        bridge.release.clone(),
        h.clock.clone(),
        passed_at.clone(),
    );
    script.plan.lock().unwrap().lines = lines();
    script.plan.lock().unwrap().pace = Some(Arc::new(move |n| {
        let (entered, release, clock, at) = (entered.clone(), release.clone(), clock.clone(), at.clone());
        Box::pin(async move {
            match n {
                // test2 runs while test1's video is being pulled.
                3 => {
                    let _ = entered.wait().await;
                    tokio::time::sleep(std::time::Duration::from_millis(SECOND_TEST_MILLIS)).await;
                    *at.lock().unwrap() = Some(clock.now());
                }
                // The pull ends only once test2 has passed.
                4 => {
                    release.complete(Ok(()));
                }
                _ => {}
            }
        })
    }));
    let (outcome, results) = run(&h, &d).await;
    outcome.map_err(|e| format!("{e:?}"))?;
    let results = results.ok_or("no results")?;
    // Both pulls finished before the batch was reported.
    assert_eq!(bridge.pulled.lock().unwrap().len(), 2);
    let [first] = results.failed.as_slice() else {
        panic!("{results:?}")
    };
    let [second] = results.finished.as_slice() else {
        panic!("{results:?}")
    };
    assert_eq!(
        (first.test.method.as_str(), second.test.method.as_str()),
        ("test1", "test2")
    );
    assert!(video(first), "{first:?}");
    assert!(video(second), "the next test keeps its video: {second:?}");
    let duration = second.end_time - second.start_time;
    assert!(
        duration >= SECOND_TEST_MILLIS as i64,
        "the next test keeps its time: {duration} ms"
    );
    // The second recording started with its test, not after it had passed.
    let passed_at = passed_at.lock().unwrap().ok_or("test2 never passed")?;
    let recordings = worker.recordings.lock().unwrap().clone();
    let started = recordings
        .iter()
        .find(|(test, _)| test == "test2")
        .map(|(_, at)| *at)
        .ok_or("test2 was not recorded")?;
    assert!(started < passed_at, "{recordings:?}, passed at {passed_at}");
    h.finish(&[d]).await;
    Ok(())
}

/// A cancelled batch does not start the video work its ended tests still had queued, and ends within its bound.
#[tokio::test(start_paused = true)]
async fn a_cancelled_batch_skips_queued_video_work_d80() -> TestResult {
    let Setup {
        h,
        d,
        script,
        bridge,
        worker,
    } = setup().await?;
    {
        let mut plan = script.plan.lock().unwrap();
        plan.lines = lines();
        plan.hang = true;
    }
    let signal = Signal::new();
    let op = run_with(
        &h,
        &d,
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    tokio::pin!(op);
    // test1's pull is in progress, and test2 has ended with its video queued behind it.
    let stopped = worker.stops.lock().unwrap()[1].clone();
    tokio::select! { r = &mut op => panic!("{r:?}"), _ = async { bridge.entered.wait().await?; stopped.wait().await } => {} }
    let before = h.worker.commands.lock().unwrap().len();
    let cancelled_at = h.clock.now();
    signal.cancel(marathon_core::executor::shared::requested("caller"));
    bridge.release.complete(Ok(()));
    let (outcome, _) = op.await;
    assert!(matches!(outcome, Err(VendorError::Cancelled(_))), "{outcome:?}");
    assert!(h.clock.now() - cancelled_at <= h.services.timeouts.cancelled_batch_millis);
    assert!(!d.lost());
    h.finish(std::slice::from_ref(&d)).await;
    assert_eq!(bridge.pulled.lock().unwrap().len(), 1);
    let after = h.worker.commands.lock().unwrap()[before..].to_vec();
    assert!(
        !after.iter().any(|c| c.iter().any(|a| a.contains("-test2-"))),
        "{after:?}"
    );
    Ok(())
}

/// The recorder can take seconds to stop on a remote worker (3.4 s in run 06, waiting for the recorder to exit). The
/// first test's stop is held until the second test has run for 7.7 s and passed: the second test keeps its time, and
/// its recording still starts only after the first one has stopped.
#[tokio::test(start_paused = true)]
async fn a_slow_recording_stop_does_not_hold_back_the_next_test_d80() -> TestResult {
    let Setup {
        h,
        d,
        script,
        bridge,
        worker,
    } = setup().await?;
    bridge.release.complete(Ok(()));
    let release = SharedDeferred::new();
    *worker.slow_stop.lock().unwrap() = Some(release.clone());
    let entered = worker.stop_entered.clone();
    script.plan.lock().unwrap().lines = lines();
    script.plan.lock().unwrap().pace = Some(Arc::new(move |n| {
        let (entered, release) = (entered.clone(), release.clone());
        Box::pin(async move {
            match n {
                3 => {
                    let _ = entered.wait().await;
                    tokio::time::sleep(std::time::Duration::from_millis(SECOND_TEST_MILLIS)).await;
                }
                4 => {
                    release.complete(Ok(()));
                }
                _ => {}
            }
        })
    }));
    let (outcome, results) = run(&h, &d).await;
    outcome.map_err(|e| format!("{e:?}"))?;
    let results = results.ok_or("no results")?;
    let [second] = results.finished.as_slice() else {
        panic!("{results:?}")
    };
    assert!(results.failed.iter().all(video) && video(second), "{results:?}");
    let duration = second.end_time - second.start_time;
    assert!(
        duration >= SECOND_TEST_MILLIS as i64,
        "the next test keeps its time: {duration} ms"
    );
    // One recording at a time: the second starts once the first has stopped, without the late-stop warning.
    let first_stopped = worker.stopped_at.lock().unwrap().first().copied().ok_or("no stop")?;
    let recordings = worker.recordings.lock().unwrap().clone();
    assert!(
        recordings
            .iter()
            .any(|(test, at)| test == "test2" && *at >= first_stopped),
        "{recordings:?}, first stop at {first_stopped}"
    );
    assert!(
        !h.logs
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.2.starts_with("Previous video recording didn't finish"))
    );
    h.finish(&[d]).await;
    Ok(())
}

/// Waits on the test clock for the debug listener to log that `test` has started.
async fn handled_start(h: &Harness, test: &str) {
    let suffix = format!("#{test}");
    while !h
        .logs
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.2.starts_with("testStarted") && r.2.ends_with(&suffix))
    {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

/// D80 lets test2's recording wait in its own task for test1's stop. The simulator is disposed of while it waits;
/// once that stop ends, the waiting start must not record on a disposed simulator.
#[tokio::test(start_paused = true)]
async fn a_recording_waiting_for_the_previous_stop_never_starts_after_disposal_d80() -> TestResult {
    let Setup {
        h,
        d,
        script,
        bridge,
        worker,
    } = setup().await?;
    bridge.release.complete(Ok(()));
    let release = SharedDeferred::new();
    *worker.slow_stop.lock().unwrap() = Some(release.clone());
    {
        let mut plan = script.plan.lock().unwrap();
        plan.lines = failed(1).into_iter().chain([passed(2)[0].clone()]).collect();
        plan.hang = true;
    }
    let batch = spawn_batch(&h, d.clone(), h.services.context.clone());
    worker.stop_entered.wait().await.map_err(|e| format!("{e:?}"))?;
    handled_start(&h, "test2").await;
    d.dispose().await.map_err(|e| format!("{e:?}"))?;
    let disposed_at = h.clock.now();
    release.complete(Ok(()));
    let _ = batch.await?;
    // Detached work, if any, runs out on the test clock: await_removal waits up to 10 s.
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    let recordings = worker.recordings.lock().unwrap().clone();
    assert!(
        recordings.iter().all(|(test, _)| test != "test2"),
        "{recordings:?}, disposed at {disposed_at}"
    );
    h.finish(&[d]).await;
    Ok(())
}

/// A test that runs twice in one batch records to the same remote file both times. Its first transfer moves, pulls and
/// removes that file, so the repeat's recording must start only after it, or the transfer takes or deletes the
/// repeat's recording. The first pull is held while the repeat starts; the repeat ends only once it is recording, so
/// its own stop never cancels a start that is still waiting.
#[tokio::test(start_paused = true)]
async fn a_repeated_test_records_again_only_after_its_first_video_is_collected_d80() -> TestResult {
    let Setup {
        h,
        d,
        script,
        bridge,
        worker,
    } = setup().await?;
    let (entered, release) = (bridge.entered.clone(), bridge.release.clone());
    script.plan.lock().unwrap().lines = passed(1).into_iter().chain(passed(1)).collect();
    script.plan.lock().unwrap().pace = Some(Arc::new(move |n| {
        let (entered, release, worker) = (entered.clone(), release.clone(), worker.clone());
        Box::pin(async move {
            match n {
                // The repeat starts while the first video is being pulled.
                2 => {
                    let _ = entered.wait().await;
                }
                3 => {
                    // On the paused clock this returns once nothing else can run: the repeat's start has been
                    // handled and its recording job has gone as far as it can while the pull is held.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    release.complete(Ok(()));
                    while worker.recordings.lock().unwrap().len() < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                }
                _ => {}
            }
        })
    }));
    let (outcome, _) = run(&h, &d).await;
    outcome.map_err(|e| format!("{e:?}"))?;
    assert_eq!(bridge.pulled.lock().unwrap().len(), 2);
    let commands = h.worker.commands.lock().unwrap().clone();
    let video = "/tmp/marathon/a/Tests-Suite-test1-";
    let starts: Vec<_> = commands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.last().is_some_and(|a| a.contains("nohup") && a.contains(video)))
        .map(|(n, _)| n)
        .collect();
    let removals: Vec<_> = commands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.first().is_some_and(|a| a == "rm") && c.last().is_some_and(|a| a.starts_with(video)))
        .map(|(n, _)| n)
        .collect();
    assert_eq!((starts.len(), removals.len()), (2, 2), "{commands:?}");
    assert!(
        removals[0] < starts[1],
        "the repeat recorded before the first video was collected: {commands:?}"
    );
    h.finish(&[d]).await;
    Ok(())
}
