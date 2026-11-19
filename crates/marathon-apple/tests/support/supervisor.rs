use super::support::{context, duration, ssh};
use fake_worker::{
    fs::VirtualFs,
    router::{Handler, HandlerFuture, Request},
    script::{Action, Chunk},
};
use marathon_apple::cmd::{CommandExecutor, CommandRequest, CommandSession, remote::ssh::sshj::SshCommandExecutor};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
    time::Instant,
};

type Groups = Arc<Mutex<HashMap<String, Group>>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running(u64),
    Done(i32),
}
#[derive(Clone)]
struct Group {
    name: String,
    duration: u64,
    state: watch::Sender<State>,
}
struct Worker {
    origin: Instant,
    groups: Groups,
    silent: AtomicBool,
    stall_open: AtomicBool,
}
impl Worker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            groups: Default::default(),
            silent: AtomicBool::new(false),
            stall_open: AtomicBool::new(false),
        })
    }
    fn group(&self, name: &str) -> Group {
        self.groups
            .lock()
            .unwrap()
            .values()
            .find(|group| group.name == name)
            .unwrap()
            .clone()
    }
    async fn complete(&self, fs: &VirtualFs, name: &str) {
        let group = self.group(name);
        assert!(
            matches!(*group.state.borrow(), State::Running(_)),
            "{name} failed: {:?}",
            group.state.borrow()
        );
        group.state.send_replace(State::Done(0));
        fs.write(&format!("/tmp/marathon/proof-{name}"), Vec::new(), 0o600)
            .await
            .unwrap();
    }
}
impl Handler for Worker {
    fn name(&self) -> &str {
        "virtual-supervisor"
    }
    fn route<'a>(&'a self, request: &'a Request, _fs: &'a VirtualFs) -> HandlerFuture<'a> {
        Box::pin(async move {
            let argv: Vec<_> = request.argv.iter().map(String::as_str).collect();
            const ROOT: &str = "/tmp/marathon-supervisor-abcdefghij";
            const PROGRAM: &str = "/tmp/marathon-supervisor-abcdefghij/supervisor.pl";
            let now = self.origin.elapsed().as_millis() as u64;
            match argv.as_slice() {
                ["/usr/bin/perl", "-MMIME::Base64", "-e", _, "--", _, _] => {
                    Ok(Some(Action::output(&format!("MARATHON_D96_1 {ROOT}\n"), 0)))
                }
                ["/usr/bin/perl", PROGRAM, "control-stream", ROOT, _]
                    if self.stall_open.swap(false, Ordering::AcqRel) =>
                {
                    std::future::pending().await
                }
                [
                    "exec",
                    "/usr/bin/perl",
                    PROGRAM,
                    mode @ ("run" | "detach"),
                    ROOT,
                    period,
                    token,
                    "/bin/sh",
                    "-c",
                    command,
                ] => {
                    let name = if *mode == "detach" {
                        "recorder"
                    } else {
                        command
                            .strip_prefix("echo ")
                            .ok_or_else(|| std::io::Error::other("payload"))?
                    };
                    let duration = period.parse::<u64>()?;
                    let state = watch::channel(State::Running(now + duration)).0;
                    let group = Group {
                        name: name.into(),
                        duration,
                        state: state.clone(),
                    };
                    assert!(self.groups.lock().unwrap().insert((*token).into(), group).is_none());
                    let mut clock = state.subscribe();
                    let origin = self.origin;
                    tokio::spawn(async move {
                        loop {
                            let State::Running(expiry) = *clock.borrow_and_update() else {
                                break;
                            };
                            tokio::select! {
                                _ = tokio::time::sleep_until(origin + Duration::from_millis(expiry)) => {
                                    state.send_if_modified(|current| {
                                        if *current != State::Running(expiry) { return false; }
                                        *current = State::Done(137);
                                        true
                                    });
                                }
                                changed = clock.changed() => if changed.is_err() { break; },
                            }
                        }
                    });
                    Ok(Some(if *mode == "detach" {
                        Action::output("", 0)
                    } else {
                        ssh::output(
                            vec![
                                ssh::chunk("ready\n"),
                                Chunk {
                                    data: "finished\n".into(),
                                    wait_for_file: Some(format!("/tmp/marathon/proof-{name}")),
                                    ..Default::default()
                                },
                            ],
                            0,
                        )
                    }))
                }
                ["/usr/bin/perl", PROGRAM, "control", ROOT, _, request] => {
                    let (token, op) = request.split_once(':').unwrap();
                    if self.silent.load(Ordering::Acquire) {
                        return Ok(Some(ssh::hang(vec![])));
                    }
                    let group = self.groups.lock().unwrap().get(token).cloned();
                    let reply = if let Some(group) = group {
                        if group.name == "recorder" && op == "1" {
                            return Ok(Some(ssh::hang(vec![])));
                        }
                        group.state.send_if_modified(|state| match *state {
                            State::Running(stamp) if now < stamp && op == format!("renew-{stamp}") => {
                                *state = State::Running((now + group.duration).max(stamp));
                                true
                            }
                            State::Running(_) if op == "3" || (group.name == "recorder" && op == "2") => {
                                *state = State::Done(if op == "3" { 137 } else { 143 });
                                true
                            }
                            _ => false,
                        });
                        match *group.state.borrow() {
                            State::Running(stamp) => format!("running:{stamp}"),
                            State::Done(status) => format!("done:{status}"),
                        }
                    } else {
                        "unknown".into()
                    };
                    Ok(Some(Action::output(&format!("{token} {reply}\n"), 0)))
                }
                ["/usr/bin/perl", PROGRAM, "retire", ROOT, _] => {
                    Ok(Some(Action::output("MARATHON_D96_1 retired\n", 0)))
                }
                _ => Ok(None),
            }
        })
    }
}

