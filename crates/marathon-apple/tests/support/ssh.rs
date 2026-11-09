//! Fake-worker plumbing for the SSH executor tests (PLAN §6.4).
use super::{Clock, Runtime};
use fake_worker::{
    Config, RunningWorker,
    script::{Action, Chunk, Matcher, Rule, Script},
    server::AuthPolicy,
};
use marathon_logging::{Level, LogSettings};
use marathon_apple::cmd::remote::ssh::sshj::{
    SshAuthentication, SshCommandExecutor, SshjCommandExecutorFactory, config::SshLogging,
};
use marathon_core::error::Failure;
use russh::keys::{Algorithm, PrivateKey};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

pub struct Fake {
    pub dir: TempDir,
    pub worker: RunningWorker,
    pub host_key: PrivateKey,
}
impl Fake {
    pub fn port(&self) -> u16 {
        self.worker.ready.endpoint.port()
    }
    pub async fn transcript(&self) -> Vec<serde_json::Value> {
        self.worker.transcript.flush().await.unwrap();
        std::fs::read_to_string(self.dir.path().join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    pub async fn events(&self, name: &str) -> Vec<serde_json::Value> {
        self.transcript()
            .await
            .into_iter()
            .filter(|e| e["event"] == name)
            .collect()
    }
    /// `events`, once at least `count` have been recorded or 10 s have passed: the fake writes an event from its
    /// own handler, which a loaded box can run after the client side has already returned.
    pub async fn events_at_least(&self, name: &str, count: usize) -> Vec<serde_json::Value> {
        for _ in 0..1000 {
            let events = self.events(name).await;
            if events.len() >= count {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        self.events(name).await
    }
}
pub async fn fake(script: Script, max_sessions: usize) -> Fake {
    fake_with(script, max_sessions, |_| {}).await
}
pub async fn fake_with(script: Script, max_sessions: usize, adjust: impl FnOnce(&mut Config)) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host = dir.path().join("host");
    std::fs::write(
        &host,
        host_key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let client_key = dir.path().join("client.pub");
    std::fs::write(&client_key, client_key_pair().public_key().to_openssh().unwrap()).unwrap();
    let mut config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        root: dir.path().join("root"),
        transcript: dir.path().join("events.jsonl"),
        host_key: host,
        auth: AuthPolicy {
            user: "fixture".into(),
            password: Some("fake-password".into()),
            authorized_keys: vec![client_key],
            keyboard_interactive: true,
            password_auth: true,
        },
        max_sessions,
        rsync_binary: "/usr/bin/rsync".into(),
        rsync_remote: "/usr/bin/rsync".into(),
        script,
        fixtures: dir.path().into(),
        run: "wp-4.2".into(),
    };
    adjust(&mut config);
    let worker = fake_worker::start(config, vec![]).await.unwrap();
    Fake { dir, worker, host_key }
}
/// A throwaway client key, generated per test process.
pub fn client_key_pair() -> &'static PrivateKey {
    static KEY: std::sync::OnceLock<PrivateKey> = std::sync::OnceLock::new();
    KEY.get_or_init(|| PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap())
}

pub type Events = Arc<Mutex<Vec<(String, Level, String, Option<Failure>)>>>;
/// A registry configured like a run without `--debug`, and every event that reached the sink.
pub fn logging(debug: bool) -> (SshLogging, Events) {
    let mut registry = LogSettings::default();
    registry.debug = debug;
    let events: Events = Arc::default();
    let sink = events.clone();
    let logging = SshLogging::new(
        Arc::new(Mutex::new(registry)),
        Arc::new(move |logger, level, message, throwable| {
            sink.lock()
                .unwrap()
                .push((logger.name().to_owned(), level, message, throwable));
        }),
    );
    (logging, events)
}
pub fn factory(logging: &SshLogging) -> SshjCommandExecutorFactory {
    SshjCommandExecutorFactory::new(logging.clone(), Arc::new(Runtime), Arc::new(Clock::default()))
}
pub fn password() -> SshAuthentication {
    SshAuthentication::Password {
        username: "fixture".into(),
        password: "fake-password".into(),
    }
}
pub async fn connect(fake: &Fake) -> SshCommandExecutor {
    let (logging, _) = logging(false);
    factory(&logging)
        .connect("127.0.0.1", fake.port(), password(), None, true)
        .await
        .unwrap()
}

pub fn exact(wire: &str, action: Action) -> Rule {
    Rule {
        name: wire.into(),
        matcher: Matcher::Exact(wire.into()),
        action,
    }
}
pub fn chunk(data: &str) -> Chunk {
    Chunk {
        data: data.into(),
        ..Default::default()
    }
}
pub fn err_chunk(data: &str) -> Chunk {
    Chunk {
        data: data.into(),
        extended: Some(1),
        ..Default::default()
    }
}
pub fn output(chunks: Vec<Chunk>, status: u32) -> Action {
    Action::Output {
        chunks,
        status,
        signal: None,
        hang: false,
        ignore_signals: vec![],
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: false,
    }
}
pub fn hang(chunks: Vec<Chunk>) -> Action {
    Action::Output {
        chunks,
        status: 0,
        signal: None,
        hang: true,
        ignore_signals: vec![],
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: false,
    }
}
fn seq(n: usize) -> String {
    (1..=n).map(|i| format!("{i}\n")).collect()
}
/// BaseCommandExecutorTest's commands as the worker's shell would answer them (orchestrator decision 5).
pub fn suite_script() -> Script {
    let date_lines: Vec<Chunk> = (0..100)
        .map(|_| Chunk {
            data: "Sat Oct 24 12:00:00 UTC 2026\n".into(),
            delay_ms: 100,
            ..Default::default()
        })
        .collect();
    Script {
        rules: vec![
            exact("echo 'Hello world'", output(vec![chunk("Hello world\n")], 0)),
            exact(
                "bash -c '1>/dev/stderr echo Hello world'",
                output(vec![err_chunk("Hello world\n")], 0),
            ),
            exact("sh -c 'exit 1'", output(vec![], 1)),
            exact("sh -c 'seq 1 100000'", output(vec![chunk(&seq(100_000))], 0)),
            exact(
                "sh -c 'echo start; seq 1 100000 1>&2; echo end'",
                output(vec![chunk("start\n"), err_chunk(&seq(100_000)), chunk("end\n")], 0),
            ),
            exact(
                "sh -c 'echo start; exec seq 1 100000 1>&2'",
                output(vec![chunk("start\n"), err_chunk(&seq(100_000))], 0),
            ),
            exact("sh -c 'sleep 100'", hang(vec![])),
            exact("sh -c 'while [ true ]; do date && sleep 0.1; done'", hang(date_lines)),
            exact("sleep 1000", hang(vec![])),
            // T07: the quoted '&' is an operand of sleep, not shell backgrounding.
            exact(
                "sleep 100000 '&'",
                output(
                    vec![err_chunk(
                        "sleep: invalid time interval ‘&’\nTry 'sleep --help' for more information.\n",
                    )],
                    1,
                ),
            ),
            exact("echo hello", output(vec![chunk("hello\n")], 0)),
        ],
        unmatched_status: 127,
        ..Default::default()
    }
}

/// sshj's 30 s reply timeouts and the keep-alive in compressed time: `delay(ms)` sleeps `ms / factor`. The fake
/// worker's own delays stay real, so withheld CLOSEs make the 30 s and 60 s cases take a fraction of a second.
pub struct ScaledClock {
    pub factor: i64,
    pub inner: Clock,
    /// How much faster than real time `now_millis` runs: `factor`, unless the delays are to lag behind it, as on a host
    /// that doesn't schedule the bridge for a while.
    pub elapsed_factor: i64,
    epoch: std::time::Instant,
}
impl ScaledClock {
    pub fn new(factor: i64) -> Self {
        Self::lagging(factor, factor)
    }
    /// Delays compressed by `factor`, and time passing `elapsed_factor` times faster than real time.
    pub fn lagging(factor: i64, elapsed_factor: i64) -> Self {
        Self {
            factor,
            inner: Clock::default(),
            elapsed_factor,
            epoch: std::time::Instant::now(),
        }
    }
}
impl marathon_core::runtime::VendorClock for ScaledClock {
    fn now_millis(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64 * self.elapsed_factor
    }
    fn delay(
        &self,
        millis: i64,
        cancellation: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        let scaled = millis / self.factor;
        self.inner.delay(scaled, cancellation)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis / self.factor, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        self.inner.with_timeout_or_null(parent, millis / self.factor, body)
    }
}
pub fn scaled_factory(logging: &SshLogging, factor: i64) -> SshjCommandExecutorFactory {
    SshjCommandExecutorFactory::new(logging.clone(), Arc::new(Runtime), Arc::new(ScaledClock::new(factor)))
}
pub async fn connect_with(
    fake: &Fake,
    factory: &SshjCommandExecutorFactory,
    debug: bool,
) -> Result<SshCommandExecutor, marathon_apple::cmd::remote::ssh::sshj::SshFailure> {
    factory.connect("127.0.0.1", fake.port(), password(), None, debug).await
}
pub async fn run(
    executor: &SshCommandExecutor,
    command: &[&str],
    timeout_millis: i64,
) -> marathon_core::error::VendorResult<marathon_apple::cmd::CommandResult> {
    use marathon_apple::cmd::{CommandExecutor, CommandRequest};
    let session = executor
        .execute(
            CommandRequest::new(command.iter().copied(), super::duration(timeout_millis)),
            super::context(),
        )
        .await?;
    let result = session.await_result(&super::context()).await;
    let _ = session.close().await;
    result
}
/// A transcript event's channel number and name, in order, for one connection.
pub fn channel_events(events: &[serde_json::Value], connection: u64) -> Vec<(u64, String)> {
    events
        .iter()
        .filter(|e| e["connection"] == connection && e["channel"].is_u64())
        .map(|e| (e["channel"].as_u64().unwrap(), e["event"].as_str().unwrap().to_owned()))
        .collect()
}
pub fn script(rules: Vec<Rule>) -> Script {
    Script {
        rules,
        unmatched_status: 127,
        ..Default::default()
    }
}
/// The `(logger, level, message)` of each sink event.
pub fn lines(events: &Events) -> Vec<(String, Level, String)> {
    events
        .lock()
        .unwrap()
        .iter()
        .map(|(logger, level, message, _)| (logger.clone(), *level, message.to_owned()))
        .collect()
}

/// `ConfigImpl.maxCircularBufferSize` less one: the most a stream can hold, which D37 keeps a flood at.
pub const UNREAD_CAP: usize = 16 * 1024 * 1024 - 1;
/// Waits until a stream has reached the cap. A test that must exercise D37 calls this: a session that blocks on the
/// initial 2 MiB window behaves the same with or without the fix. The connection dying here is the A32 overflow.
pub async fn wait_for_the_cap(executor: &SshCommandExecutor) {
    use marathon_apple::cmd::CommandExecutor;
    let shared = &executor.connection().shared;
    let reached = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while shared.peak_unread() < UNREAD_CAP {
            assert!(
                executor.connected(),
                "the connection was lost at {} unread bytes",
                shared.peak_unread()
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "stopped at {} unread bytes", shared.peak_unread());
    assert_eq!(shared.peak_unread(), UNREAD_CAP);
    assert!(executor.connected());
}

/// The test clock, except that a command's timeout expires when the test says so, however long the command's own
/// timeout is: a timeout that must fire in a given state cannot race the way to that state on a loaded host.
pub struct GatedTimeoutClock {
    inner: super::Clock,
    expire: Arc<tokio::sync::Notify>,
}
impl GatedTimeoutClock {
    pub fn new(expire: Arc<tokio::sync::Notify>) -> Self {
        Self {
            inner: super::Clock::default(),
            expire,
        }
    }
}
impl marathon_core::runtime::VendorClock for GatedTimeoutClock {
    fn delay(
        &self,
        millis: i64,
        cancellation: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        self.inner.delay(millis, cancellation)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.inner.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        _: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        use marathon_apple::cmd::{SessionScope, cancelled};
        use marathon_core::error::VendorError;
        Box::pin(async move {
            let child = Arc::new(SessionScope::default());
            let mut future = body(child.clone());
            tokio::select! { biased;
                reason = parent.cancelled() => {
                    child.cancel(reason.clone());
                    let _ = future.await;
                    Err(VendorError::Cancelled(reason))
                },
                result = &mut future => result.map(Some),
                _ = self.expire.notified() => {
                    child.cancel(cancelled("timeout"));
                    let _ = future.await;
                    Ok(None)
                },
            }
        })
    }
}
