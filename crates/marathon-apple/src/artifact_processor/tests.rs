//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/ArtifactProcessorTest.kt
//! and ArtifactProcessorFairnessTest.kt and ArtifactQueueSizingTest.kt.

use super::core::{Admission, AdmissionCore};
use super::*;
use crate::test_support as support;
use marathon_core::executor::shared::requested;
use marathon_error::{FailureKind, FatalKind};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
};

type Logs = Arc<Mutex<Vec<(Level, String, Option<Failure>)>>>;
struct Harness {
    p: Arc<ArtifactProcessor>,
    context: VendorContext,
    clock: Arc<support::Clock>,
    logs: Logs,
}
impl Harness {
    fn new(capacity: i32, concurrent: i32, share: i32) -> Self {
        let clock = Arc::new(support::Clock::default());
        let logs: Logs = Arc::default();
        let output = logs.clone();
        let logging = SshLogging::new(
            Arc::new(Mutex::new(marathon_logging::LogSettings::default())),
            Arc::new(move |_, level, message, error| output.lock().unwrap().push((level, message, error))),
        );
        let p = ArtifactProcessor::new(
            capacity,
            concurrent,
            share,
            Arc::new(support::Runtime),
            clock.clone(),
            logging,
        )
        .unwrap();
        let context = VendorContext {
            cancellation: Signal::new(),
            clock: clock.clone(),
            hooks: clock.clone(),
        };
        Self {
            p,
            context,
            clock,
            logs,
        }
    }
    async fn submit(&self, owner: &str, batch: &str, worker: &str, action: ArtifactAction) -> VendorResult<bool> {
        self.p
            .submit(
                owner.into(),
                batch.into(),
                worker.into(),
                None,
                action,
                self.context.clone(),
            )
            .await
    }
    async fn close(&self) -> Vec<String> {
        self.p.close_and_join(300_000, 60_000, &self.context).await.unwrap()
    }
    async fn barrier(&self, owner: &str) {
        self.p.await_owner(owner, &self.context).await.unwrap();
    }
    fn messages(&self) -> Vec<String> {
        self.logs.lock().unwrap().iter().map(|(_, m, _)| m.clone()).collect()
    }
}
fn empty() -> ArtifactAction {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}
fn gated(gate: Arc<SharedDeferred<()>>, aware: bool) -> ArtifactAction {
    Arc::new(move |context| {
        let gate = gate.clone();
        Box::pin(async move {
            if aware {
                gate.wait_in(context.cancellation).await?;
            } else {
                gate.wait().await?;
            }
            Ok(())
        })
    })
}
fn delay(millis: i64) -> ArtifactAction {
    Arc::new(move |context| Box::pin(async move { context.clock.delay(millis, context.cancellation).await }))
}
async fn once<T>(mut f: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    poll_fn(|cx| Poll::Ready(f.as_mut().poll(cx))).await
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
fn strings(values: &[String]) -> Vec<String> {
    values.iter().map(String::clone).collect()
}

struct Model {
    q: AdmissionCore<usize>,
    next: u64,
    admitted: Vec<(usize, Admission)>,
    now: i64,
}
impl Model {
    fn new(capacity: i32, concurrent: i32, share: i32) -> Self {
        Self {
            q: AdmissionCore::new(capacity, concurrent, share),
            next: 0,
            admitted: vec![],
            now: 0,
        }
    }
    fn submit(&mut self, owner: &str, worker: &str) -> usize {
        let id = self.next;
        self.next += 1;
        let w = Waiter {
            item: Item {
                id,
                owner: owner.into(),
                worker: worker.into(),
                payload: id as usize,
                enqueued_at: 0,
            },
            arrived_at: self.now,
            blocked_by_run: None,
            decided: SharedDeferred::new(),
        };
        if self.q.admissible(&w.item.worker) {
            let info = self.q.admit(w, self.now).2;
            self.admitted.push((id as usize, info));
        } else {
            self.q.wait(w);
        }
        id as usize
    }
    fn complete(&mut self, owner: &str, worker: &str) -> Vec<Admission> {
        self.q
            .complete(owner, worker, self.now)
            .admitted
            .into_iter()
            .map(|(_, d, a)| {
                d.complete(Ok(Some(a)));
                a
            })
            .collect()
    }
    fn take(&mut self, worker: &str) -> usize {
        self.q.take(worker).unwrap().payload
    }
}
#[test]
fn one_blocked_worker_keeps_only_its_share_while_healthy_workers_are_admitted_and_transfer() {
    let mut m = Model::new(8, 4, 2);
    for i in 0..8 {
        m.submit(&format!("slow-{i}"), "slow");
    }
    assert_eq!(m.admitted.len(), 2);
    assert_eq!(m.take("slow"), 0);
    for i in 0..3 {
        m.submit(&format!("healthy-{i}"), &format!("healthy-{i}"));
    }
    assert_eq!(m.admitted.len(), 5);
    for i in 0..3 {
        m.take(&format!("healthy-{i}"));
        m.complete(&format!("healthy-{i}"), &format!("healthy-{i}"));
    }
    assert_eq!(m.q.outstanding, 2);
    for i in 0..8 {
        if i > 0 {
            assert_eq!(m.take("slow"), i);
        }
        m.complete(&format!("slow-{i}"), "slow");
    }
    assert_eq!(m.q.outstanding, 0);
}
#[test]
fn freed_capacity_goes_to_the_longest_waiting_submitter_whose_worker_is_below_its_share() {
    let mut m = Model::new(4, 2, 2);
    for i in 0..4 {
        m.submit(&format!("slow-{i}"), "slow");
    }
    for i in 0..2 {
        m.submit(&format!("other-{i}"), "other");
    }
    for i in 0..2 {
        m.submit(&format!("late-{i}"), "late");
    }
    assert_eq!(m.q.outstanding, 4);
    assert_eq!(m.take("other"), 4);
    let first = m.complete("other-0", "other");
    assert_eq!(first.len(), 1);
    assert!(first[0].starts_worker);
    assert_eq!(m.take("late"), 6);
    assert_eq!(m.q.waiting.len(), 3);
    assert_eq!(m.take("other"), 5);
    m.complete("other-1", "other");
    assert_eq!(m.take("late"), 7);
    assert_eq!(m.q.waiting.len(), 2);
}
#[test]
fn fewer_stalled_workers_than_transfer_slots_cannot_hold_back_the_rest_of_the_fleet() {
    let mut m = Model::new(8, 4, 2);
    for w in 0..3 {
        for s in 0..3 {
            m.submit(&format!("{w}-{s}"), &format!("stalled-{w}"));
        }
    }
    assert_eq!(m.q.outstanding, 6);
    for i in 0..5 {
        m.submit(&format!("healthy-{i}"), "healthy");
        m.take("healthy");
        m.complete(&format!("healthy-{i}"), "healthy");
    }
    for i in 0..3 {
        m.submit(&format!("fourth-{i}"), "fourth");
    }
    assert_eq!(m.q.outstanding, 8);
    m.submit("healthy-blocked", "healthy");
    assert_eq!(m.q.waiting.len(), 5);
}
#[test]
fn simulators_of_one_worker_share_its_capacity_in_arrival_order_and_keep_their_own_barriers() {
    let mut m = Model::new(8, 4, 2);
    for i in 0..4 {
        m.submit(&format!("sim-{i}"), "mac");
    }
    let barrier = m.q.owner_done("sim-1").unwrap();
    assert!(barrier.completed().is_none());
    assert_eq!(m.take("mac"), 0);
    m.complete("sim-0", "mac");
    assert_eq!(m.q.waiting.len(), 1);
    assert_eq!(m.take("mac"), 1);
    let effects = m.q.complete("sim-1", "mac", 0);
    for done in effects.barriers {
        done.complete(Ok(()));
    }
    assert!(barrier.completed().is_some());
    assert!(m.q.waiting.is_empty());
    assert_eq!(m.take("mac"), 2);
    assert_eq!(m.take("mac"), 3);
}
#[tokio::test(start_paused = true)]
async fn cancelled_waiting_is_reported_once_and_cancellation_racing_with_admission_still_delivers() {
    let h = Harness::new(2, 1, 2);
    let gate = SharedDeferred::new();
    h.submit("mac", "0", "mac", gated(gate.clone(), true)).await.unwrap();
    h.submit("mac", "1", "mac", empty()).await.unwrap();
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let cancelled = h.p.submit(
        "cancelled".into(),
        "cancelled".into(),
        "mac".into(),
        None,
        empty(),
        context,
    );
    tokio::pin!(cancelled);
    assert!(once(cancelled.as_mut()).await.is_pending());
    signal.cancel(requested("device cancelled"));
    assert!(matches!(cancelled.await, Err(VendorError::Cancelled(_))));
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let raced =
        h.p.submit("raced".into(), "raced".into(), "mac".into(), None, empty(), context);
    tokio::pin!(raced);
    assert!(once(raced.as_mut()).await.is_pending());
    gate.complete(Ok(()));
    settle().await;
    signal.cancel(requested("race"));
    // Prompt waiter cancellation leaves the admitted artifact owned by its worker.
    assert!(matches!(raced.await, Err(VendorError::Cancelled(_))));
    h.barrier("raced").await;
    let retained = h.close().await;
    assert_eq!(
        strings(&retained),
        vec!["cancelled, batch cancelled: admission was cancelled"]
    );
}
#[tokio::test(start_paused = true)]
async fn capacity_includes_active_work_and_cancellation_releases_blocked_producers() {
    let h = Harness::new(2, 1, 2);
    let gate = SharedDeferred::new();
    h.submit("device-a", "first", "device-a", gated(gate.clone(), true))
        .await
        .unwrap();
    h.submit("device-b", "second", "device-b", empty()).await.unwrap();
    let signal = Signal::new();
    let mut c = h.context.clone();
    c.cancellation = signal.clone();
    let cancelled = h.p.submit(
        "device-c".into(),
        "cancelled".into(),
        "device-c".into(),
        None,
        empty(),
        c,
    );
    tokio::pin!(cancelled);
    assert!(once(cancelled.as_mut()).await.is_pending());
    signal.cancel(requested("cancel"));
    assert!(cancelled.await.is_err());
    let third = h.submit("device-a", "third", "device-a", empty());
    tokio::pin!(third);
    assert!(once(third.as_mut()).await.is_pending());
    let close = h.close();
    tokio::pin!(close);
    assert!(once(close.as_mut()).await.is_pending());
    assert!(!third.await.unwrap());
    gate.complete(Ok(()));
    assert_eq!(
        strings(&close.await),
        vec![
            "device-c, batch cancelled: admission was cancelled",
            "device-a, batch third: artifact queue is closed"
        ]
    );
}
#[tokio::test(start_paused = true)]
async fn saturated_queue_applies_backpressure_and_runs_one_transfer_across_workers() {
    let h = Harness::new(2, 1, 2);
    let gate = SharedDeferred::new();
    let order: Arc<Mutex<Vec<usize>>> = Arc::default();
    let action = |i, gate: Option<Arc<SharedDeferred<()>>>| -> ArtifactAction {
        let order = order.clone();
        Arc::new(move |c| {
            let order = order.clone();
            let gate = gate.clone();
            Box::pin(async move {
                order.lock().unwrap().push(i);
                if let Some(g) = gate {
                    g.wait_in(c.cancellation).await?;
                }
                Ok(())
            })
        })
    };
    h.submit("a", "one", "worker-1", action(1, Some(gate.clone())))
        .await
        .unwrap();
    h.submit("b", "two", "worker-2", action(2, None)).await.unwrap();
    let third = h.submit("c", "three", "worker-1", action(3, None));
    tokio::pin!(third);
    assert!(once(third.as_mut()).await.is_pending());
    settle().await;
    assert_eq!(*order.lock().unwrap(), vec![1]);
    gate.complete(Ok(()));
    assert!(third.await.unwrap());
    h.close().await;
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
}
#[tokio::test(start_paused = true)]
async fn device_barrier_protects_all_its_bundles_until_cleanup_completes_and_failure_does_not_stop_queue() {
    let h = Harness::new(3, 1, 3);
    let gate = SharedDeferred::new();
    h.submit("device", "one", "device", gated(gate.clone(), true))
        .await
        .unwrap();
    h.submit(
        "device",
        "two",
        "device",
        Arc::new(|_| {
            Box::pin(async {
                Err(VendorError::Other(failure(
                    FailureKind::Fatal(FatalKind::Assertion),
                    "transfer failed",
                )))
            })
        }),
    )
    .await
    .unwrap();
    h.submit("other", "three", "other", empty()).await.unwrap();
    let prepare = h.barrier("device");
    tokio::pin!(prepare);
    assert!(once(prepare.as_mut()).await.is_pending());
    let reports = h.close();
    tokio::pin!(reports);
    assert!(once(reports.as_mut()).await.is_pending());
    gate.complete(Ok(()));
    prepare.await;
    let retained = reports.await;
    assert_eq!(retained.len(), 1);
    assert!(retained[0].to_owned().contains("transfer failed"));
    h.barrier("device").await;
    h.barrier("unknown").await;
}
#[tokio::test(start_paused = true)]
async fn blocked_worker_does_not_block_another_host_and_per_worker_and_global_concurrency_stay_bounded() {
    let h = Harness::new(5, 2, 2);
    let gate = SharedDeferred::new();
    let started: Arc<Mutex<Vec<usize>>> = Arc::default();
    for (i, w) in [(0, "w1"), (1, "w1"), (2, "w2"), (3, "w3")] {
        let started = started.clone();
        let gate = gate.clone();
        h.submit(
            "device",
            &i.to_string(),
            w,
            Arc::new(move |c| {
                let started = started.clone();
                let gate = gate.clone();
                Box::pin(async move {
                    started.lock().unwrap().push(i);
                    if i == 0 || i == 2 {
                        gate.wait_in(c.cancellation).await?;
                    }
                    Ok(())
                })
            }),
        )
        .await
        .unwrap();
    }
    settle().await;
    assert_eq!(*started.lock().unwrap(), vec![0, 2]);
    gate.complete(Ok(()));
    h.close().await;
    assert_eq!(started.lock().unwrap().len(), 4);
}
#[tokio::test(start_paused = true)]
async fn timed_out_drain_cancels_admitted_work_and_later_admission_does_not_throw() {
    let h = Harness::new(2, 4, 1);
    h.submit("device", "batch", "device", gated(SharedDeferred::new(), true))
        .await
        .unwrap();
    settle().await;
    let retained = h.p.close_and_join(10, 500, &h.context).await.unwrap();
    assert_eq!(retained.len(), 1);
    assert!(retained[0].to_owned().contains("job cancelled: Job was cancelled"));
    assert_eq!(h.clock.now(), 10);
    h.barrier("device").await;
    assert!(!h.submit("device", "late", "device", empty()).await.unwrap());
}
#[tokio::test(start_paused = true)]
async fn every_undelivered_bundle_is_returned_and_logged_as_an_error_at_finalization() {
    let h = Harness::new(4, 4, 1);
    let p = h.p.clone();
    h.submit(
        "a@worker-1",
        "reported",
        "w1",
        Arc::new(move |_| {
            let p = p.clone();
            Box::pin(async move {
                p.report_undelivered(
                    "a@worker-1",
                    Some("/remote/a.xcresult"),
                    "a@worker-1, batch reported: pull failed".into(),
                );
                Ok(())
            })
        }),
    )
    .await
    .unwrap();
    h.submit(
        "b@worker-2",
        "thrown",
        "w2",
        Arc::new(|_| {
            Box::pin(async {
                Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    "transport closed",
                )))
            })
        }),
    )
    .await
    .unwrap();
    h.submit("c@worker-3", "delivered", "w3", empty()).await.unwrap();
    h.submit("d@worker-4", "stuck", "w4", gated(SharedDeferred::new(), true))
        .await
        .unwrap();
    settle().await;
    let retained = h.p.close_and_join(10, 500, &h.context).await.unwrap();
    assert_eq!(retained.len(), 3);
    assert_eq!(retained[0], String::from("a@worker-1, batch reported: pull failed"));
    let logs = h.logs.lock().unwrap();
    let summary = logs
        .iter()
        .find(|(l, m, _)| *l == Level::ERROR && m.starts_with("3 artifact bundle"))
        .unwrap();
    for d in retained {
        assert!(summary.1.contains(&d.to_owned()));
    }
}
#[tokio::test(start_paused = true)]
async fn closing_rejects_waiting_submitters_at_once_drains_admitted_bundles_and_rejects_later_ones() {
    let h = Harness::new(4, 2, 2);
    let gate = SharedDeferred::new();
    for (i, w) in ["slow", "slow", "busy", "busy"].into_iter().enumerate() {
        h.submit(w, &i.to_string(), w, gated(gate.clone(), true)).await.unwrap();
    }
    let a = h.submit("slow-late", "slow-late", "slow", empty());
    tokio::pin!(a);
    assert!(once(a.as_mut()).await.is_pending());
    let b = h.submit("idle-late", "idle-late", "idle", empty());
    tokio::pin!(b);
    assert!(once(b.as_mut()).await.is_pending());
    let close = h.close();
    tokio::pin!(close);
    assert!(once(close.as_mut()).await.is_pending());
    assert!(!a.await.unwrap());
    assert!(!b.await.unwrap());
    assert!(!h.submit("device", "after-close", "idle", empty()).await.unwrap());
    gate.complete(Ok(()));
    assert_eq!(
        strings(&close.await),
        vec![
            "device, batch after-close: artifact queue is closed",
            "slow-late, batch slow-late: artifact queue is closed",
            "idle-late, batch idle-late: artifact queue is closed"
        ]
    );
}
#[tokio::test(start_paused = true)]
async fn closing_reports_waiting_submitters_before_it_returns_and_a_later_cancellation_does_not_report_them_again() {
    let h = Harness::new(2, 1, 2);
    let gate = SharedDeferred::new();
    for i in 0..2 {
        h.submit("mac", &i.to_string(), "mac", gated(gate.clone(), true))
            .await
            .unwrap();
    }
    let a = h.submit("waiting-0", "waiting-0", "mac", empty());
    tokio::pin!(a);
    assert!(once(a.as_mut()).await.is_pending());
    let signal = Signal::new();
    let mut c = h.context.clone();
    c.cancellation = signal.clone();
    let b =
        h.p.submit("waiting-1".into(), "waiting-1".into(), "mac".into(), None, empty(), c);
    tokio::pin!(b);
    assert!(once(b.as_mut()).await.is_pending());
    let close = h.close();
    tokio::pin!(close);
    assert!(once(close.as_mut()).await.is_pending());
    gate.complete(Ok(()));
    let retained = close.await;
    assert_eq!(retained.len(), 2);
    signal.cancel(requested("late cancellation"));
    assert!(!a.await.unwrap());
    assert!(matches!(b.await, Err(VendorError::Cancelled(_))));
    assert_eq!(h.close().await, retained);
}
#[tokio::test(start_paused = true)]
async fn failing_report_callbacks_do_not_stop_shutdown_and_waiting_submitters_are_reported_after_the_drain() {
    let h = Harness::new(2, 1, 2);
    let gate = SharedDeferred::new();
    for i in 0..2 {
        h.submit("mac", &i.to_string(), "mac", gated(gate.clone(), true))
            .await
            .unwrap();
    }
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let e = events.clone();
    let a = h.p.submit(
        "failing".into(),
        "failing".into(),
        "mac".into(),
        Some(Arc::new(move |r| {
            e.lock().unwrap().push(r.to_owned());
            Err(failure(FailureKind::IllegalState, "report failed"))
        })),
        empty(),
        h.context.clone(),
    );
    tokio::pin!(a);
    assert!(once(a.as_mut()).await.is_pending());
    let b = h.submit("reported", "reported", "mac", empty());
    tokio::pin!(b);
    assert!(once(b.as_mut()).await.is_pending());
    h.p.protect(
        "sim".into(),
        "/remote/failing".into(),
        Arc::new(|_| Err(failure(FailureKind::IllegalState, "sweep failed"))),
    )
    .unwrap();
    let close = h.close();
    tokio::pin!(close);
    assert!(once(close.as_mut()).await.is_pending());
    assert!(!a.await.unwrap());
    assert!(!b.await.unwrap());
    assert!(events.lock().unwrap().is_empty());
    gate.complete(Ok(()));
    assert_eq!(close.await.len(), 1);
    assert_eq!(*events.lock().unwrap(), vec!["artifact queue is closed"]);
    assert_eq!(
        h.messages()
            .iter()
            .filter(|m| m.starts_with("Reporting an undelivered artifact failed:"))
            .count(),
        2
    );
}
#[tokio::test(start_paused = true)]
async fn an_inline_dispatcher_processes_admitted_work_and_admits_waiting_submitters() {
    let h = Harness::new(3, 1, 2);
    let gate = SharedDeferred::new();
    h.submit("a", "1", "mac", gated(gate.clone(), true)).await.unwrap();
    let p = h.p.clone();
    let c = h.context.clone();
    h.submit(
        "a",
        "2",
        "mac",
        Arc::new(move |_| {
            let p = p.clone();
            let c = c.clone();
            Box::pin(async move {
                p.report_undelivered("inner", None, "reentrant".into());
                p.await_owner("unknown", &c).await?;
                p.submit("inner".into(), "inner".into(), "other".into(), None, empty(), c)
                    .await?;
                Ok(())
            })
        }),
    )
    .await
    .unwrap();
    let third = h.submit("b", "3", "mac", empty());
    tokio::pin!(third);
    assert!(once(third.as_mut()).await.is_pending());
    gate.complete(Ok(()));
    assert!(third.await.unwrap());
    h.barrier("a").await;
    h.barrier("b").await;
    assert_eq!(strings(&h.close().await), vec!["reentrant"]);
}
#[tokio::test(start_paused = true)]
async fn work_cancelled_before_its_worker_starts_is_reported_once_and_releases_the_device_barrier() {
    let h = Harness::new(2, 4, 1);
    h.submit(
        "device",
        "queued",
        "device",
        Arc::new(|_| Box::pin(async { panic!("must not run") })),
    )
    .await
    .unwrap();
    let retained = h.p.close_and_join(0, 500, &h.context).await.unwrap();
    assert_eq!(retained.len(), 1);
    h.barrier("device").await;
    assert!(!h.submit("device", "late", "device", empty()).await.unwrap());
    assert_eq!(h.close().await.len(), 2);
}
#[tokio::test(start_paused = true)]
async fn shutdown_waits_a_bounded_time_for_work_that_ignores_cancellation_and_reports_it_once() {
    let h = Harness::new(4, 3, 2);
    let gate = SharedDeferred::new();
    h.submit("stuck@mac", "stuck", "mac", gated(gate.clone(), false))
        .await
        .unwrap();
    h.submit("queued@mac", "queued", "mac", empty()).await.unwrap();
    h.submit("aware@mini", "aware", "mini", gated(SharedDeferred::new(), true))
        .await
        .unwrap();
    h.p.submit(
        "failing@studio".into(),
        "failing".into(),
        "studio".into(),
        Some(Arc::new(|_| Err(failure(FailureKind::IllegalState, "report failed")))),
        gated(gate.clone(), false),
        h.context.clone(),
    )
    .await
    .unwrap();
    settle().await;
    let retained = h.p.close_and_join(1_000, 500, &h.context).await.unwrap();
    assert_eq!(h.clock.now(), 1_500);
    assert_eq!(retained.len(), 3);
    assert_eq!(
        retained[1],
        String::from("stuck@mac, batch stuck: still running 500 ms after shutdown cancelled it")
    );
    assert_eq!(
        retained[2],
        String::from("queued@mac, batch queued: not started before shutdown")
    );
    for owner in ["stuck@mac", "queued@mac", "aware@mini", "failing@studio"] {
        h.barrier(owner).await;
    }
    gate.complete(Err(VendorError::Other(failure(FailureKind::Io, "transport closed"))));
    settle().await;
    assert_eq!(h.close().await, retained);
    assert_eq!(
        h.messages()
            .iter()
            .filter(|m| m.contains("returned after shutdown reported it"))
            .count(),
        2
    );
}
#[tokio::test(start_paused = true)]
async fn protected_sources_stay_kept_through_finalization_and_are_reported_once() {
    let h = Harness::new(2, 4, 1);
    let callback = |source: &str| -> Undelivered {
        let p = Arc::downgrade(&h.p);
        let source = String::from(source);
        Arc::new(move |reason| {
            let p = p.upgrade().unwrap();
            p.release("device", &source);
            p.report_undelivered(
                "device",
                Some(&source),
                format!("{}: {}", source.to_owned(), reason.to_owned()).as_str().into(),
            );
            Ok(())
        })
    };
    h.p.protect(
        "device".into(),
        "/remote/delivered".into(),
        callback("/remote/delivered"),
    )
    .unwrap();
    h.p.release("device", "/remote/delivered");
    h.p.protect("device".into(), "/remote/skipped".into(), callback("/remote/skipped"))
        .unwrap();
    assert_eq!(
        strings(&h.close().await),
        vec!["/remote/skipped: never handed to the artifact queue"]
    );
    assert!(!h.p.report_undelivered("device", Some("/remote/skipped"), "duplicate".into()));
    h.p.release("device", "/remote/skipped");
    h.p.protect("device".into(), "/remote/late".into(), callback("/remote/late"))
        .unwrap();
    assert_eq!(
        strings(&h.p.kept_sources("device")),
        vec!["/remote/skipped", "/remote/late"]
    );
    assert_eq!(h.close().await.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_blocked_in_a_transfer_does_not_hold_finalization_and_its_late_return_is_not_reported_again() {
    let h = Harness::new(2, 4, 1);
    let entered = SharedDeferred::new();
    let latch = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let runtime = h.p.runtime.clone();
    let e = entered.clone();
    let l = latch.clone();
    h.submit(
        "device",
        "blocked",
        "device",
        Arc::new(move |_| {
            let runtime = runtime.clone();
            let e = e.clone();
            let l = l.clone();
            Box::pin(async move {
                runtime
                    .spawn_blocking(Box::new(move || {
                        e.complete(Ok(()));
                        let (lock, cv) = &*l;
                        let (_guard, timeout) = cv
                            .wait_timeout_while(lock.lock().unwrap(), std::time::Duration::from_secs(60), |v| !*v)
                            .unwrap();
                        assert!(!timeout.timed_out(), "blocking test watchdog");
                        Ok(())
                    }))
                    .join()
                    .await
            })
        }),
    )
    .await
    .unwrap();
    entered.wait().await.unwrap();
    let start = std::time::Instant::now();
    let retained = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        h.p.close_and_join(200, 200, &h.context),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(start.elapsed() >= std::time::Duration::from_millis(400));
    assert_eq!(
        strings(&retained),
        vec!["device, batch blocked: still running 200 ms after shutdown cancelled it"]
    );
    *latch.0.lock().unwrap() = true;
    latch.1.notify_all();
    assert_eq!(h.close().await, retained);
    assert_eq!(
        h.messages()
            .iter()
            .filter(|m| m.contains("returned after shutdown reported it"))
            .count(),
        1
    );
}
struct BlockingWorkers;
impl VendorRuntime for BlockingWorkers {
    fn spawn_cancellable(
        &self,
        body: marathon_core::runtime::VendorTaskBody,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        let handle = tokio::runtime::Handle::current();
        self.spawn_blocking(Box::new(move || handle.block_on(body(Signal::new()))))
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn marathon_core::runtime::VendorTask> {
        self.spawn_cancellable(Box::new(move |_| body))
    }
    fn spawn_blocking(
        &self,
        body: Box<dyn FnOnce() -> VendorResult<()> + Send>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        support::Runtime.spawn_blocking(body)
    }
}
struct ObservedClock {
    inner: Arc<support::Clock>,
    calls: std::sync::atomic::AtomicUsize,
    second_wait: Arc<SharedDeferred<()>>,
}
impl marathon_core::runtime::VendorClock for ObservedClock {
    fn delay(&self, m: i64, s: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        self.inner.delay(m, s)
    }
    fn with_timeout<'a>(
        &'a self,
        s: Arc<dyn CancellationSignal>,
        m: i64,
        b: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.inner.with_timeout(s, m, b)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        s: Arc<dyn CancellationSignal>,
        m: i64,
        b: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
            self.second_wait.complete(Ok(()));
        }
        self.inner.with_timeout_or_null(s, m, b)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn work_its_worker_finishes_reporting_after_the_cancellation_deadline_is_still_returned() {
    let mut h = Harness::new(2, 4, 1);
    h.p = ArtifactProcessor::new(2, 4, 1, Arc::new(BlockingWorkers), h.clock.clone(), h.p.logging.clone()).unwrap();
    let entered = SharedDeferred::new();
    let started = SharedDeferred::new();
    let second = SharedDeferred::new();
    h.context.clock = Arc::new(ObservedClock {
        inner: h.clock.clone(),
        calls: 0.into(),
        second_wait: second.clone(),
    });
    let latch = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let l = latch.clone();
    let e = entered.clone();
    let p = Arc::downgrade(&h.p);
    let cb: Undelivered = Arc::new(move |reason| {
        e.complete(Ok(()));
        let (lock, cv) = &*l;
        let (_guard, timeout) = cv
            .wait_timeout_while(lock.lock().unwrap(), std::time::Duration::from_secs(60), |v| !*v)
            .unwrap();
        assert!(!timeout.timed_out());
        p.upgrade().unwrap().report_undelivered(
            "device",
            None,
            format!("device, batch late: {}", reason.to_owned()).as_str().into(),
        );
        Ok(())
    });
    let s = started.clone();
    h.p.submit(
        "device".into(),
        "late".into(),
        "device".into(),
        Some(cb),
        Arc::new(move |c| {
            s.complete(Ok(()));
            Box::pin(async move { Err(VendorError::Cancelled(c.cancellation.cancelled().await)) })
        }),
        h.context.clone(),
    )
    .await
    .unwrap();
    started.wait().await.unwrap();
    let p = h.p.clone();
    let context = h.context.clone();
    let close = tokio::spawn(async move { p.close_and_join(0, 200, &context).await });
    entered.wait().await.unwrap();
    second.wait().await.unwrap();
    assert!(!close.is_finished());
    *latch.0.lock().unwrap() = true;
    latch.1.notify_all();
    let retained = tokio::time::timeout(std::time::Duration::from_secs(60), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert!(
        retained[0]
            .to_owned()
            .contains("device, batch late: processing did not complete")
    );
    assert_eq!(h.close().await, retained);
}

#[derive(Clone)]
struct SimWork {
    device: usize,
    batch: usize,
}
enum Event {
    Submit(usize, usize),
    Start(String),
    Finish(String, Item<SimWork>),
}
struct Simulation {
    q: AdmissionCore<SimWork>,
    events: Vec<(i64, u64, Event)>,
    sequence: u64,
    now: i64,
    next_id: u64,
    permits: std::collections::VecDeque<(String, Item<SimWork>)>,
    active: usize,
    peak_active: usize,
    peak_depth: i32,
    peak_outstanding: i32,
    stalled_peak: i32,
    waits: Vec<i64>,
    healthy_max: i64,
    busy: i64,
    batches: usize,
    batch_time: i64,
    pull_time: i64,
    stalled: usize,
    release_at: i64,
    characterization: bool,
    timer_ties: bool,
}
impl Simulation {
    fn event(&mut self, at: i64, event: Event) {
        self.events.push((at, self.sequence, event));
        self.sequence += 1;
    }
    fn admitted(&mut self, worker: String, work: SimWork, info: Admission) {
        self.waits.push(info.wait_millis);
        self.peak_depth = self.peak_depth.max(info.worker_depth);
        self.peak_outstanding = self.peak_outstanding.max(info.depth);
        if work.device / 4 < self.stalled {
            self.stalled_peak = self.stalled_peak.max(info.worker_depth);
        } else {
            self.healthy_max = self.healthy_max.max(info.wait_millis);
        }
        if info.starts_worker {
            self.event(self.now, Event::Start(worker));
        }
        if work.batch + 1 < self.batches {
            self.event(self.now + self.batch_time, Event::Submit(work.device, work.batch + 1));
        }
    }
    fn transfer(&mut self, worker: String, item: Item<SimWork>) {
        if self.active >= self.q.concurrency as usize {
            self.permits.push_back((worker, item));
            return;
        }
        self.active += 1;
        self.peak_active = self.peak_active.max(self.active);
        let stalled = item.payload.device / 4 < self.stalled;
        let duration = if self.characterization && stalled {
            120_000
        } else {
            self.pull_time
        };
        let at = if stalled && !self.characterization {
            self.now.max(self.release_at) + duration
        } else {
            self.now + duration
        };
        self.busy += at - self.now;
        self.event(at, Event::Finish(worker, item));
    }
    #[allow(clippy::too_many_arguments)]
    fn run(
        workers: usize,
        capacity: i32,
        concurrent: i32,
        share: i32,
        spread: bool,
        stalled: usize,
        batches: usize,
        characterization: bool,
    ) -> Self {
        let mut s = Self {
            q: AdmissionCore::new(capacity, concurrent, share),
            events: vec![],
            sequence: 0,
            now: 0,
            next_id: 0,
            permits: Default::default(),
            active: 0,
            peak_active: 0,
            peak_depth: 0,
            peak_outstanding: 0,
            stalled_peak: 0,
            waits: vec![],
            healthy_max: 0,
            busy: 0,
            batches,
            batch_time: if characterization { 60_000 } else { 40_000 },
            pull_time: if characterization { 10_000 } else { 1_100 },
            stalled,
            release_at: 30_000 + batches as i64 * 40_000,
            characterization,
            timer_ties: false,
        };
        for d in 0..workers * 4 {
            let offset = if characterization {
                d as i64 * 1_000 + 60_000
            } else if spread {
                d as i64 * 30_000 / (workers * 4) as i64
            } else {
                0
            };
            s.event(offset, Event::Submit(d, 0));
        }
        while !s.events.is_empty() {
            let at = s.events.iter().enumerate().min_by_key(|(_, e)| (e.0, e.1)).unwrap().0;
            let (time, _, event) = s.events.remove(at);
            s.now = time;
            if !matches!(event, Event::Start(_))
                && s.events.iter().any(|e| e.0 == time && !matches!(e.2, Event::Start(_)))
            {
                s.timer_ties = true;
            }
            match event {
                Event::Submit(device, batch) => {
                    let worker: String = format!("worker-{}", device / 4).as_str().into();
                    let waiter = Waiter {
                        item: Item {
                            id: s.next_id,
                            owner: format!("{}-{}", device / 4, device % 4).as_str().into(),
                            worker: worker.clone(),
                            payload: SimWork { device, batch },
                            enqueued_at: 0,
                        },
                        arrived_at: s.now * 1_000_000,
                        blocked_by_run: None,
                        decided: SharedDeferred::new(),
                    };
                    s.next_id += 1;
                    if s.q.admissible(&worker) {
                        let info = s.q.admit(waiter, s.now * 1_000_000).2;
                        s.admitted(worker, SimWork { device, batch }, info);
                    } else {
                        s.q.wait(waiter);
                    }
                }
                Event::Start(worker) => {
                    if let Some(item) = s.q.take(&worker) {
                        s.transfer(worker, item);
                    }
                }
                Event::Finish(worker, item) => {
                    s.active -= 1;
                    if let Some((w, i)) = s.permits.pop_front() {
                        s.transfer(w, i);
                    }
                    let admitted_payloads =
                        s.q.waiting
                            .iter()
                            .map(|w| (w.decided.clone(), w.item.payload.clone()))
                            .collect::<Vec<_>>();
                    let effects = s.q.complete(&item.owner, &worker, s.now * 1_000_000);
                    for (w, d, info) in effects.admitted {
                        let payload = admitted_payloads
                            .iter()
                            .find(|(cell, _)| Arc::ptr_eq(cell, &d))
                            .unwrap()
                            .1
                            .clone();
                        s.admitted(w, payload, info);
                    }
                    s.event(s.now, Event::Start(worker));
                }
            }
        }
        assert_eq!(s.q.outstanding, 0);
        assert!(s.q.waiting.is_empty());
        assert_eq!(s.waits.len(), workers * 4 * batches);
        assert!(s.peak_active <= concurrent as usize);
        assert!(s.peak_depth <= share);
        assert!(s.peak_outstanding <= capacity);
        let mut waits = s.waits.clone();
        waits.sort();
        let stats = s.q.stats();
        assert_eq!(stats.max_wait_millis, *waits.last().unwrap());
        assert_eq!(stats.p95_wait_millis, waits[(waits.len() * 95).div_ceil(100) - 1]);
        assert_eq!(stats.total_wait_millis, waits.iter().sum::<i64>());
        s
    }
}
#[test]
fn characterization_of_the_start_of_run_burst_and_stalled_workers_on_a_72_simulator_farm() {
    let mut rows = vec![];
    for (capacity, concurrent, share) in [(8, 4, 2), (72, 8, 4), (72, 8, 72)] {
        for spread in [false, true] {
            for stalled in [0, 1, 3] {
                rows.push(Simulation::run(
                    18, capacity, concurrent, share, spread, stalled, 10, false,
                ));
            }
        }
    }
    assert!(rows[0].q.stats().max_wait_millis > 15_000);
    assert!(rows[3].q.stats().max_wait_millis <= 2_000);
    assert_eq!(rows[1].stalled_peak, 2);
    assert!(rows[13].stalled_peak > 4);
    assert!(rows[14].healthy_max > 60_000);
}
#[test]
fn recommended_large_farm_limits_admit_the_start_of_run_burst_without_delaying_a_device() {
    for spread in [false, true] {
        let s = Simulation::run(18, 72, 8, 4, spread, 0, 5, false);
        assert!(s.q.stats().max_wait_millis <= 2_000);
        assert_eq!(s.q.stats().waited, 0);
    }
}
#[test]
fn under_the_recommended_limits_a_stalled_worker_keeps_only_its_share() {
    for stalled in [1, 3, 7] {
        let s = Simulation::run(18, 72, 8, 4, false, stalled, 10, false);
        assert_eq!(s.stalled_peak, 4);
        if stalled < 7 {
            assert!(s.healthy_max <= 2_000);
            assert_eq!(s.q.stats().waited, stalled as i32 * 4);
            assert_eq!(s.q.stats().waited_for_run, 0);
        }
    }
}
#[test]
fn virtual_time_characterization_of_admission_wait_worker_depth_and_transfer_occupancy() {
    for workers in [1, 4, 16, 32] {
        let mut fair = 0;
        let mut global = 0;
        for skewed in [false, true] {
            for share in [2, 8] {
                let s = Simulation::run(workers, 8, 4, share, false, usize::from(skewed), 12, true);
                assert!(s.busy * 100 / (4 * s.now) <= 100);
                if skewed {
                    if share == 2 {
                        fair = s.healthy_max;
                    } else {
                        global = s.healthy_max;
                    }
                }
            }
        }
        if workers >= 4 {
            assert!(fair < global, "workers={workers}: fair={fair},global={global}");
        }
    }
}
#[tokio::test(start_paused = true)]
async fn transfer_concurrency_and_worker_capacity_come_from_the_queue_configuration() {
    for (concurrency, millis) in [(4, 4_000), (16, 1_000)] {
        let config = XcresultQueueConfiguration {
            capacity: 16,
            concurrency,
            worker_capacity: None,
        };
        let mut h = Harness::new(16, concurrency, config.effective_worker_capacity());
        h.p = ArtifactProcessor::from_configuration(&config, h.p.runtime.clone(), h.clock.clone(), h.p.logging.clone())
            .unwrap();
        for i in 0..16 {
            h.submit(
                &format!("device-{i}"),
                &i.to_string(),
                &format!("worker-{i}"),
                delay(1_000),
            )
            .await
            .unwrap();
        }
        assert!(h.close().await.is_empty());
        assert_eq!(h.clock.now(), millis);
    }
    let h = Harness::new(8, 4, 1);
    let gate = SharedDeferred::new();
    h.submit("first", "first", "mac", gated(gate.clone(), true))
        .await
        .unwrap();
    let second = h.submit("second", "second", "mac", empty());
    tokio::pin!(second);
    assert!(once(second.as_mut()).await.is_pending());
    gate.complete(Ok(()));
    assert!(second.await.unwrap());
    h.close().await;
    assert!(ArtifactProcessor::new(2, 4, 3, h.p.runtime.clone(), h.clock.clone(), h.p.logging.clone()).is_err());
    assert!(ArtifactProcessor::new(0, 4, 1, h.p.runtime.clone(), h.clock.clone(), h.p.logging.clone()).is_err());
}
#[tokio::test(start_paused = true)]
async fn admission_waits_are_logged_per_submission_and_summarized_at_finalization() {
    let h = Harness::new(2, 1, 2);
    let mut submitters = vec![];
    for i in 0..20 {
        let p = h.p.clone();
        let c = h.context.clone();
        submitters.push(tokio::spawn(async move {
            p.submit(
                format!("device-{i}").as_str().into(),
                i.to_string().as_str().into(),
                format!("worker-{i}").as_str().into(),
                None,
                delay(1_000),
                c,
            )
            .await
        }));
    }
    for submitter in submitters {
        assert!(submitter.await.unwrap().unwrap());
    }
    assert!(h.close().await.is_empty());
    assert_eq!(
        h.p.admission_stats(),
        AdmissionStats {
            admitted: 20,
            waited: 18,
            waited_for_run: 18,
            max_wait_millis: 18_000,
            p95_wait_millis: 17_000,
            total_wait_millis: 171_000
        }
    );
    assert!(h.messages().contains(&"Artifact queue admission: 20 bundles admitted, 18 after waiting (18 for run capacity, 0 for their worker); wait max 18000 ms, p95 17000 ms, total 171000 ms; capacity=2, concurrency=1, workerCapacity=2".to_owned()));
    for line in include_str!("../../../../fixtures/oracle/artifact-queue/events.jsonl").lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["scenario"] == "admission waits are logged per submission and summarized at finalization"
            && event["logger"] == "marathon_apple::artifact_processor"
        {
            let message = event["message"].as_str().unwrap();
            if message.contains("device-") || message.starts_with("Artifact queue admission: 20") {
                assert!(h.messages().iter().any(|m| m == message), "missing JVM event: {event}");
            }
        }
    }
}

#[test]
fn frozen_jvm_transitions_match_the_admission_core_field_by_field() {
    use serde_json::{Value, json};
    let mut queues: std::collections::BTreeMap<u64, Vec<Value>> = Default::default();
    for line in include_str!("../../../../fixtures/oracle/artifact-queue/transitions.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        queues.entry(row["queue"].as_u64().unwrap()).or_default().push(row);
    }
    let mut admissions = 0;
    for rows in queues.values() {
        let header = &rows[0];
        let scenario = header["scenario"].as_str().unwrap();
        let mut q = AdmissionCore::new(
            header["capacity"].as_i64().unwrap() as i32,
            header["concurrency"].as_i64().unwrap() as i32,
            header["share"].as_i64().unwrap() as i32,
        );
        let mut work: std::collections::HashMap<u64, (String, String)> = Default::default();
        let mut expected: std::collections::VecDeque<(u64, Admission, i64)> = Default::default();
        for (index, row) in rows.iter().enumerate().skip(1) {
            let kind = row["kind"].as_str().unwrap();
            let id = row["id"].as_u64().unwrap_or(0);
            if kind != "admit" {
                assert_eq!(
                    i64::from(q.outstanding),
                    row["outstanding"].as_i64().unwrap(),
                    "{scenario}, event {index}: {row}"
                );
            }
            if let Some(pending) = row.get("pending") {
                let actual = q
                    .pending()
                    .iter()
                    .map(|(o, n)| json!([o.to_owned(), n]))
                    .collect::<Vec<_>>();
                assert_eq!(json!(actual), *pending, "{scenario}, pending at {index}");
            }
            match kind {
                "submit" => {
                    let owner: String = row["owner"].as_str().unwrap().into();
                    let worker: String = row["worker"].as_str().unwrap().into();
                    work.insert(id, (owner.clone(), worker.clone()));
                    let waiter = Waiter {
                        item: Item {
                            id,
                            owner,
                            worker: worker.clone(),
                            payload: id,
                            enqueued_at: 0,
                        },
                        arrived_at: row["timeNanos"].as_i64().unwrap(),
                        blocked_by_run: None,
                        decided: SharedDeferred::new(),
                    };
                    if !q.closed {
                        if q.admissible(&worker) {
                            let time = rows[index + 1]["timeNanos"].as_i64().unwrap();
                            let info = q.admit(waiter, time).2;
                            expected.push_back((id, info, time));
                        } else {
                            q.wait(waiter);
                        }
                    }
                }
                "admit" => {
                    let (actual_id, info, time) = expected.pop_front().unwrap();
                    assert_eq!(actual_id, id, "{scenario}");
                    assert_eq!(time, row["timeNanos"].as_i64().unwrap());
                    assert_eq!(info.depth as i64, row["depth"].as_i64().unwrap());
                    assert_eq!(info.worker_depth as i64, row["workerDepth"].as_i64().unwrap());
                    assert_eq!(info.blocked_by_run, row["blockedByRun"].as_bool());
                    assert_eq!(info.starts_worker, row["startsWorker"].as_bool().unwrap());
                    admissions += 1;
                }
                "take" => {
                    let item = q.take(&work[&id].1).unwrap();
                    assert_eq!(item.id, id, "{scenario}: worker queue order");
                }
                "idle" => {
                    assert!(q.take(row["worker"].as_str().unwrap()).is_none());
                }
                "cancel" => {
                    assert!(q.cancel_waiter(id).is_some());
                }
                "close" => {
                    q.close();
                }
                "sweep" => {
                    q.residual();
                }
                "complete" => {
                    let mut times = rows[index + 1..]
                        .iter()
                        .take_while(|r| r["kind"] == "admit")
                        .map(|r| r["timeNanos"].as_i64().unwrap());
                    let waiting = q
                        .waiting
                        .iter()
                        .map(|w| (w.decided.clone(), w.item.id))
                        .collect::<Vec<_>>();
                    let (owner, worker) = &work[&id];
                    let effects = q.complete_with_times(owner, worker, || times.next().unwrap());
                    for (_, d, info) in effects.admitted {
                        let admitted_id = waiting.iter().find(|(cell, _)| Arc::ptr_eq(cell, &d)).unwrap().1;
                        let time = rows[index + 1 + expected.len()]["timeNanos"].as_i64().unwrap();
                        expected.push_back((admitted_id, info, time));
                    }
                }
                "stats" => {
                    let stats = q.stats();
                    assert_eq!(
                        json!({"admitted":stats.admitted,"waited":stats.waited,"waitedForRun":stats.waited_for_run,"maxWaitMillis":stats.max_wait_millis,"p95WaitMillis":stats.p95_wait_millis,"totalWaitMillis":stats.total_wait_millis}),
                        row["stats"],
                        "{scenario}"
                    );
                }
                _ => panic!("unknown JVM transition: {row}"),
            }
        }
        assert!(expected.is_empty());
        assert_eq!(q.outstanding, 0, "{scenario}");
    }
    assert!(admissions > 20_000);
}
#[tokio::test(start_paused = true)]
async fn closed_submit_callback_escapes_and_source_reinsertion_matches_jvm() {
    let h = Harness::new(2, 4, 1);
    let noop: Undelivered = Arc::new(|_| Ok(()));
    for source in ["first", "second"] {
        h.p.protect("owner".into(), source.into(), noop.clone()).unwrap();
    }
    h.p.release("owner", "first");
    h.p.protect("owner".into(), "first".into(), noop).unwrap();
    let fixture = include_str!("../../../../fixtures/oracle/artifact-queue/events.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["kind"] == "kept")
        .unwrap();
    assert_eq!(
        serde_json::json!(strings(&h.p.kept_sources("owner"))),
        fixture["sources"]
    );
    h.close().await;
    let error =
        h.p.submit(
            "owner".into(),
            "closed".into(),
            "owner".into(),
            Some(Arc::new(|_| {
                Err(failure(FailureKind::IllegalState, "callback escaped"))
            })),
            empty(),
            h.context.clone(),
        )
        .await
        .unwrap_err();
    let fixture = include_str!("../../../../fixtures/oracle/artifact-queue/events.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["kind"] == "closedCallback")
        .unwrap();
    assert_eq!(
        error.failure().to_string().to_owned(),
        marathon_test_support::recorded_errors::rendered(fixture["failure"].as_str().unwrap()).unwrap()
    );
}

#[test]
// A run-wide tie permits P2 report differences; it does not establish that a
// particular mismatch was caused by that tie. Untied runs must match exactly.
fn independent_simulator_requires_exact_jvm_reports_for_untied_runs() {
    use serde_json::{Value, json};
    let mut exact = 0;
    let mut tied = 0;
    for line in include_str!("../../../../fixtures/oracle/artifact-queue/reports.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let n = |key: &str| row[key].as_i64().unwrap();
        let (s, actual) = if row["kind"] == "sizing" {
            let s = Simulation::run(
                18,
                n("capacity") as i32,
                n("concurrency") as i32,
                n("share") as i32,
                row["spread"].as_bool().unwrap(),
                n("stalled") as usize,
                10,
                false,
            );
            let stats = s.q.stats();
            let actual = json!({"wait":stats.max_wait_millis,"healthy":s.healthy_max,"p95":stats.p95_wait_millis,
                "waited":stats.waited,"admitted":stats.admitted,"for_run":stats.waited_for_run,
                "outstanding":s.peak_outstanding,"depth":s.peak_depth,"stalled_depth":s.stalled_peak,"active":s.peak_active});
            (s, actual)
        } else {
            let s = Simulation::run(
                n("workers") as usize,
                8,
                4,
                n("share") as i32,
                false,
                usize::from(row["skewed"].as_bool().unwrap()),
                12,
                true,
            );
            let actual = json!({"runtime":s.now/1000,"mean":s.waits.iter().sum::<i64>()/s.waits.len() as i64/1000,
                "healthy":s.healthy_max/1000,"idle":s.waits.iter().sum::<i64>()/1000,
                "depth":s.peak_depth,"outstanding":s.peak_outstanding,"occupancy":s.busy*100/(4*s.now)});
            (s, actual)
        };
        if actual == row["expected"] {
            exact += 1;
        } else {
            assert!(s.timer_ties, "untied JVM report mismatch: {row}; Rust={actual}");
            tied += 1;
            println!("TIED_REPORT {}", json!({"input":row,"rust":actual}));
        }
    }
    println!("JVM_REPORT_COUNTS exact={exact} tied_differences={tied}");
    assert_eq!(exact + tied, 34);
    assert!(exact > 0);
}

struct MeasurementClock {
    elapsed: i64,
    barrier_reads: std::sync::atomic::AtomicUsize,
}
impl MonotonicClock for MeasurementClock {
    fn nanos(&self, label: &'static str) -> marathon_core::time::ClockResult<i64> {
        Ok(match label {
            "apple.ArtifactProcessor.clock" | "apple.ArtifactProcessor.closeAndJoin" => 0,
            "apple.ArtifactProcessor.await" => {
                self.barrier_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i64 * self.elapsed
            }
            _ => panic!("unexpected timing label: {label}"),
        })
    }
}
#[tokio::test(start_paused = true)]
async fn barrier_measurement_is_independent_of_the_injected_admission_clock() {
    let events: Vec<serde_json::Value> = include_str!("../../../../fixtures/oracle/artifact-queue/events.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let event = events
        .iter()
        .find(|r| {
            r["scenario"] == "witnesses"
                && r["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("Artifact barrier for clock took"))
        })
        .unwrap();
    let message = event["message"].as_str().unwrap();
    let elapsed: i64 = message.split_whitespace().nth(5).unwrap().parse().unwrap();
    let elapsed_virtual = events.iter().find(|r| r["kind"] == "measureClock").unwrap()["injectedElapsedMillis"]
        .as_i64()
        .unwrap();
    assert_ne!(elapsed, elapsed_virtual);
    let mut h = Harness::new(2, 4, 1);
    h.p = ArtifactProcessor::new(
        2,
        4,
        1,
        h.p.runtime.clone(),
        Arc::new(MeasurementClock {
            elapsed: elapsed * 1_000_000,
            barrier_reads: 0.into(),
        }),
        h.p.logging.clone(),
    )
    .unwrap();
    h.submit("clock", "batch", "clock", delay(1_000)).await.unwrap();
    h.barrier("clock").await;
    assert_eq!(h.clock.now(), elapsed_virtual);
    assert!(h.messages().contains(&message.to_owned()));
    assert!(h.close().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn submit_reads_enqueue_clock_only_for_immediate_admission() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    struct Reads(AtomicUsize);
    impl MonotonicClock for Reads {
        fn nanos(&self, label: &'static str) -> marathon_core::time::ClockResult<i64> {
            if label == "apple.ArtifactProcessor.clock" {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            Ok(0)
        }
    }
    let mut h = Harness::new(1, 1, 1);
    let reads = Arc::new(Reads::default());
    h.p = ArtifactProcessor::new(1, 1, 1, h.p.runtime.clone(), reads.clone(), h.p.logging.clone()).unwrap();
    let gate = SharedDeferred::new();
    h.submit("first", "b", "w", gated(gate.clone(), false)).await.unwrap();
    assert_eq!(reads.0.load(Ordering::SeqCst), 2);
    let cancel = Signal::new();
    let context = VendorContext {
        cancellation: cancel.clone(),
        ..h.context.clone()
    };
    let queued =
        h.p.submit("second".into(), "b".into(), "w".into(), None, empty(), context);
    tokio::pin!(queued);
    assert!(once(queued.as_mut()).await.is_pending());
    assert_eq!(reads.0.load(Ordering::SeqCst), 3);
    cancel.cancel(requested("cancel"));
    assert!(queued.await.is_err());
    gate.complete(Ok(()));
    h.close().await;
    let before = reads.0.load(Ordering::SeqCst);
    assert!(!h.submit("closed", "b", "w", empty()).await.unwrap());
    assert_eq!(reads.0.load(Ordering::SeqCst), before + 1);
}