struct Proxy {
    port: u16,
    accept: JoinHandle<()>,
    blocked: Arc<AtomicBool>,
    links: Arc<Mutex<Vec<Link>>>,
}
struct Link {
    task: JoinHandle<()>,
    held: watch::Sender<bool>,
}
impl Proxy {
    async fn start(worker: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let blocked = Arc::new(AtomicBool::new(false));
        let links = Arc::new(Mutex::new(Vec::new()));
        let accept = {
            let blocked = blocked.clone();
            let links = links.clone();
            tokio::spawn(async move {
                while let Ok((mut downstream, _)) = listener.accept().await {
                    if !blocked.load(Ordering::Acquire) {
                        let (held, paused) = watch::channel(false);
                        let task = tokio::spawn(async move {
                            if let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", worker)).await {
                                let _ = downstream.set_nodelay(true);
                                let _ = upstream.set_nodelay(true);
                                let (from_client, to_client) = downstream.split();
                                let (from_worker, to_worker) = upstream.split();
                                let _ = tokio::try_join!(
                                    relay(from_client, to_worker, Some(paused)),
                                    relay(from_worker, to_client, None),
                                );
                            }
                        });
                        links.lock().unwrap().push(Link { task, held });
                    }
                }
            })
        };
        Self {
            port,
            accept,
            blocked,
            links,
        }
    }
    fn block_new(&self) {
        self.blocked.store(true, Ordering::Release);
    }
    fn cut(&self, connection: usize) {
        self.links.lock().unwrap()[connection].task.abort();
    }
    fn hold(&self, connection: usize, value: bool) {
        self.links.lock().unwrap()[connection].held.send_replace(value);
    }
    fn count(&self) -> usize {
        self.links.lock().unwrap().len()
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        for link in self.links.lock().unwrap().drain(..) {
            link.task.abort();
        }
    }
}

async fn relay(
    mut from: impl tokio::io::AsyncRead + Unpin,
    mut to: impl tokio::io::AsyncWrite + Unpin,
    mut held: Option<watch::Receiver<bool>>,
) -> std::io::Result<()> {
    let mut buf = [0; 16 * 1024];
    loop {
        let size = from.read(&mut buf).await?;
        if size == 0 {
            return to.shutdown().await;
        }
        if let Some(held) = &mut held
            && held.wait_for(|held| !held).await.is_err()
        {
            return Ok(());
        }
        to.write_all(&buf[..size]).await?;
    }
}

async fn executor(port: u16) -> SshCommandExecutor {
    executor_with_keep_alive(port, 1000).await
}

async fn executor_with_keep_alive(port: u16, interval: i64) -> SshCommandExecutor {
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.keep_alive_interval = duration(interval);
    factory.keep_alive_max_count = 4;
    factory
        .connect_worker("127.0.0.1", port, ssh::password(), None, false, Arc::default())
        .await
        .unwrap()
}
async fn open(executor: &dyn CommandExecutor, name: &str) -> CommandSession {
    let session = executor
        .execute(CommandRequest::new(["echo", name], duration(120000)), context())
        .await
        .unwrap();
    assert_eq!(session.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
    session
}
async fn wait_for(mut group: watch::Receiver<State>, predicate: impl Fn(&State) -> bool) -> State {
    let state = tokio::time::timeout(Duration::from_secs(6), group.wait_for(predicate))
        .await
        .unwrap()
        .unwrap();
    *state
}
fn stamp(group: &Group) -> u64 {
    let State::Running(stamp) = *group.state.borrow() else {
        panic!("worker group already expired")
    };
    stamp
}
async fn advance(seconds: u64) {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(seconds)).await;
    tokio::time::resume();
}
async fn finish(worker: &Worker, fake: &ssh::Fake, name: &str, session: CommandSession) {
    worker.complete(&fake.worker.fs, name).await;
    let result = session.await_result(&context()).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    session.close().await.unwrap();
}

