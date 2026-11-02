//! Loopback transport, bounded channel actors and run-owned child processes.
use crate::{
    fs::VirtualFs,
    router::{Plan, Request, Router},
    script::{Action, Disconnect, Keepalive, Script},
    transcript::{self, Transcript},
};
use anyhow::{Result, ensure};
use russh::{
    Channel, ChannelId, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig,
    keys::{self, PublicKey},
    server::{Auth, ChannelOpenHandle, Msg, Response, Session},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthPolicy {
    pub user: String,
    pub password: Option<String>,
    #[serde(default)]
    pub authorized_keys: Vec<PathBuf>,
    #[serde(default = "yes")]
    pub keyboard_interactive: bool,
    #[serde(default = "yes")]
    pub password_auth: bool,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub root: PathBuf,
    pub transcript: PathBuf,
    pub host_key: PathBuf,
    pub auth: AuthPolicy,
    pub max_sessions: usize,
    pub rsync_binary: PathBuf,
    #[serde(default = "remote_rsync")]
    pub rsync_remote: String,
    #[serde(default)]
    pub script: Script,
    #[serde(default = "fixture_dir")]
    pub fixtures: PathBuf,
    #[serde(default = "run_id")]
    pub run: String,
}
fn remote_rsync() -> String {
    "/usr/bin/rsync".into()
}
fn fixture_dir() -> PathBuf {
    PathBuf::from("fixtures/fake-worker")
}
fn run_id() -> String {
    "fake-worker".into()
}
#[derive(Serialize)]
pub struct Readiness {
    pub schema: u32,
    pub endpoint: SocketAddr,
    pub host_key_fingerprint: String,
    pub run: String,
}
pub struct RunningWorker {
    pub ready: Readiness,
    pub fs: VirtualFs,
    pub transcript: Transcript,
    stop: CancellationToken,
    task: Option<JoinHandle<Result<()>>>,
}
impl Drop for RunningWorker {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl RunningWorker {
    pub async fn shutdown(mut self) -> Result<()> {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Shared {
    config: Config,
    router: Router,
    fs: VirtualFs,
    log: Transcript,
    keys: Arc<Vec<PublicKey>>,
    tasks: TaskTracker,
    processes: crate::processes::Processes,
}
pub async fn start(config: Config, extensions: Vec<Arc<dyn crate::router::Handler>>) -> Result<RunningWorker> {
    ensure!(config.listen.ip().is_loopback(), "worker listener must be loopback");
    ensure!(config.max_sessions > 0, "max_sessions must be positive");
    ensure!(
        config.rsync_binary.is_absolute(),
        "rsync_binary must be explicit absolute path"
    );
    let auth_paths = config.auth.authorized_keys.clone();
    let host_path = config.host_key.clone();
    let (host_key, authorized) = tokio::task::spawn_blocking(move || -> Result<_> {
        let host = keys::load_secret_key(host_path, None)?;
        let mut authorized = vec![];
        for path in auth_paths {
            for line in std::fs::read_to_string(path)?
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            {
                authorized.push(PublicKey::from_openssh(line)?);
            }
        }
        Ok((host, authorized))
    })
    .await??;
    let fingerprint = host_key.public_key().fingerprint(keys::HashAlg::Sha256).to_string();
    let log = Transcript::create(&config.transcript, config.run.clone()).await?;
    let fs = VirtualFs::create(config.root.clone()).await?;
    let mut ssh = russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::from_millis(20),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(300)),
        channel_buffer_size: 16,
        event_buffer_size: 32,
        window_size: 262144,
        ..Default::default()
    };
    if config.script.faults.no_common_kex {
        ssh.preferred.kex = std::borrow::Cow::Owned(vec![]);
    }
    let ssh = Arc::new(ssh);
    let listener = TcpListener::bind(config.listen).await?;
    let endpoint = listener.local_addr()?;
    let ready = Readiness {
        schema: 1,
        endpoint,
        host_key_fingerprint: fingerprint,
        run: config.run.clone(),
    };
    log.event(0, None, "ready", serde_json::to_value(&ready)?).await?;
    let shared = Shared {
        router: Router {
            script: config.script.clone(),
            rsync_remote: config.rsync_remote.clone(),
            rsync_binary: config.rsync_binary.clone(),
            extensions,
        },
        config,
        fs: fs.clone(),
        log: log.clone(),
        keys: Arc::new(authorized),
        tasks: TaskTracker::new(),
        processes: crate::processes::Processes::default(),
    };
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let mut next = 1;
        let permits = Arc::new(Semaphore::new(64));
        let mut outcome: Result<()> = loop {
            tokio::select! {
                _=stopping.cancelled()=>break Ok(()),
                Some(result)=connections.join_next(),if !connections.is_empty()=>{
                    if let Err(e)=result.map_err(anyhow::Error::from).and_then(|r|r){break Err(e);}
                },
                accepted=listener.accept()=>{
                    let (stream,_)=match accepted {Ok(pair)=>pair,Err(e)=>break Err(e.into())};
                    let Ok(permit)=permits.clone().try_acquire_owned() else {drop(stream);continue};
                    let shared=shared.clone();let ssh=ssh.clone();let stop=stopping.child_token();let id=next;next+=1;
                    connections.spawn(async move{let _permit=permit;connection(shared,ssh,stream,id,stop).await});
                }
            }
        };
        stopping.cancel();
        drop(listener);
        while let Some(result) = connections.join_next().await {
            if let Err(e) = result.map_err(anyhow::Error::from).and_then(|r| r) {
                outcome = Err(e);
            }
        }
        shared.tasks.close();
        shared.tasks.wait().await;
        let active = shared.processes.active().await;
        if !active.is_empty() {
            outcome = Err(anyhow::anyhow!("virtual processes survived shutdown: {active:?}"));
        }
        let snapshot = shared.fs.snapshot().await?;
        shared
            .log
            .event(
                0,
                None,
                "cleanup",
                json!({"active_processes":active,"outcome":if outcome.is_ok(){"completed"}else{"failed"},"files":snapshot.0.into_iter().map(|(e,_)|e).collect::<Vec<_>>()}),
            )
            .await?;
        shared.log.flush().await?;
        outcome
    });
    Ok(RunningWorker {
        ready,
        fs,
        transcript: log,
        stop,
        task: Some(task),
    })
}
async fn connection(
    shared: Shared,
    ssh: Arc<russh::server::Config>,
    mut stream: TcpStream,
    id: u64,
    stop: CancellationToken,
) -> Result<()> {
    shared.log.event(id, None, "connection", json!({})).await?;
    if let Some(d) = &shared.config.script.faults.before_kex {
        shared
            .log
            .event(
                id,
                None,
                "disconnect",
                json!({"stage":"before_kex","reason":d.reason,"message":d.message}),
            )
            .await?;
        // RFC 4253 clear packet before KEX. No cryptographic state exists yet.
        let mut payload = vec![1];
        payload.extend(d.reason.to_be_bytes());
        for s in [&d.message, ""] {
            payload.extend((s.len() as u32).to_be_bytes());
            payload.extend(s.as_bytes());
        }
        let padding = 8 - ((payload.len() + 5) % 8) + 8;
        let mut packet = vec![];
        packet.extend(((payload.len() + padding + 1) as u32).to_be_bytes());
        packet.push(padding as u8);
        packet.extend(payload);
        packet.extend(vec![0; padding]);
        tokio::time::timeout(Duration::from_secs(2), async {
            stream.write_all(b"SSH-2.0-fake-worker\r\n").await?;
            for i in 0..4096 {
                let byte = stream.read_u8().await?;
                if byte == b'\n' {
                    break;
                }
                if i == 4095 {
                    return Err(std::io::Error::other("oversized client banner"));
                }
            }
            stream.write_all(&packet).await?;
            stream.shutdown().await?;
            // Drain the peer's KEX bytes until it observes the disconnect and
            // closes. Closing with unread TCP data can replace the wire reason
            // with a reset/Broken pipe on the client (U8 would go untested).
            let mut buf = [0; 4096];
            while stream.read(&mut buf).await? != 0 {}
            Ok::<_, std::io::Error>(())
        })
        .await??;
        return Ok(());
    }
    let lifetime = stop.child_token();
    let handler = Connection {
        shared: shared.clone(),
        id,
        stop: lifetime.clone(),
        sessions: HashMap::new(),
        slots: HashMap::new(),
        channel_stops: HashMap::new(),
        limit: Arc::new(Semaphore::new(shared.config.max_sessions)),
        withheld: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
    };
    let running = tokio::select! {_ =stop.cancelled()=>return Ok(()),r=tokio::time::timeout(Duration::from_secs(10),russh::server::run_stream(ssh,stream,handler))=>r};
    let result = match running {
        Ok(Ok(running)) => {
            let mut owned = OwnedSession(running);
            tokio::select! {
                r=&mut owned.0=>r,
                _=lifetime.cancelled()=>{owned.0.abort();let _=(&mut owned.0).await;Ok(())},
            }
        }
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.into()),
    };
    lifetime.cancel();
    shared
        .log
        .event(
            id,
            None,
            "connection_end",
            json!({"outcome":if result.is_ok(){"completed"}else{"failed"},"error":result.err().map(|e|e.to_string())}),
        )
        .await
}
struct OwnedSession(russh::server::RunningSession<Connection>);
impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Open {
    channel: Channel<Msg>,
    permit: OwnedSemaphorePermit,
}
struct Connection {
    shared: Shared,
    id: u64,
    stop: CancellationToken,
    sessions: HashMap<ChannelId, Open>,
    channel_stops: HashMap<ChannelId, CancellationToken>,
    slots: HashMap<ChannelId, std::sync::Weak<tokio::sync::Mutex<Option<OwnedSemaphorePermit>>>>,
    limit: Arc<Semaphore>,
    withheld: Arc<tokio::sync::Mutex<HashSet<ChannelId>>>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Connection {
    async fn event(&self, ch: Option<ChannelId>, event: &str, detail: Value) -> Result<()> {
        self.shared.log.event(self.id, ch.map(u32::from), event, detail).await
    }
    async fn auth(&self, user: &str, method: &str, accepted: bool) -> Result<Auth> {
        self.event(None, "auth", json!({"user":user,"method":method,"accepted":accepted}))
            .await?;
        Ok(if accepted { Auth::Accept } else { Auth::reject() })
    }
    async fn reject(&mut self, ch: ChannelId, kind: &str, session: &mut Session) -> Result<()> {
        self.event(Some(ch), "request", json!({"kind":kind,"accepted":false}))
            .await?;
        session.channel_failure(ch)?;
        // russh also queues request messages on Channel before the callback.
        // Until exec starts an actor, consume these rejected requests ourselves.
        if let Some(open) = self.sessions.get_mut(&ch) {
            while let Ok(Some(_)) = tokio::time::timeout(Duration::ZERO, open.channel.wait()).await {}
        }
        Ok(())
    }
    async fn disconnect(&self, d: &Disconnect, stage: &str, session: &mut Session) -> Result<()> {
        self.event(
            None,
            "disconnect",
            json!({"stage":stage,"reason":d.reason,"message":d.message}),
        )
        .await?;
        session.disconnect(russh::Disconnect::try_from(d.reason)?, &d.message, "")?;
        Ok(())
    }
}
impl russh::server::Handler for Connection {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, user: &str) -> Result<Auth> {
        self.auth(user, "none", false).await
    }
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
        let p = &self.shared.config.auth;
        self.auth(
            user,
            "password",
            p.password_auth && p.user == user && p.password.as_deref() == Some(password),
        )
        .await
    }
    async fn auth_publickey_offered(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        self.auth(
            user,
            "publickey_offer",
            self.shared.config.auth.user == user && self.shared.keys.iter().any(|k| k.key_data() == key.key_data()),
        )
        .await
    }
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        self.auth(
            user,
            "publickey",
            self.shared.config.auth.user == user && self.shared.keys.iter().any(|k| k.key_data() == key.key_data()),
        )
        .await
    }
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _sub: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth> {
        if !self.shared.config.auth.keyboard_interactive {
            return self.auth(user, "keyboard-interactive", false).await;
        }
        if let Some(mut response) = response {
            let accepted = self.shared.config.auth.user == user
                && response.next().is_some_and(|r| {
                    self.shared
                        .config
                        .auth
                        .password
                        .as_ref()
                        .is_some_and(|p| r.as_ref() == p.as_bytes())
                });
            self.auth(user, "keyboard-interactive", accepted).await
        } else {
            self.event(None, "auth_challenge", json!({"user":user,"prompt":"Password:"}))
                .await?;
            Ok(Auth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: vec![("Password: ".into(), false)].into(),
            })
        }
    }
    async fn auth_succeeded(&mut self, s: &mut Session) -> Result<()> {
        if let Some(d) = &self.shared.config.script.faults.after_auth {
            self.disconnect(d, "after_auth", s).await?;
        }
        Ok(())
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _s: &mut Session,
    ) -> Result<()> {
        let id = channel.id();
        self.slots.retain(|_, slot| slot.strong_count() > 0);
        self.channel_stops.retain(|id, _| self.slots.contains_key(id));
        match self.limit.clone().try_acquire_owned() {
            Ok(permit) => {
                self.event(Some(id), "session_open", json!({})).await?;
                self.sessions.insert(id, Open { channel, permit });
                reply.accept().await;
            }
            Err(_) => {
                self.event(
                    Some(id),
                    "session_rejected",
                    json!({"reason":"ResourceShortage","limit":self.shared.config.max_sessions}),
                )
                .await?;
                reply.reject(russh::ChannelOpenFailure::ResourceShortage).await;
            }
        }
        Ok(())
    }
    async fn exec_request(&mut self, ch: ChannelId, data: &[u8], s: &mut Session) -> Result<()> {
        self.event(Some(ch), "exec", transcript::bytes(data)).await?;
        if self.shared.config.script.faults.reject_exec {
            return self.reject(ch, "exec", s).await;
        }
        if let Some(d) = &self.shared.config.script.faults.during_exec {
            return self.disconnect(d, "during_exec", s).await;
        }
        let Some(open) = self.sessions.remove(&ch) else {
            return self.reject(ch, "duplicate_exec", s).await;
        };
        let raw = data.to_vec();
        let slot = Arc::new(tokio::sync::Mutex::new(Some(open.permit)));
        self.slots.insert(ch, Arc::downgrade(&slot));
        let ctx = Context {
            shared: self.shared.clone(),
            slot,
            connection: self.id,
            channel: ch,
            stop: self.stop.child_token(),
            transport: self.stop.clone(),
            handle: s.handle(),
            withhold: self.shared.config.script.faults.withhold_close,
        };
        self.channel_stops.insert(ch, ctx.stop.clone());
        let withheld = self.withheld.clone();
        self.shared.tasks.spawn(async move {
            let result = dispatch(ctx.clone(), open.channel, raw, withheld).await;
            if let Err(e) = result {
                let _ = ctx.event("channel_error", json!({"error":e.to_string()})).await;
                ctx.transport.cancel();
            }
        });
        Ok(())
    }
    async fn reply_to_channel_close(&mut self, ch: ChannelId) -> bool {
        !self.shared.config.script.faults.withhold_close && !self.withheld.lock().await.contains(&ch)
    }
    async fn channel_close(&mut self, ch: ChannelId, _s: &mut Session) -> Result<()> {
        self.sessions.remove(&ch);
        if let Some(stop) = self.channel_stops.remove(&ch) {
            stop.cancel();
        }
        if let Some(slot) = self.slots.remove(&ch).and_then(|slot| slot.upgrade()) {
            slot.lock().await.take();
        }
        let withheld = self.withheld.lock().await.remove(&ch) || self.shared.config.script.faults.withhold_close;
        self.event(Some(ch), "client_close", json!({"reply_withheld":withheld}))
            .await
    }
    async fn channel_eof(&mut self, ch: ChannelId, _s: &mut Session) -> Result<()> {
        self.event(Some(ch), "client_eof", json!({})).await
    }
    async fn signal(&mut self, ch: ChannelId, signal: Sig, _s: &mut Session) -> Result<()> {
        self.event(Some(ch), "signal", json!({"signal":signal_name(&signal)}))
            .await
    }
    async fn unknown_global_request(&mut self, name: &str) -> Option<Duration> {
        let keepalive = name.starts_with("keepalive");
        let mode = if keepalive {
            &self.shared.config.script.faults.keepalive
        } else {
            &Keepalive::Answer
        };
        if self
            .event(
                None,
                if keepalive { "keepalive" } else { "global_request" },
                json!({"name":name,"reply":mode}),
            )
            .await
            .is_err()
        {
            self.stop.cancel();
        }
        match mode {
            Keepalive::Answer => Some(Duration::ZERO),
            Keepalive::Delay(ms) => Some(Duration::from_millis(*ms)),
            Keepalive::Withhold => None,
        }
    }
    async fn env_request(&mut self, ch: ChannelId, _name: &str, _value: &str, s: &mut Session) -> Result<()> {
        self.reject(ch, "env", s).await
    }
    async fn pty_request(
        &mut self,
        ch: ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        s: &mut Session,
    ) -> Result<()> {
        self.reject(ch, "pty", s).await
    }
    async fn shell_request(&mut self, ch: ChannelId, s: &mut Session) -> Result<()> {
        self.reject(ch, "shell", s).await
    }
    async fn subsystem_request(&mut self, ch: ChannelId, _name: &str, s: &mut Session) -> Result<()> {
        self.reject(ch, "subsystem", s).await
    }
    async fn tcpip_forward(&mut self, _address: &str, _port: &mut u32, _s: &mut Session) -> Result<bool> {
        self.event(None, "forwarding", json!({"accepted":false})).await?;
        Ok(false)
    }
}
#[derive(Clone)]
struct Context {
    shared: Shared,
    slot: Arc<tokio::sync::Mutex<Option<OwnedSemaphorePermit>>>,
    connection: u64,
    channel: ChannelId,
    stop: CancellationToken,
    transport: CancellationToken,
    handle: russh::server::Handle,
    withhold: bool,
}
impl Context {
    async fn event(&self, event: &str, detail: Value) -> Result<()> {
        self.shared
            .log
            .event(self.connection, Some(self.channel.into()), event, detail)
            .await
    }
}
fn signal_name(s: &Sig) -> String {
    match s {
        Sig::Custom(s) => s.clone(),
        s => format!("{s:?}"),
    }
}
async fn dispatch(
    mut ctx: Context,
    channel: Channel<Msg>,
    raw: Vec<u8>,
    withheld: Arc<tokio::sync::Mutex<HashSet<ChannelId>>>,
) -> Result<()> {
    let routed = tokio::select! {
        _ = ctx.stop.cancelled() => return Ok(()),
        result = async {
            let request = Request::parse(&raw)?;
            ctx.shared.router.route(&request, &ctx.shared.fs).await
        } => result,
    };
    let plan = match routed {
        Ok(r) => {
            ctx.event(
                "route",
                json!({"handler":r.name,"warning":r.unmatched.then_some("unmatched command")}),
            )
            .await?;
            if r.name == "builtin" && matches!(r.plan, Plan::Action(_)) {
                ctx.event(
                    "filesystem",
                    json!({"files":ctx.shared.fs.snapshot().await?.0.into_iter().map(|(e,_)|e).collect::<Vec<_>>()}),
                )
                .await?;
            }
            r.plan
        }
        Err(e) => {
            ctx.event("route_error", json!({"error":e.to_string()})).await?;
            Plan::Action(Action::output("", 2))
        }
    };
    if matches!(plan, Plan::Action(Action::Reject)) {
        ctx.handle
            .channel_failure(ctx.channel)
            .await
            .map_err(|_| anyhow::anyhow!("exec failure send"))?;
        ctx.event("request", json!({"kind":"exec","accepted":false})).await?;
        return Ok(());
    }
    if let Plan::Action(Action::Disconnect { reason, message }) = plan {
        ctx.event(
            "disconnect",
            json!({"stage":"during_exec","reason":reason,"message":message}),
        )
        .await?;
        ctx.handle
            .disconnect(russh::Disconnect::try_from(reason)?, message, String::new())
            .await?;
        return Ok(());
    }
    if matches!(plan, Plan::Action(Action::DropTransport)) {
        ctx.event("transport_drop", json!({})).await?;
        ctx.transport.cancel();
        return Ok(());
    }
    ctx.withhold |= matches!(
        &plan,
        Plan::Action(Action::Output {
            withhold_close: true,
            ..
        })
    );
    if ctx.withhold {
        withheld.lock().await.insert(ctx.channel);
    }
    ctx.handle
        .channel_success(ctx.channel)
        .await
        .map_err(|_| anyhow::anyhow!("exec success send"))?;
    run_channel(ctx, channel, plan).await
}
async fn run_channel(ctx: Context, channel: Channel<Msg>, plan: Plan) -> Result<()> {
    let (mut input, output) = channel.split();
    let output = Arc::new(output);
    let (signals, mut signal_rx) = mpsc::channel(16);
    let (stdin, mut stdin_rx) = mpsc::channel(16);
    let read_ctx = ctx.clone();
    let reader = Box::pin(async move {
        loop {
            tokio::select! {
                biased;
                msg=input.wait()=>match msg {
                    Some(ChannelMsg::Data{data})=>{read_ctx.event("stdin",transcript::bytes(&data)).await?;if stdin.send(data.to_vec()).await.is_err(){return Ok(());}},
                    Some(ChannelMsg::Eof)=>{drop(stdin);return drain_control(input,signals,read_ctx.stop).await;},
                    Some(ChannelMsg::Signal{signal})=>{if signals.send(signal_name(&signal)).await.is_err(){return Ok(());}},
                    Some(ChannelMsg::Close)|None=>{read_ctx.stop.cancel();return Ok(());},
                    _=>(),
                },
                _=read_ctx.stop.cancelled()=>return Ok::<_,anyhow::Error>(()),
            }
        }
    });
    let execute = async {
        match plan {
            Plan::Rsync(args) => run_rsync(&ctx, &output, args, &mut signal_rx, &mut stdin_rx).await,
            plan => {
                let execution = crate::execution::Execution {
                    router: ctx.shared.router.clone(),
                    fs: ctx.shared.fs.clone(),
                    processes: ctx.shared.processes.clone(),
                    tasks: ctx.shared.tasks.clone(),
                    fixtures: ctx.shared.config.fixtures.clone(),
                    stop: ctx.stop.clone(),
                    transport: ctx.transport.clone(),
                    sink: Arc::new(SshSink {
                        ctx: ctx.clone(),
                        output: output.clone(),
                    }),
                };
                execution.run(plan, &mut signal_rx, &mut stdin_rx).await
            }
        }
    };
    let mut reader = reader;
    tokio::pin!(execute);
    let result = tokio::select! {
        // CLOSE also cancels the channel from the connection handler. Forward
        // earlier wire signals before the action can observe that cancellation.
        biased;
        result=&mut reader=>{drop(reader);result?;execute.await},
        result=&mut execute=>{drop(reader);result},
    };
    ctx.stop.cancel();
    ctx.event(
        "channel_end",
        json!({"outcome":if result.is_ok(){"completed"}else{"failed"}}),
    )
    .await?;
    result
}
async fn drain_control(
    mut input: ChannelReadHalf,
    signals: mpsc::Sender<String>,
    stop: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {biased; msg=input.wait()=>match msg{
            Some(ChannelMsg::Signal{signal})=>{if signals.send(signal_name(&signal)).await.is_err(){return Ok(());}},
            Some(ChannelMsg::Close)|None=>{stop.cancel();return Ok(());},_=>(),
        },_=stop.cancelled()=>return Ok(())}
    }
}
async fn send_chunk(ctx: &Context, out: &ChannelWriteHalf<Msg>, data: Vec<u8>, extended: Option<u32>) -> Result<()> {
    for part in data.chunks(32768) {
        tokio::select! {_=ctx.stop.cancelled()=>return Ok(()),r=async{if let Some(ext)=extended{out.extended_data_bytes(ext,part.to_vec()).await}else{out.data_bytes(part.to_vec()).await}}=>r?};
        let mut detail = transcript::bytes(part);
        detail["extended"] = json!(extended);
        ctx.event("output", detail).await?;
    }
    Ok(())
}
async fn finish(
    ctx: &Context,
    out: &ChannelWriteHalf<Msg>,
    status: u32,
    signal: Option<String>,
    eof_first: bool,
    close_first: bool,
) -> Result<()> {
    // Completion becomes observable on the wire below. Release first so the
    // client's immediate next open cannot race the actor's final transcript.
    ctx.slot.lock().await.take();
    if eof_first {
        out.eof().await?;
        ctx.event("server_eof", json!({})).await?;
    }
    if let Some(signal) = signal {
        ctx.handle
            .exit_signal_request(
                ctx.channel,
                Sig::Custom(signal.clone()),
                false,
                String::new(),
                String::new(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("exit signal send"))?;
        ctx.event("exit_signal", json!({"signal":signal})).await?;
    } else {
        out.exit_status(status).await?;
        ctx.event("exit_status", json!({"status":status})).await?;
    }
    if close_first && !ctx.withhold {
        out.close().await?;
        ctx.event("server_close", json!({})).await?;
    }
    if !eof_first {
        out.eof().await?;
        ctx.event("server_eof", json!({})).await?;
    }
    if !close_first && !ctx.withhold {
        out.close().await?;
        ctx.event("server_close", json!({})).await?;
    }
    if ctx.withhold {
        ctx.event("close_withheld", json!({"trigger":"command_completion"}))
            .await?;
    }
    Ok(())
}
async fn pipe_output<R: AsyncRead + Unpin>(
    ctx: &Context,
    out: &ChannelWriteHalf<Msg>,
    mut reader: R,
    ext: Option<u32>,
) -> Result<()> {
    let mut buf = vec![0; 32768];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        send_chunk(ctx, out, buf[..n].to_vec(), ext).await?;
    }
}
async fn run_rsync(
    ctx: &Context,
    out: &ChannelWriteHalf<Msg>,
    mut args: Vec<String>,
    signals: &mut mpsc::Receiver<String>,
    stdin: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    let lease = tokio::select! {_=ctx.stop.cancelled()=>return Ok(()),l=ctx.shared.fs.lease()=>l};
    let remote = args.last().ok_or_else(|| anyhow::anyhow!("missing operand"))?.clone();
    // This resolver runs in the blocking pool, and the lease remains held until
    // the child is killed/reaped and both output streams have drained.
    let (lease, path) = tokio::task::spawn_blocking(move || -> Result<_> {
        let path = lease.resolve(&remote)?;
        Ok((lease, path))
    })
    .await??;
    let original = args.clone();
    let last = args.len() - 1;
    let trailing = args[last].ends_with('/');
    args[last] = format!("{}{}", path.display(), if trailing { "/" } else { "" });
    let mut child = tokio::process::Command::new(&ctx.shared.router.rsync_binary)
        .args(&args)
        .current_dir(&lease.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    ctx.event(
        "child_start",
        json!({"program":"rsync","argv":original,"pid":child.id()}),
    )
    .await?;
    let mut child_in = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no child stdin"))?;
    let child_out = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no child stdout"))?;
    let child_err = child.stderr.take().ok_or_else(|| anyhow::anyhow!("no child stderr"))?;
    let status = {
        let write = async {
            while let Some(bytes) = stdin.recv().await {
                child_in.write_all(&bytes).await?;
            }
            child_in.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let output = async {
            tokio::try_join!(
                pipe_output(ctx, out, child_out, None),
                pipe_output(ctx, out, child_err, Some(1))
            )?;
            Ok::<_, anyhow::Error>(())
        };
        tokio::pin!(write);
        tokio::pin!(output);
        let mut write_done = false;
        let mut output_done = false;
        let status = loop {
            tokio::select! {
                _=ctx.stop.cancelled()=>{child.kill().await?;break child.wait().await?;},
                result=&mut write,if !write_done=>{if let Err(e)=result{ctx.event("stdin_closed",json!({"error":e.to_string()})).await?;}write_done=true;},
                result=&mut output,if !output_done=>{if let Err(e)=result{let _=child.kill().await;let _=child.wait().await;return Err(e);}output_done=true;},
                Some(signal)=signals.recv()=>{if let Some(pid)=child.id(){let name=signal;if let Ok(signal)=format!("SIG{name}").parse::<nix::sys::signal::Signal>(){nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32),signal)?;}}},
                result=child.wait()=>break result?,
            }
        };
        if !output_done && !ctx.stop.is_cancelled() {
            tokio::select! {_=ctx.stop.cancelled()=>(),r=&mut output=>r?};
        }
        status
    };
    ctx.event("child_reaped", json!({"code":status.code()})).await?;
    drop(lease);
    ctx.event(
        "filesystem",
        json!({"files":ctx.shared.fs.snapshot().await?.0.into_iter().map(|(e,_)|e).collect::<Vec<_>>()}),
    )
    .await?;
    if ctx.stop.is_cancelled() {
        return Ok(());
    }
    use std::os::unix::process::ExitStatusExt;
    finish(
        ctx,
        out,
        status.code().unwrap_or(1) as u32,
        status.signal().map(|s| {
            if s == 15 {
                "TERM".into()
            } else if s == 9 {
                "KILL".into()
            } else {
                s.to_string()
            }
        }),
        false,
        false,
    )
    .await
}

