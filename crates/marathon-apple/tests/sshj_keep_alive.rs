//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjKeepAliveTest.kt
//! plus sshj `KeepAliveRunner` semantics (R7) on paused time, labelled as ours.
mod support;
use marathon_apple::cmd::{
    SessionScope,
    remote::ssh::sshj::{
        SshFailure,
        error::{CONNECTION_EXCEPTION, DisconnectReason, TIMEOUT_EXCEPTION},
        executor::suspend_for,
        keep_alive::{KeepAliveConnection, KeepAliveRunner},
        known_hosts::PromiscuousVerifier,
    },
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{Clock, duration, exit_state::Command, ssh};

#[tokio::test]
async fn client_expects_replies_to_keep_alive_requests() {
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.keep_alive_interval = duration(15_000);
    factory.keep_alive_max_count = 3;
    let client = factory.create_client("worker", 22, Arc::new(PromiscuousVerifier), false);
    let keep_alive = client.keep_alive_settings();
    assert_eq!(keep_alive.keep_alive_interval, 15);
    assert_eq!(keep_alive.max_alive_count, 3);
    // russh's own keep-alive, which any received data resets, stays off: the runner replaces it.
    assert_eq!(client.russh_config("worker", 22).keepalive_interval, None);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_the_exit_status_retries_poll_timeouts() {
    let mut command = Command::new(|| false, Some(0));
    command.join = Box::new(|call| {
        if call < 3 {
            Err(SshFailure::wrapping(
                CONNECTION_EXCEPTION,
                SshFailure::new(TIMEOUT_EXCEPTION, "Timeout expired"),
            ))
        } else {
            Ok(())
        }
    });
    let clock = Clock::default();
    assert_eq!(
        suspend_for(&command, &clock, Arc::new(SessionScope::default())).await,
        Ok(Some(0))
    );
    assert_eq!(command.joins.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn waiting_for_the_exit_status_fails_when_the_connection_is_lost() {
    let lost = SshFailure::connection(
        DisconnectReason::ConnectionLost,
        "Did not receive any keep-alive response for 60 seconds",
    );
    let mut command = Command::new(|| false, None);
    let answer = lost.clone();
    command.join = Box::new(move |_| Err(answer.clone()));
    let clock = Clock::default();
    let thrown = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        suspend_for(&command, &clock, Arc::new(SessionScope::default())),
    )
    .await
    .unwrap();
    assert_eq!(
        thrown,
        Err(marathon_apple::cmd::remote::ssh::sshj::executor::ExitWaitError::Failed(
            lost
        ))
    );
}

/// A transport that answers keep-alives (or not) and records when it was killed.
#[derive(Default)]
struct Transport {
    answer: AtomicBool,
    authenticated: AtomicBool,
    sent: AtomicUsize,
    died: Mutex<Option<(u128, SshFailure)>>,
    start: Mutex<Option<tokio::time::Instant>>,
}
impl KeepAliveConnection for Transport {
    fn is_running(&self) -> bool {
        self.died.lock().unwrap().is_none()
    }
    fn service_active(&self) -> bool {
        self.authenticated.load(Ordering::SeqCst)
    }
    fn send_keep_alive(&self) -> Arc<AtomicBool> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        // REQUEST_FAILURE fulfils the promise just as REQUEST_SUCCESS does.
        Arc::new(AtomicBool::new(self.answer.load(Ordering::SeqCst)))
    }
    fn die(&self, cause: SshFailure) {
        let elapsed = self.start.lock().unwrap().unwrap().elapsed().as_millis();
        *self.died.lock().unwrap() = Some((elapsed, cause));
    }
}
async fn run(interval_millis: i64, max: i32, answer: bool, run_for: u64) -> Arc<Transport> {
    let transport = Arc::new(Transport::default());
    transport.answer.store(answer, Ordering::SeqCst);
    *transport.start.lock().unwrap() = Some(tokio::time::Instant::now());
    let runner = KeepAliveRunner::new(duration(interval_millis), max);
    let connection: Arc<dyn KeepAliveConnection> = transport.clone();
    let clock = Clock::default();
    let authenticate = transport.clone();
    let scope = Arc::new(SessionScope::default());
    if runner.is_enabled() {
        // sshj starts the thread on connect; the first round still sees authentication in progress.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(run_for), async {
            tokio::join!(runner.run(connection, &clock, scope.clone()), async {
                tokio::task::yield_now().await;
                authenticate.authenticated.store(true, Ordering::SeqCst);
            })
        })
        .await;
    }
    transport
}

#[tokio::test(start_paused = true)]
async fn unanswered_keep_alives_kill_the_transport_after_max_plus_one_intervals() {
    let transport = run(2_000, 2, false, 60_000).await;
    let (at, cause) = transport.died.lock().unwrap().clone().unwrap();
    // sent at 2 s and 4 s, lost at the 6 s round: (max + 1) x interval of silence (T09: 5.9 s for 2 s x 2).
    assert_eq!(at, 6_000);
    assert_eq!(transport.sent.load(Ordering::SeqCst), 2);
    assert_eq!(cause.class, CONNECTION_EXCEPTION);
    assert_eq!(cause.reason, DisconnectReason::ConnectionLost);
    assert_eq!(
        cause.message().as_deref(),
        Some("Did not receive any keep-alive response for 4 seconds")
    );
}

#[tokio::test(start_paused = true)]
async fn answered_keep_alives_including_request_failure_keep_the_transport() {
    let transport = run(1_000, 2, true, 30_000).await;
    assert!(transport.died.lock().unwrap().is_none());
    assert!(transport.sent.load(Ordering::SeqCst) >= 29);
}

#[tokio::test(start_paused = true)]
async fn keep_alive_interval_is_truncated_to_whole_seconds() {
    // 1500 ms is 1 s (T10: lost after 3 s for 1 s x 2); 999 ms, 0 and negative values disable it.
    let transport = run(1_500, 2, false, 60_000).await;
    assert_eq!(transport.died.lock().unwrap().as_ref().unwrap().0, 3_000);
    for millis in [999, 0, -1_000] {
        assert!(!KeepAliveRunner::new(duration(millis), 2).is_enabled(), "{millis}");
    }
    assert_eq!(
        KeepAliveRunner::new(duration(i64::MAX / 1_000_000 * 1000), 2).keep_alive_interval,
        i32::MAX
    );
}