async fn control_loss(reconnect: bool) {
    let worker = Worker::new();
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 8, |_| {}, vec![worker.clone()]).await;
    let proxy = Proxy::start(fake.port()).await;
    let executor = executor(proxy.port).await;
    let first = open(&executor, "first").await;
    let sibling = open(&executor, "sibling").await;
    let group = worker.group("first");
    let start = stamp(&group);
    advance(2).await;
    wait_for(
        group.state.subscribe(),
        |state| matches!(state, State::Running(stamp) if *stamp > start),
    )
    .await;
    let original = stamp(&group);
    assert_eq!(proxy.count(), 2);
    if !reconnect {
        proxy.block_new();
    }
    proxy.cut(1);
    // Let the monotonic stamp move before asking the watcher to renew.
    advance(3).await;
    wait_for(
        group.state.subscribe(),
        |state| matches!(state, State::Running(stamp) if *stamp > original + 1500),
    )
    .await;
    assert_eq!(proxy.count(), if reconnect { 3 } else { 2 });
    // Cross the original expiry while both real SSH command sessions remain
    // active. Wait for their worker-side renewal stamps before moving time.
    let other = worker.group("sibling");
    for _ in 0..5 {
        let first_stamp = stamp(&group);
        let sibling_stamp = stamp(&other);
        advance(6).await;
        tokio::join!(
            wait_for(
                group.state.subscribe(),
                |state| matches!(state, State::Running(stamp) if *stamp > first_stamp)
            ),
            wait_for(
                other.state.subscribe(),
                |state| matches!(state, State::Running(stamp) if *stamp > sibling_stamp)
            ),
        );
    }
    finish(&worker, &fake, "first", first).await;
    finish(&worker, &fake, "sibling", sibling).await;
    executor.close().await.unwrap();
    drop(executor);
    fake.worker.shutdown().await.unwrap();
}

async fn with_paused_time(test: impl Future<Output = ()>) {
    tokio::time::pause();
    // The loopback sockets still use real I/O. Only this test advances the timers.
    let (finished, waiting) = std::sync::mpsc::channel::<()>();
    let watchdog = tokio::task::spawn_blocking(move || waiting.recv_timeout(Duration::from_secs(60)));
    tokio::select! {
        () = test => {},
        _ = watchdog => panic!("test exceeded the wall-clock watchdog"),
    }
    drop(finished);
    tokio::time::resume();
}

async fn io_tick() {
    tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(2)))
        .await
        .unwrap();
}

async fn advance_until(window: Duration, mut ready: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + window;
    while !ready().await {
        assert!(Instant::now() < deadline, "no progress within {window:?}");
        tokio::time::advance(Duration::from_millis(50)).await;
        io_tick().await;
    }
}

async fn warm_control(worker: &Worker, name: &str) -> Group {
    let group = worker.group(name);
    let start = stamp(&group);
    advance_until(Duration::from_secs(17), async || stamp(&group) > start).await;
    group
}