struct SshSink {
    ctx: Context,
    output: Arc<ChannelWriteHalf<Msg>>,
}
impl crate::execution::Sink for SshSink {
    fn event<'a>(&'a self, event: &'a str, detail: Value) -> crate::execution::SinkFuture<'a> {
        Box::pin(self.ctx.event(event, detail))
    }
    fn data(&self, data: Vec<u8>, extended: Option<u32>) -> crate::execution::SinkFuture<'_> {
        Box::pin(async move {
            if let Some(ext) = extended {
                self.output.extended_data_bytes(ext, data).await?;
            } else {
                self.output.data_bytes(data).await?;
            }
            Ok(())
        })
    }
    fn eof(&self) -> crate::execution::SinkFuture<'_> {
        Box::pin(async {
            self.output.eof().await?;
            Ok(())
        })
    }
    fn finish(
        &self,
        status: u32,
        signal: Option<String>,
        eof_first: bool,
        close_first: bool,
    ) -> crate::execution::SinkFuture<'_> {
        Box::pin(finish(&self.ctx, &self.output, status, signal, eof_first, close_first))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, prelude::BASE64_STANDARD};
    use russh::{
        client,
        keys::{Algorithm, PrivateKey, PublicKeyOrCertificate},
    };
    use tokio::time::timeout;

    struct Client;
    impl client::Handler for Client {
        type Error = russh::Error;
        async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn term_drops_interrupted_output_before_finish_with_transcript_locked() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let host = dir.path().join("host");
        tokio::fs::write(&host, key.to_openssh(keys::ssh_key::LineEnding::LF)?.as_bytes()).await?;
        let path = dir.path().join("events.jsonl");
        let config = Config {
            listen: "127.0.0.1:0".parse()?,
            root: dir.path().join("root"),
            transcript: path.clone(),
            host_key: host,
            auth: AuthPolicy {
                user: "fixture".into(),
                password: Some("fake-password".into()),
                authorized_keys: vec![],
                keyboard_interactive: true,
                password_auth: true,
            },
            max_sessions: 1,
            rsync_binary: "/usr/bin/rsync".into(),
            rsync_remote: "/usr/bin/rsync".into(),
            script: serde_json::from_value(json!({"rules":[{
                "name":"held", "matcher":{"exact":"held"},
                "action":{"type":"output", "chunks":[{"data":"out\n"}], "hang":true}
            }]}))?,
            fixtures: dir.path().into(),
            run: "term-interleaving".into(),
        };
        let w = start(config, vec![]).await?;
        let mut c = client::connect(Arc::new(client::Config::default()), w.ready.endpoint, Client).await?;
        ensure!(c.authenticate_password("fixture", "fake-password").await?.success());
        let pause = w.transcript.pause_append("output", false);
        let mut ch = c.channel_open_session().await?;
        ch.exec(true, "held").await?;
        timeout(Duration::from_secs(2), pause.entered.notified()).await?;
        ch.signal(Sig::TERM).await?;
        // The writer holds its mutex halfway through the output row. TERM must
        // destroy the suspended output future before finish tries to record exit.
        timeout(Duration::from_secs(2), pause.dropped.notified()).await?;
        pause.release.send(())?;
        let (mut signal, mut eof, mut close) = (false, false, false);
        timeout(Duration::from_secs(2), async {
            while let Some(message) = ch.wait().await {
                match message {
                    ChannelMsg::ExitSignal {
                        signal_name: Sig::TERM, ..
                    } => signal = true,
                    ChannelMsg::Eof => eof = true,
                    ChannelMsg::Close => {
                        close = true;
                        break;
                    }
                    _ => (),
                }
            }
        })
        .await?;
        assert!(signal && eof && close);
        let mut reused = c.channel_open_session().await?;
        reused.exec(true, "test -e /tmp/marathon").await?;
        timeout(Duration::from_secs(2), async {
            while let Some(message) = reused.wait().await {
                if matches!(message, ChannelMsg::Close) {
                    break;
                }
            }
        })
        .await?;
        c.disconnect(russh::Disconnect::ByApplication, "test", "").await?;
        timeout(Duration::from_secs(2), w.shutdown()).await??;
        let text = tokio::fs::read_to_string(path).await?;
        let rows: Vec<Value> = text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row["seq"], i as u64);
        }
        let output = rows
            .iter()
            .position(|r| r["event"] == "output")
            .ok_or_else(|| anyhow::anyhow!("missing output"))?;
        let exit = rows
            .iter()
            .position(|r| r["event"] == "exit_signal")
            .ok_or_else(|| anyhow::anyhow!("missing exit signal"))?;
        assert!(output < exit);
        assert_eq!(rows[output]["detail"]["base64"], BASE64_STANDARD.encode(b"out\n"));
        assert_eq!(rows.last().map(|r| &r["event"]), Some(&json!("cleanup")));
        Ok(())
    }
}
