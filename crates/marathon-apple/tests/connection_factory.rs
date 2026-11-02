//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/device/ConnectionFactoryTest.kt
mod support;
use jcompat::{JavaRegex, logging::Logger};
use marathon_apple::{
    cmd::{
        CommandExecutor, CommandHost, CommandRequest, CommandSession, failure,
        remote::ssh::sshj::{DisconnectReason, SshFailure, config::SshLogging, error::*},
    },
    device::{connection_factory::*, connection_failures::is_transient_connection_failure},
    logparser::parser::*,
};
use marathon_config::{
    devices::Transport,
    vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration},
};
use marathon_core::{error::VendorError, runtime::VendorFuture, vendor::VendorContext};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{Clock, Runtime, duration, ssh};
use tokio::sync::Semaphore;
fn worker(name: &str) -> Transport {
    Transport::Ssh {
        addr: name.into(),
        port: 22,
        authentication: None,
        check_reachability: false,
    }
}
fn config(shared: bool) -> SshConfiguration {
    SshConfiguration {
        authentication: None,
        known_hosts_path: None,
        keep_alive_interval: duration(0),
        keep_alive_max_count: 4,
        debug: false,
        share_worker_connection: shared,
    }
}
fn rsync() -> RsyncConfiguration {
    RsyncConfiguration {
        remote_path: "/usr/bin/rsync".into(),
    }
}
async fn wait(gate: &Semaphore, n: u32) {
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.acquire_many(n))
        .await
        .unwrap()
        .unwrap()
        .forget();
}
struct BlockingGate {
    open: Mutex<bool>,
    condition: Condvar,
    started: Semaphore,
}
impl BlockingGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            condition: Condvar::new(),
            started: Semaphore::new(0),
        })
    }
    fn block(&self) {
        self.started.add_permits(1);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.condition.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.condition.notify_all();
    }
}
struct Fake {
    host: CommandHost,
    logger: Logger,
    alive: AtomicBool,
    closing: AtomicBool,
    closed: AtomicUsize,
    check: Mutex<Option<Arc<BlockingGate>>>,
    check_error: Mutex<Option<ConnectionError>>,
    close_gate: Semaphore,
    close_started: Semaphore,
    close_error: Mutex<Option<VendorError>>,
}
impl Fake {
    fn new(t: &Transport, logging: &SshLogging) -> Arc<Self> {
        Arc::new(Self {
            host: CommandHost {
                id: t.id().to_string_lossy(),
                charset: "UTF-8".into(),
            },
            logger: logging.logger("FakeExecutor"),
            alive: AtomicBool::new(true),
            closing: AtomicBool::new(false),
            closed: AtomicUsize::new(0),
            check: Mutex::new(None),
            check_error: Mutex::new(None),
            close_gate: Semaphore::new(1),
            close_started: Semaphore::new(0),
            close_error: Mutex::new(None),
        })
    }
    fn count(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
    }
}
impl CommandExecutor for Fake {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn log_error(&self, _: String) {}
    fn connected(&self) -> bool {
        let gate = self.check.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.block();
        }
        assert!(!self.closing.load(Ordering::SeqCst));
        self.alive.load(Ordering::SeqCst)
    }
    fn execute(&self, _: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async {
            Err(VendorError::Other(failure(
                "java.lang.UnsupportedOperationException",
                "unused",
            )))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.closing.store(true, Ordering::SeqCst);
            self.close_started.add_permits(1);
            wait(&self.close_gate, 1).await;
            tokio::task::yield_now().await;
            self.closed.fetch_add(1, Ordering::SeqCst);
            self.alive.store(false, Ordering::SeqCst);
            self.close_error.lock().unwrap().clone().map_or(Ok(()), Err)
        })
    }
}
struct Control {
    logging: SshLogging,
    created: Mutex<Vec<Arc<Fake>>>,
    block_only: Mutex<Option<String>>,
    prepared: Semaphore,
    started: Semaphore,
    waiting: Semaphore,
    release: Semaphore,
    active: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
    fail: Mutex<Option<ConnectionError>>,
    prepare_fail: Mutex<Option<ConnectionError>>,
}
impl Control {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            logging: ssh::logging(true).0,
            created: Mutex::new(Vec::new()),
            block_only: Mutex::new(None),
            prepared: Semaphore::new(0),
            started: Semaphore::new(0),
            waiting: Semaphore::new(0),
            release: Semaphore::new(10000),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            fail: Mutex::new(None),
            prepare_fail: Mutex::new(None),
        })
    }
    fn block(&self) {
        self.release.forget_permits(self.release.available_permits());
    }
    fn last(&self) -> Arc<Fake> {
        self.created.lock().unwrap().last().unwrap().clone()
    }
    fn fake(&self, e: &Executor) -> Arc<Fake> {
        self.created
            .lock()
            .unwrap()
            .iter()
            .find(|f| Arc::ptr_eq(&((*f).clone() as Executor), e))
            .unwrap()
            .clone()
    }
}
struct TestConnector(Arc<Control>);
impl Connector for TestConnector {
    fn prepare(&self, t: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        let c = self.0.clone();
        Box::pin(async move {
            c.prepared.add_permits(1);
            if let Some(error) = c.prepare_fail.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(Box::new(move || {
                Box::pin(async move {
                    c.calls.fetch_add(1, Ordering::SeqCst);
                    let active = c.active.fetch_add(1, Ordering::SeqCst) + 1;
                    c.peak.fetch_max(active, Ordering::SeqCst);
                    c.started.add_permits(1);
                    let blocked = c
                        .block_only
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_none_or(|id| *id == t.id().to_string_lossy());
                    if blocked {
                        wait(&c.release, 1).await;
                    }
                    c.active.fetch_sub(1, Ordering::SeqCst);
                    if let Some(error) = c.fail.lock().unwrap().take() {
                        return Err(error);
                    }
                    let e = Fake::new(&t, &c.logging);
                    c.created.lock().unwrap().push(e.clone());
                    Ok(e as Executor)
                }) as ConnectionFuture<'static, Executor>
            }) as PreparedConnection)
        })
    }
    fn waiting(&self, _: &Transport) {
        self.0.waiting.add_permits(1);
    }
    fn connected(&self, e: Executor) -> ConnectionResult<bool> {
        let fake = self.0.fake(&e);
        if let Some(error) = fake.check_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(e.connected())
    }
}
struct Probe;
impl AddressProbe for Probe {
    fn check(&self, _: Transport, _: jcompat::jtime::Duration) -> ConnectionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn factory(shared: bool, max: i32) -> (Arc<ConnectionFactory>, Arc<Control>) {
    let c = Control::new();
    let f = ConnectionFactory::with_seams(
        false,
        config(shared),
        rsync(),
        duration(1000),
        max,
        Arc::new(Runtime),
        Arc::new(Clock::default()),
        c.logging.clone(),
        Arc::new(TestConnector(c.clone())),
        Arc::new(Probe),
    )
    .unwrap();
    (f, c)
}
fn call(f: &Arc<ConnectionFactory>, name: &str) -> tokio::task::JoinHandle<ConnectionResult<Executor>> {
    let f = f.clone();
    let t = worker(name);
    tokio::spawn(async move { f.connect_worker(t, support::context()).await })
}
async fn get(f: &Arc<ConnectionFactory>, name: &str) -> Executor {
    f.connect_worker(worker(name), support::context()).await.unwrap()
}
async fn done(f: &Arc<ConnectionFactory>) {
    f.join_pending().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_connect_to_one_worker_does_not_hold_up_another_worker() {
    for shared in [false, true] {
        let (f, c) = factory(shared, 8);
        c.block();
        *c.block_only.lock().unwrap() = Some("a:22".into());
        let a = call(&f, "a");
        wait(&c.started, 1).await;
        let b = get(&f, "b").await;
        assert_eq!(b.host().id, "b:22");
        assert!(!a.is_finished());
        c.release.add_permits(1);
        let a = a.await.unwrap().unwrap();
        f.dispose(a).await.unwrap();
        f.dispose(b).await.unwrap();
        done(&f).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_for_a_shared_worker_wait_for_one_connection_and_count_every_user() {
    let (f, c) = factory(true, 8);
    c.block();
    let calls = [call(&f, "a"), call(&f, "a"), call(&f, "a")];
    wait(&c.started, 1).await;
    wait(&c.waiting, 2).await;
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    c.release.add_permits(10);
    let mut users = Vec::new();
    for task in calls {
        users.push(task.await.unwrap().unwrap());
    }
    let e = users[0].clone();
    let fake = c.fake(&e);
    users.push(get(&f, "a").await);
    for user in users.drain(..3) {
        f.dispose(user).await.unwrap();
    }
    assert_eq!(fake.count(), 0);
    f.dispose(users.pop().unwrap()).await.unwrap();
    assert_eq!(fake.count(), 1);
    let next = get(&f, "a").await;
    assert!(!Arc::ptr_eq(&e, &next));
    f.dispose(next).await.unwrap();
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_shared_connection_is_replaced_and_closed_after_its_last_user() {
    let (f, c) = factory(true, 8);
    let old = get(&f, "a").await;
    let old2 = get(&f, "a").await;
    let lost = c.fake(&old);
    lost.alive.store(false, Ordering::SeqCst);
    let next = get(&f, "a").await;
    assert!(!Arc::ptr_eq(&old, &next));
    let next2 = get(&f, "a").await;
    f.dispose(old).await.unwrap();
    assert_eq!(lost.count(), 0);
    f.dispose(old2).await.unwrap();
    assert_eq!(lost.count(), 1);
    f.dispose(next).await.unwrap();
    let next3 = get(&f, "a").await;
    assert!(Arc::ptr_eq(&next2, &next3));
    f.dispose(next2).await.unwrap();
    f.dispose(next3).await.unwrap();
    done(&f).await;
}
async fn checked(lost: bool, other: bool) {
    let (f, c) = factory(true, 8);
    let e = get(&f, "a").await;
    let fake = c.fake(&e);
    let gate = BlockingGate::new();
    *fake.check.lock().unwrap() = Some(gate.clone());
    let check = call(&f, "a");
    wait(&gate.started, 1).await;
    if other {
        let b = get(&f, "b").await;
        f.dispose(b).await.unwrap();
    }
    f.dispose(e.clone()).await.unwrap();
    assert_eq!(fake.count(), 0);
    if lost {
        fake.alive.store(false, Ordering::SeqCst);
    }
    gate.release();
    let checked = check.await.unwrap().unwrap();
    assert_eq!(Arc::ptr_eq(&e, &checked), !lost);
    assert_eq!(fake.count(), usize::from(lost));
    f.dispose(checked).await.unwrap();
    assert_eq!(fake.count(), 1);
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_check_that_blocks_on_one_worker_holds_up_neither_other_workers_nor_disposal() {
    checked(false, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_being_checked_is_not_closed_by_its_last_user() {
    checked(false, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_connection_released_by_its_last_user_during_the_check_is_closed_once_and_replaced() {
    checked(true, false).await;
}
#[tokio::test]
async fn configuration_errors_propagate_instead_of_reporting_the_worker_as_unreachable() {
    let log = ssh::logging(false).0;
    let f = ConnectionFactory::new(
        false,
        config(false),
        rsync(),
        duration(1000),
        Arc::new(Runtime),
        Arc::new(Clock::default()),
        log.clone(),
    )
    .unwrap();
    let error = f.create(worker("127.0.0.1"), support::context()).await.err().unwrap();
    assert!(matches!(error, ConnectionError::Vendor(VendorError::Other(_))));
    done(&f).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(false);
    cfg.authentication = Some(SshAuthentication::Password {
        username: "fixture".into(),
        password: "fake-password".into(),
    });
    cfg.known_hosts_path = Some(jcompat::file_manager::JavaPath::from_text(dir.path().to_str().unwrap()));
    let f = ConnectionFactory::new(
        false,
        cfg,
        rsync(),
        duration(1000),
        Arc::new(Runtime),
        Arc::new(Clock::default()),
        log,
    )
    .unwrap();
    assert!(matches!(
        f.create(worker("127.0.0.1"), support::context()).await,
        Err(ConnectionError::Vendor(_))
    ));
    done(&f).await;
}
async fn failed_shared(error: SshFailure, swallow: bool) {
    let (f, c) = factory(true, 1);
    c.block();
    *c.fail.lock().unwrap() = Some(error.clone().into());
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let f = f.clone();
        tasks.push(tokio::spawn(async move {
            if swallow {
                f.create(worker("a"), support::context()).await.map(|p| p.0)
            } else {
                f.connect_worker(worker("a"), support::context()).await.map(Some)
            }
        }));
    }
    wait(&c.started, 1).await;
    wait(&c.waiting, 2).await;
    c.release.add_permits(10);
    for task in tasks {
        let result = task.await.unwrap();
        if swallow {
            assert!(result.unwrap().is_none());
        } else {
            let actual = result.err().unwrap();
            if error.is_io() {
                assert!(matches!(actual, ConnectionError::Device(_)));
            } else {
                assert_eq!(actual, ConnectionError::Ssh(error.clone()));
            }
        }
    }
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    let e = get(&f, "a").await;
    f.dispose(e).await.unwrap();
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callers_waiting_for_a_failed_shared_connection_get_none_and_a_later_request_retries() {
    failed_shared(
        SshFailure::new(IO_EXCEPTION, "Exhausted available authentication methods"),
        true,
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callers_waiting_for_a_failed_shared_connection_get_its_failure_from_connect_worker() {
    failed_shared(
        SshFailure::new("java.net.NoRouteToHostException", "No route to host"),
        false,
    )
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unexpected_connect_error_reaches_every_caller_of_the_shared_connection_and_releases_the_permit() {
    failed_shared(SshFailure::new(ILLEGAL_STATE_EXCEPTION, "broken connector"), false).await;
}
#[tokio::test]
async fn connect_worker_reports_why_a_worker_can_t_be_connected_and_only_network_failures_are_transient() {
    for shared in [false, true] {
        for (error, transient) in [
            (SshFailure::new("java.net.NoRouteToHostException", "No route"), true),
            (SshFailure::new(CONNECT_EXCEPTION, "refused"), true),
            (SshFailure::new(SOCKET_EXCEPTION, "reset"), true),
            (SshFailure::new(SOCKET_TIMEOUT_EXCEPTION, "timeout"), true),
            (SshFailure::transport(DisconnectReason::Unknown, "EOF"), true),
            (
                SshFailure::transport(DisconnectReason::TooManyConnections, "too many"),
                true,
            ),
            (
                SshFailure::new(USER_AUTH_EXCEPTION, "auth interrupted")
                    .caused_by(SshFailure::transport(DisconnectReason::ConnectionLost, "lost")),
                true,
            ),
            (
                SshFailure::wrapping(
                    USER_AUTH_EXCEPTION,
                    SshFailure::new(TIMEOUT_EXCEPTION, "Timeout expired"),
                ),
                true,
            ),
            (SshFailure::new(USER_AUTH_EXCEPTION, "Exhausted"), false),
            (
                SshFailure::transport(DisconnectReason::HostKeyNotVerifiable, "host key"),
                false,
            ),
            (SshFailure::transport(DisconnectReason::KeyExchangeFailed, "KEX"), false),
            (SshFailure::new(FILE_NOT_FOUND_EXCEPTION, "/missing"), false),
        ] {
            let (f, c) = factory(shared, 8);
            *c.fail.lock().unwrap() = Some(error.clone().into());
            let e = f.connect_worker(worker("a"), support::context()).await.err().unwrap();
            assert_eq!(is_transient_connection_failure(&e), transient);
            assert!(
                matches!(e,ConnectionError::Device(DeviceFailureException{cause:Some(ref cause),..})if cause==&error)
            );
            *c.fail.lock().unwrap() = Some(error.into());
            assert!(f.create(worker("a"), support::context()).await.unwrap().0.is_none());
            done(&f).await;
        }
    }
}
#[test]
fn failures_other_than_an_unreachable_worker_are_not_transient() {
    for reason in [
        DeviceFailureReason::Unknown,
        DeviceFailureReason::ConnectionAbort,
        DeviceFailureReason::FailedRunner,
    ] {
        assert!(!is_transient_connection_failure(&ConnectionError::Device(
            DeviceFailureException::new(reason, None, Some(SshFailure::new(CONNECT_EXCEPTION, "refused")))
        )));
    }
    assert!(!is_transient_connection_failure(
        &SshFailure::new(CONNECT_EXCEPTION, "refused").into()
    ));
    assert!(is_transient_connection_failure(&ConnectionError::Device(
        DeviceFailureException::new(DeviceFailureReason::UnreachableHost, None, None)
    )));
}
#[tokio::test]
async fn unshared_connections_are_owned_separately_and_a_failed_connect_releases_its_permit() {
    let (f, c) = factory(false, 1);
    *c.fail.lock().unwrap() = Some(SshFailure::new(IO_EXCEPTION, "failed").into());
    assert!(f.create(worker("a"), support::context()).await.unwrap().0.is_none());
    let first = get(&f, "a").await;
    let second = get(&f, "a").await;
    assert!(!Arc::ptr_eq(&first, &second));
    f.dispose(first.clone()).await.unwrap();
    assert_eq!(c.fake(&first).count(), 1);
    assert_eq!(c.fake(&second).count(), 0);
    f.dispose(second.clone()).await.unwrap();
    f.dispose(second.clone()).await.unwrap();
    assert_eq!(c.fake(&second).count(), 1);
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_close_holds_up_neither_other_workers_nor_new_connections_to_the_same_worker() {
    for shared in [false, true] {
        let (f, c) = factory(shared, 8);
        let e = get(&f, "a").await;
        let fake = c.fake(&e);
        fake.close_gate.forget_permits(1);
        let dispose = {
            let f = f.clone();
            tokio::spawn(async move { f.dispose(e).await })
        };
        wait(&fake.close_started, 1).await;
        let other = get(&f, "b").await;
        f.dispose(other).await.unwrap();
        let next = get(&f, "a").await;
        assert!(!dispose.is_finished());
        fake.close_gate.add_permits(1);
        dispose.await.unwrap().unwrap();
        f.dispose(next).await.unwrap();
        assert_eq!(fake.count(), 1);
        done(&f).await;
    }
}
async fn bounded(shared: bool, max: i32, total: usize) {
    let (f, c) = factory(shared, max);
    c.block();
    let tasks = (0..total).map(|i| call(&f, &format!("worker-{i}"))).collect::<Vec<_>>();
    wait(&c.prepared, total as u32).await;
    wait(&c.started, max as u32).await;
    assert_eq!(c.active.load(Ordering::SeqCst), max as usize);
    c.release.add_permits(total);
    for task in tasks {
        f.dispose(task.await.unwrap().unwrap()).await.unwrap();
    }
    assert_eq!(c.peak.load(Ordering::SeqCst), max as usize);
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_setup_is_bounded_across_workers() {
    for shared in [false, true] {
        bounded(shared, 2, 5).await;
        assert_eq!(DEFAULT_MAX_CONCURRENT_CONNECTS, 8);
        bounded(shared, DEFAULT_MAX_CONCURRENT_CONNECTS, 12).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_create_and_dispose_never_hand_out_a_closing_shared_connection() {
    let (f, c) = factory(true, 8);
    let mut tasks = Vec::new();
    for thread in 0..8 {
        let f = f.clone();
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..300 {
                let e = get(&f, if (i + thread) % 2 == 0 { "a" } else { "b" }).await;
                let fake = c.fake(&e);
                assert!(!fake.closing.load(Ordering::SeqCst));
                if i % 50 == thread {
                    fake.alive.store(false, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
                f.dispose(e).await.unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    done(&f).await;
    let all = c.created.lock().unwrap();
    assert!(all.len() > 2);
    assert!(all.iter().all(|e| e.count() == 1));
}
#[tokio::test]
async fn thrown_checks_release_the_checking_reference() {
    let (f, c) = factory(true, 8);
    let e = get(&f, "a").await;
    *c.fake(&e).check_error.lock().unwrap() = Some(SshFailure::new(ILLEGAL_STATE_EXCEPTION, "check failed").into());
    assert!(f.connect_worker(worker("a"), support::context()).await.is_err());
    f.dispose(e.clone()).await.unwrap();
    assert_eq!(c.fake(&e).count(), 1);
    done(&f).await;
}
#[tokio::test]
async fn pending_cleanup_joins_later_closes_before_returning_the_first_error() {
    let (f, c) = factory(true, 8);
    let first = get(&f, "first").await;
    let second = get(&f, "second").await;
    done(&f).await;
    let error = VendorError::Other(failure("java.io.IOException", "close failed"));
    *c.fake(&first).close_error.lock().unwrap() = Some(error.clone());
    assert_eq!(f.dispose(first).await, Err(error.clone()));

    let second_fake = c.fake(&second);
    second_fake.close_gate.forget_permits(1);
    let closing = {
        let f = f.clone();
        tokio::spawn(async move { f.dispose(second).await })
    };
    wait(&second_fake.close_started, 1).await;
    let mut cleanup = std::pin::pin!(f.join_pending());
    std::future::poll_fn(|context| {
        assert!(cleanup.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    second_fake.close_gate.add_permits(1);
    assert_eq!(support::watchdog(cleanup).await, Err(error));
    closing.await.unwrap().unwrap();
    assert_eq!(second_fake.count(), 1);
    done(&f).await;
}
/// Records every task the factory spawns, so a test can join them.
struct Recording(std::sync::Mutex<Vec<Arc<dyn marathon_core::runtime::VendorTask>>>);
impl marathon_core::runtime::VendorRuntime for Recording {
    fn spawn_cancellable(
        &self,
        body: marathon_core::runtime::VendorTaskBody,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        let task = Runtime.spawn_cancellable(body);
        self.0.lock().unwrap().push(task.clone());
        task
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn marathon_core::runtime::VendorTask> {
        let task = Runtime.spawn(body);
        self.0.lock().unwrap().push(task.clone());
        task
    }
    fn spawn_blocking(
        &self,
        body: Box<dyn FnOnce() -> marathon_core::error::VendorResult<()> + Send>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        let task = Runtime.spawn_blocking(body);
        self.0.lock().unwrap().push(task.clone());
        task
    }
}
/// Not a Kotlin test: retained tasks must not grow with every create/dispose of a long run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn succeeded_tasks_are_pruned_when_new_ones_are_retained() {
    let c = Control::new();
    let runtime = Arc::new(Recording(std::sync::Mutex::new(Vec::new())));
    let f = ConnectionFactory::with_seams(
        false,
        config(true),
        rsync(),
        duration(1000),
        8,
        runtime.clone(),
        Arc::new(Clock::default()),
        c.logging.clone(),
        Arc::new(TestConnector(c.clone())),
        Arc::new(Probe),
    )
    .unwrap();
    for _ in 0..200 {
        let e = get(&f, "a").await;
        // connect_worker returns on the create task's acknowledgement, before its body
        // returns. Join every task spawned since the last cycle; dispose joins its own.
        let spawned = std::mem::take(&mut *runtime.0.lock().unwrap());
        for task in spawned {
            support::watchdog(task.join()).await.unwrap();
        }
        f.dispose(e).await.unwrap();
        // The dispose's spawn pruned every earlier task, all succeeded; only its own remains.
        assert_eq!(f.retained_tasks(), 1);
    }
    done(&f).await;
    assert_eq!(f.retained_tasks(), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiter_and_abandoned_owner_finish_and_dispose_undelivered_leases() {
    let (f, c) = factory(true, 8);
    c.block();
    let owner = call(&f, "a");
    wait(&c.started, 1).await;
    let waiter = call(&f, "a");
    wait(&c.waiting, 1).await;
    owner.abort();
    assert!(owner.await.is_err());
    c.release.add_permits(10);
    let e = waiter.await.unwrap().unwrap();
    done(&f).await;
    assert_eq!(c.fake(&e).count(), 0);
    f.dispose(e.clone()).await.unwrap();
    assert_eq!(c.fake(&e).count(), 1);
    c.block();
    let abandoned = call(&f, "b");
    wait(&c.started, 1).await;
    abandoned.abort();
    assert!(abandoned.await.is_err());
    c.release.add_permits(1);
    done(&f).await;
    assert_eq!(c.last().count(), 1);
}
#[test]
fn all_disconnect_reasons_cause_depth_and_device_reason_mapping() {
    for code in 0..16 {
        let reason = DisconnectReason::from_int(code);
        let transient = matches!(code, 0 | 5 | 7 | 10 | 11 | 12);
        let error = ConnectionError::Device(DeviceFailureException::new(
            DeviceFailureReason::UnreachableHost,
            None,
            Some(SshFailure::transport(reason, "wire")),
        ));
        assert_eq!(is_transient_connection_failure(&error), transient);
    }
    for depth in [16, 17] {
        let mut cause = SshFailure::new(CONNECT_EXCEPTION, "refused");
        for _ in 1..depth {
            cause = SshFailure::new(IO_EXCEPTION, "outer").caused_by(cause);
        }
        assert_eq!(
            is_transient_connection_failure(&ConnectionError::Device(DeviceFailureException::new(
                DeviceFailureReason::UnreachableHost,
                None,
                Some(cause)
            ))),
            depth == 16
        );
    }
    for reason in [
        DeviceFailureReason::Unknown,
        DeviceFailureReason::FailedRunner,
        DeviceFailureReason::ConnectionAbort,
        DeviceFailureReason::InvalidSimulatorIdentifier,
        DeviceFailureReason::UnreachableHost,
        DeviceFailureReason::ServicesUnavailable,
        DeviceFailureReason::IncompatibleDevice,
        DeviceFailureReason::SystemAppUnavailable,
    ] {
        let e = DeviceFailureException::new(reason, None, None);
        assert_eq!(e.message, reason.name());
        assert_eq!(
            reason.is_batch_failure(),
            matches!(reason, DeviceFailureReason::Unknown | DeviceFailureReason::FailedRunner)
        );
        let regex = JavaRegex::compile("a.*b", "test").unwrap();
        assert_eq!(DeviceFailureException::from_regex(reason, &regex).message, "a.*b");
    }
}
#[tokio::test]
async fn bridges_share_only_endpoint_while_executors_key_the_whole_transport() {
    let (f, _) = factory(true, 8);
    let a = get(&f, "a").await;
    let mut transport = worker("a");
    if let Transport::Ssh { authentication, .. } = &mut transport {
        *authentication = Some(SshAuthentication::Password {
            username: "other".into(),
            password: "fake-password".into(),
        });
    }
    let (e, bridge) = f.create_or_throw(transport, support::context()).await.unwrap();
    let (a2, first) = f.create_or_throw(worker("a"), support::context()).await.unwrap();
    assert!(Arc::ptr_eq(&first, &bridge));
    assert!(
        bridge
            .send(std::path::Path::new("/unused"), "/tmp/marathon/dst")
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(&a, &a2));
    assert!(!Arc::ptr_eq(&a, &e));
    for e in [a, a2, e] {
        f.dispose(e).await.unwrap();
    }
    done(&f).await;
}
#[tokio::test]
async fn reachability_uses_echo_and_rejects_approved_numeric_edges() {
    let probe = EchoAddressProbe {
        runtime: Arc::new(Runtime),
    };
    for host in ["127.0.0.1", "::1"] {
        let mut t = worker(host);
        if let Transport::Ssh { check_reachability, .. } = &mut t {
            *check_reachability = true;
        }
        probe.check(t.clone(), duration(150)).await.unwrap();
        for ms in [-1, i32::MAX as i64 + 1] {
            assert!(matches!(
                probe.check(t.clone(), duration(ms)).await,
                Err(ConnectionError::Vendor(_))
            ));
        }
    }
    let Err(ConnectionError::Device(error)) = probe.check(worker("no-such-host.invalid"), duration(1)).await else {
        panic!("unresolvable host must be a device failure");
    };
    let cause = error.cause.expect("UnknownHostException cause");
    assert_eq!(cause.class, "java.net.UnknownHostException");
    // getaddrinfo's text is platform-specific; only the JDK's "<host>: " prefix is pinned.
    assert!(cause.message().unwrap().starts_with("no-such-host.invalid: "));
}

/// fixtures/oracle/connections/reachability-{macos,linux}.jsonl: broadcast and
/// multicast fail connect immediately (EAFNOSUPPORT on macOS, ENETUNREACH on
/// Linux), which tcp_ping4 reports as unreachable rather than throwing.
#[tokio::test]
async fn reachability_reports_immediate_broadcast_and_multicast_failures_as_unreachable() {
    let probe = EchoAddressProbe {
        runtime: Arc::new(Runtime),
    };
    for host in ["255.255.255.255", "224.0.0.1"] {
        let mut t = worker(host);
        if let Transport::Ssh { check_reachability, .. } = &mut t {
            *check_reachability = true;
        }
        let Err(ConnectionError::Device(error)) = probe.check(t, duration(150)).await else {
            panic!("{host} must be unreachable");
        };
        assert_eq!(
            error.message,
            format!(
                "Ssh(addr={host}, port=22, authentication=null, checkReachability=true) is not reachable within 150 ms"
            )
        );
    }
}

#[test]
fn a16_failed_kex_is_transient_despite_the_source_kdoc() {
    let error = ConnectionError::Device(DeviceFailureException::new(
        DeviceFailureReason::UnreachableHost,
        None,
        Some(SshFailure::transport(
            DisconnectReason::Unknown,
            "Unable to reach a settlement",
        )),
    ));
    assert!(is_transient_connection_failure(&error));
}
#[tokio::test]
async fn local_create_registers_but_direct_local_and_repeated_disposal_only_warn() {
    let (f, _) = factory(true, 8);
    let (direct, _) = f.create_local();
    f.dispose(direct).await.unwrap();
    let (registered, _) = f.create_or_throw(Transport::Local, support::context()).await.unwrap();
    f.dispose(registered.clone()).await.unwrap();
    f.dispose(registered).await.unwrap();
    done(&f).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiter_leaves_the_shared_attempt_for_its_owner() {
    use marathon_apple::cmd::{SessionScope, cancelled};
    let (f, c) = factory(true, 8);
    c.block();
    let owner = call(&f, "a");
    wait(&c.started, 1).await;
    let signal = Arc::new(SessionScope::default());
    let mut context = support::context();
    context.cancellation = signal.clone();
    let waiter = {
        let f = f.clone();
        tokio::spawn(async move { f.connect_worker(worker("a"), context).await })
    };
    wait(&c.waiting, 1).await;
    let reason = cancelled("cancelled waiter");
    signal.cancel(reason.clone());
    assert!(
        matches!(waiter.await.unwrap(),Err(ConnectionError::Vendor(VendorError::Cancelled(ref actual)))if actual==&reason)
    );
    assert!(!owner.is_finished());
    c.release.add_permits(10);
    let e = owner.await.unwrap().unwrap();
    done(&f).await;
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.fake(&e).count(), 0);
    f.dispose(e.clone()).await.unwrap();
    assert_eq!(c.fake(&e).count(), 1);
    done(&f).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preparation_is_outside_a_fair_permit_and_auth_errors_release_no_lease() {
    let (f, c) = factory(false, 1);
    c.block();
    let calls = [call(&f, "first"), call(&f, "second"), call(&f, "third")];
    wait(&c.prepared, 3).await;
    wait(&c.started, 1).await;
    assert_eq!(c.active.load(Ordering::SeqCst), 1);
    c.release.add_permits(3);
    for task in calls {
        f.dispose(task.await.unwrap().unwrap()).await.unwrap();
    }
    done(&f).await;
    for max in [0, -1] {
        let error = ConnectionFactory::with_seams(
            false,
            config(false),
            rsync(),
            duration(1),
            max,
            Arc::new(Runtime),
            Arc::new(Clock::default()),
            c.logging.clone(),
            Arc::new(TestConnector(c.clone())),
            Arc::new(Probe),
        )
        .err()
        .unwrap();
        assert_eq!(
            error,
            ConnectionError::Ssh(SshFailure::new(
                ILLEGAL_ARGUMENT_EXCEPTION,
                format!("maxConcurrentConnects should be positive, got {max}")
            ))
        );
    }
}

#[tokio::test]
async fn zero_budget_reachability_observes_the_nonblocking_connect_race() {
    let probe = EchoAddressProbe {
        runtime: Arc::new(Runtime),
    };
    // Fresh JDK probes observe both outcomes: completion may race the zero-budget poll.
    for host in ["127.0.0.1", "::1"] {
        let mut t = worker(host);
        if let Transport::Ssh { check_reachability, .. } = &mut t {
            *check_reachability = true;
        }
        probe.check(t.clone(), duration(150)).await.unwrap();
        if let Err(error) = probe.check(t, duration(0)).await {
            assert!(
                matches!(error, ConnectionError::Device(ref failure)
                if failure.reason == DeviceFailureReason::UnreachableHost
                    && failure.message.ends_with("is not reachable within 0 ms")
                    && failure.cause.is_none()),
                "{error:?}"
            );
        }
    }
}