#[tokio::test]
async fn silent_control_reconnects_with_keep_alive_off_and_no_spare_session_d96() {
    let worker = Worker::new();
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker.clone()]).await;
    let proxy = Proxy::start(fake.port()).await;
    let executor = executor_with_keep_alive(proxy.port, 0).await;
    let session = open(&executor, "leased").await;
    with_paused_time(async {
        let group = warm_control(&worker, "leased").await;
        let original = stamp(&group);
        assert_eq!(proxy.count(), 2);
        proxy.hold(1, true);

        // A first miss tries the data path, which has no session to spare.
        advance_until(Duration::from_secs(22), async || {
            !fake.events("session_rejected").await.is_empty()
        })
        .await;
        assert_eq!(proxy.count(), 2, "one unanswered request must leave the link open");
        // The client has to replace the still-open TCP link before the worker's lease expires.
        let before_expiry = Duration::from_millis(original).saturating_sub(worker.origin.elapsed());
        advance_until(before_expiry, async || stamp(&group) > original).await;
        assert_eq!(proxy.count(), 3);
    })
    .await;
    finish(&worker, &fake, "leased", session).await;
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_silent_control_open_is_replaced_before_the_lease_expires_d96() {
    let worker = Worker::new();
    worker.stall_open.store(true, Ordering::Release);
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker.clone()]).await;
    let proxy = Proxy::start(fake.port()).await;
    let executor = executor_with_keep_alive(proxy.port, 0).await;
    let session = open(&executor, "leased").await;
    with_paused_time(async {
        let group = worker.group("leased");
        let original = stamp(&group);
        let before_expiry = Duration::from_millis(original).saturating_sub(worker.origin.elapsed());
        advance_until(before_expiry, async || stamp(&group) > original).await;
        assert_eq!(proxy.count(), 3);
    })
    .await;
    finish(&worker, &fake, "leased", session).await;
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_control_replies_clear_the_timeout_streak_d96() {
    let worker = Worker::new();
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker.clone()]).await;
    let proxy = Proxy::start(fake.port()).await;
    let executor = executor_with_keep_alive(proxy.port, 0).await;
    let session = open(&executor, "leased").await;
    with_paused_time(async {
        warm_control(&worker, "leased").await;
        let replies = async || fake.events("supervisor_reply").await.len();
        advance_until(Duration::from_secs(1), async || replies().await >= 2).await;
        for misses in 1..=2 {
            let previous = replies().await;
            proxy.hold(1, true);
            advance_until(Duration::from_secs(22), async || {
                fake.events("session_rejected").await.len() >= misses
            })
            .await;
            assert_eq!(proxy.count(), 2);
            proxy.hold(1, false);
            // The first timeout has already tried the data path when this reply is released.
            advance_until(Duration::from_secs(2), async || replies().await > previous).await;
        }
        let previous = replies().await;
        advance_until(Duration::from_secs(17), async || {
            proxy.count() > 2 || replies().await > previous
        })
        .await;
        assert_eq!(proxy.count(), 2, "a late reply clears the prior miss");
    })
    .await;
    finish(&worker, &fake, "leased", session).await;
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn exit_proof_is_bounded_while_the_data_path_is_down_d96() {
    use marathon_apple::cmd::{TransferCleanup, device_commands::DeviceCommands};
    use marathon_core::executor::shared::Signal;
    let worker = Worker::new();
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker.clone()]).await;
    let proxy = Proxy::start(fake.port()).await;
    let executor = Arc::new(executor_with_keep_alive(proxy.port, 0).await);
    let device = DeviceCommands::new(executor.clone());
    let session = open(device.as_ref(), "unreachable").await;
    let receipt = device.handed_commands().pop().unwrap();
    with_paused_time(async {
        warm_control(&worker, "unreachable").await;
        worker.silent.store(true, Ordering::Release);
        proxy.cut(0);
        let kill_requests = async || {
            fake.events("supervisor_request")
                .await
                .iter()
                .filter(|row| super::matches_operation(row, ":3"))
                .count()
        };
        while kill_requests().await == 0 {
            io_tick().await;
        }
        let ending = Instant::now();
        advance_until(Duration::from_secs(10), async || kill_requests().await >= 2).await;
        let before_bound = ending + Duration::from_millis(31_500);
        while Instant::now() < before_bound {
            tokio::time::advance(Duration::from_millis(50)).await;
            io_tick().await;
        }
        assert!(receipt.ended().is_none());
        advance_until(Duration::from_millis(600), async || receipt.ended().is_some()).await;
        let TransferCleanup::Unconfirmed(error) = receipt.wait_in(Signal::new()).await.unwrap() else {
            panic!("cleanup requires a worker receipt");
        };
        assert!(error.to_string().contains("Worker exit proof did not arrive"));
    })
    .await;
    drop(session);
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn recorder_term_preempts_an_unanswered_int_d96() {
    use marathon_apple::bin::{
        Tools,
        xcrun::simctl::{SimctlService, service::IoService},
    };
    use marathon_config::vendor::{AppleTimeoutParameters, Codec, Display, Mask, apple_timeout_configuration};
    let worker = Worker::new();
    let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker]).await;
    let executor = Arc::new(executor_with_keep_alive(fake.port(), 0).await);
    let (logging, _) = ssh::logging(false);
    let io = IoService::new(SimctlService::new(Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(super::support::Runtime),
        logging,
    }));
    let recorder = io
        .record_video(
            "fixture",
            "/tmp/marathon/video.mp4",
            Codec::H264,
            Display::Internal,
            Mask::Black,
            &context(),
        )
        .await
        .unwrap();
    with_paused_time(async {
        recorder.interrupt().unwrap();
        let requests = async || fake.events("supervisor_request").await;
        while !requests().await.iter().any(|row| super::matches_operation(row, ":1")) {
            io_tick().await;
        }
        recorder.terminate().unwrap();
        advance_until(Duration::from_secs(1), async || {
            requests().await.iter().any(|row| super::matches_operation(row, ":2"))
        })
        .await;
        let rows = requests().await;
        let int = rows.iter().find(|row| super::matches_operation(row, ":1")).unwrap();
        let term = rows.iter().find(|row| super::matches_operation(row, ":2")).unwrap();
        assert_eq!(int["connection"], term["connection"]);
        assert_eq!(int["channel"], term["channel"]);
        assert_eq!(recorder.await_result(&context()).await.unwrap().exit_code, Some(143));
    })
    .await;
    recorder.close().await.unwrap();
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn control_reconnects_before_healthy_groups_expire_d96() {
    super::support::watchdog(control_loss(true)).await;
}
#[tokio::test]
async fn an_alive_command_connection_renews_during_control_outage_d96() {
    super::support::watchdog(control_loss(false)).await;
}

#[tokio::test]
async fn the_only_control_session_can_retire_the_worker_after_data_loss_d96() {
    super::support::watchdog(async {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let worker = Worker::new();
        let fake = ssh::fake_with_handlers(ssh::script(vec![]), 1, |_| {}, vec![worker.clone()]).await;
        let proxy = Proxy::start(fake.port()).await;
        let executor = executor(proxy.port).await;
        let session = open(&executor, "retiring").await;
        let group = worker.group("retiring");
        let start = stamp(&group);
        advance(2).await;
        wait_for(
            group.state.subscribe(),
            |state| matches!(state, State::Running(stamp) if *stamp > start),
        )
        .await;
        finish(&worker, &fake, "retiring", session).await;
        let request = fake.events("supervisor_request").await.remove(0);
        proxy.cut(0);
        super::support::bounded(5, async {
            while executor.connected() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        executor.close().await.unwrap();
        let execs = fake.events("exec").await;
        let retire = execs
            .iter()
            .find(|row| {
                let wire = STANDARD.decode(row["detail"]["base64"].as_str().unwrap()).unwrap();
                wire.windows(b"/supervisor.pl retire ".len())
                    .any(|bytes| bytes == b"/supervisor.pl retire ")
            })
            .unwrap();
        assert_eq!(retire["connection"], request["connection"]);
        assert_ne!(retire["channel"], request["channel"]);
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_partition_expires_only_the_unreachable_worker_leases_d96() {
    super::support::watchdog(async {
        let worker = Worker::new();
        let fake = ssh::fake_with_handlers(ssh::script(vec![]), 8, |_| {}, vec![worker.clone()]).await;
        let proxy = Proxy::start(fake.port()).await;
        use marathon_apple::cmd::{TransferCleanup, device_commands::DeviceCommands};
        use marathon_core::executor::shared::Signal;
        let lost = Arc::new(executor(proxy.port).await);
        let device = DeviceCommands::new(lost.clone());
        let sibling = executor(fake.port()).await;
        let stranded = open(device.as_ref(), "stranded").await;
        let receipt = device.handed_commands().pop().unwrap();
        let continuing = open(&sibling, "continuing").await;
        let stranded_group = worker.group("stranded");
        let start = stamp(&stranded_group);
        advance(2).await;
        wait_for(
            stranded_group.state.subscribe(),
            |state| matches!(state, State::Running(stamp) if *stamp > start),
        )
        .await;
        proxy.block_new();
        proxy.cut(0);
        proxy.cut(1);
        let active = worker.group("continuing");
        // Keep the sibling's actual controller renewals flowing while time
        // advances past the disconnected group's whole lease.
        for _ in 0..8 {
            let before = stamp(&active);
            advance(6).await;
            wait_for(
                active.state.subscribe(),
                |state| matches!(state, State::Running(stamp) if *stamp > before),
            )
            .await;
        }
        assert_eq!(
            wait_for(worker.group("stranded").state.subscribe(), |state| *state
                == State::Done(137))
            .await,
            State::Done(137)
        );
        assert!(matches!(
            receipt.wait_in(Signal::new()).await.unwrap(),
            TransferCleanup::Unconfirmed(_)
        ));
        assert_eq!(device.handed_left().len(), 1);
        assert!(stranded.await_result(&context()).await.is_err());
        finish(&worker, &fake, "continuing", continuing).await;
        lost.close().await.unwrap();
        sibling.close().await.unwrap();
        drop((lost, sibling));
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}
