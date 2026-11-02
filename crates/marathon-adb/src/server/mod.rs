//! A scripted ADB server that plays fake Android devices (PLAN.md §6.4, `docs/design/fake-adb-script.md`).
//!
//! It replaces adam's `server-stub` in our tests and drives the Kotlin marathon through `adbServers`.
//! Unlike the stub, which replays one expected request after another, it keeps device state: a device
//! list with `host:track-devices`, an in-memory filesystem behind sync, reverse forwards, and shell
//! expectations matched against each command.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use marathon_adb::server::{FakeAdbServer, Script, ServerOptions};
//! let script = Script::from_yaml("devices: [{ serial: emulator-5554 }]")?.compile(std::path::Path::new("."))?;
//! let server = FakeAdbServer::start(script, ServerOptions::default()).await?;
//! println!("listening on {}", server.port());
//! # Ok(()) }
//! ```

mod device;
pub mod fs;
mod host;
mod protocol;
pub mod script;
mod sync;
mod transcript;
mod world;

#[cfg(test)]
mod tests;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

pub use fs::Node;
pub use script::{CompiledDevice, CompiledScript, EventAction, Script, ScriptError};
pub use transcript::Transcript;

use protocol::read_request;
use world::{KillRequest, Selector, World};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub host: IpAddr,
    /// 0 picks a free port; [`FakeAdbServer::port`] tells which.
    pub port: u16,
    /// Where to write the JSONL transcript, in addition to keeping it in memory.
    pub transcript: Option<PathBuf>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            transcript: None,
        }
    }
}

/// A running server. Dropping it stops the server and closes every connection.
pub struct FakeAdbServer {
    world: Arc<World>,
    addr: SocketAddr,
    stopped: watch::Receiver<bool>,
}

impl FakeAdbServer {
    /// Binds the port and starts serving on the current tokio runtime.
    pub async fn start(script: CompiledScript, options: ServerOptions) -> io::Result<FakeAdbServer> {
        let transcript = match &options.transcript {
            Some(p) => Transcript::with_file(p)?,
            None => Transcript::default(),
        };
        let (kill_tx, kill_rx) = mpsc::unbounded_channel();
        let world = Arc::new(World::new(&script, transcript, kill_tx));
        let listener = TcpListener::bind((options.host, options.port)).await?;
        let addr = listener.local_addr()?;
        let (stopped_tx, stopped) = watch::channel(false);
        let timeline =
            (!script.events.is_empty()).then(|| tokio::spawn(timeline(world.clone(), script.events.clone())));
        // The supervisor is not aborted on drop: it must see the Exit request, stop the timeline and close
        // the connections.
        tokio::spawn(supervise(world.clone(), listener, addr, kill_rx, stopped_tx, timeline));
        Ok(FakeAdbServer { world, addr, stopped })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every transcript record so far.
    pub fn transcript(&self) -> Vec<serde_json::Value> {
        self.world.transcript.records()
    }

    /// The number of `warning` records so far.
    pub fn warnings(&self) -> usize {
        self.world.transcript.warnings()
    }

    /// Complete device filesystem bytes and state. Capture after `shutdown` to
    /// exclude mutations by still-running connection or timeline tasks.
    pub fn snapshot(&self) -> serde_json::Value {
        self.world.snapshot()
    }

    /// Record successful shell output writes for whole-CLI evidence. Disabled
    /// for existing request-only corpora to preserve their transcript schema.
    pub fn capture_output(&self) {
        self.world.capture_output.store(true, Ordering::Relaxed);
    }

    /// Waits until the transcript file holds every record so far; fails if writing it failed.
    pub async fn flush_transcript(&self) -> io::Result<()> {
        self.world.transcript.flush().await
    }

    /// Open `host:track-devices` sessions.
    #[cfg(test)]
    pub(crate) fn track_sessions(&self) -> usize {
        self.world.devices_changed.receiver_count()
    }

    pub fn add_device(&self, device: &CompiledDevice) -> Result<(), String> {
        self.world.add_device(device)
    }

    pub fn connect_device(&self, serial: &str) -> Result<(), String> {
        self.world.connect(serial)
    }

    /// Removes the device from the list and ends its open sessions.
    pub fn disconnect_device(&self, serial: &str) -> Result<(), String> {
        self.world.disconnect(serial)
    }

    pub fn set_device_state(&self, serial: &str, state: &str) -> Result<(), String> {
        self.world.set_state(serial, state)
    }

    /// A file or directory on a device's filesystem.
    pub fn file(&self, serial: &str, path: &str) -> Option<Node> {
        self.world.with_device(serial, |d| d.fs.get(path).cloned()).flatten()
    }

    /// Reverse forwards of a device, local spec → remote spec.
    pub fn reverse_forwards(&self, serial: &str) -> Vec<(String, String)> {
        self.world
            .with_device(serial, |d| d.reverse.clone())
            .unwrap_or_default()
    }

    /// Like a timeline `kill_server` event.
    pub fn kill(&self, restart_after: Option<Duration>) {
        let _ = self.world.kill.send(KillRequest::Down { restart_after });
    }

    /// Resolves once the server has stopped for good (after `host:kill` with `on_kill: exit`, or
    /// [`FakeAdbServer::shutdown`]).
    pub async fn stopped(&self) {
        let mut rx = self.stopped.clone();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Stops the server for good and completes the transcript: the timeline stops, every connection
    /// closes and has finished recording, `server_stopped` is recorded last, and the transcript file is
    /// drained and closed. Reports a transcript write error. Records made later (through this handle)
    /// stay in memory only.
    pub async fn shutdown(&mut self) -> io::Result<()> {
        // Fails only if the supervisor has already stopped, which is what we wait for anyway.
        let _ = self.world.kill.send(KillRequest::Exit);
        self.stopped().await;
        let transcript = self.world.transcript.clone();
        tokio::task::spawn_blocking(move || transcript.close())
            .await
            .map_err(io::Error::other)?
    }
}

impl Drop for FakeAdbServer {
    fn drop(&mut self) {
        let _ = self.world.kill.send(KillRequest::Exit);
    }
}

/// Accepts connections; on a kill drops them all, then listens again or stops.
async fn supervise(
    world: Arc<World>,
    mut listener: TcpListener,
    addr: SocketAddr,
    mut kill_rx: mpsc::UnboundedReceiver<KillRequest>,
    stopped: watch::Sender<bool>,
    timeline: Option<JoinHandle<()>>,
) {
    let conn_ids = Arc::new(AtomicU64::new(1));
    // Connections watch this generation and close when it moves.
    let (generation_tx, _) = watch::channel(0u64);
    // Kept so that a kill can wait until every connection has stopped recording.
    let mut connections = JoinSet::new();
    loop {
        let kill = loop {
            tokio::select! {
                k = kill_rx.recv() => break k,
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        world.transcript.warning(format!("connection task failed: {error}"));
                    }
                }
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let conn = conn_ids.fetch_add(1, Ordering::Relaxed);
                    let mut generation = generation_tx.subscribe();
                    let world = world.clone();
                    connections.spawn(async move {
                        tokio::select! {
                            r = connection(world.clone(), stream, conn) => {
                                match r {
                                    Ok(()) => {}
                                    // The client stopped listening to a command's output, as marathon does
                                    // when it stops a logcat stream or abandons a batch: not a server problem.
                                    // Every other unfinished exchange (an upload, a sync transfer) warns.
                                    Err(e) if device::is_output_cancelled(&e) => world.transcript.record(json!({
                                        "type": "event",
                                        "event": "client_closed",
                                        "conn": conn,
                                        "error": e.to_string(),
                                    })),
                                    Err(e) => world.transcript.warning(format!("connection {conn}: {e}")),
                                }
                            }
                            _ = generation.changed() => {}
                        }
                    });
                }
            }
        };
        generation_tx.send_modify(|g| *g += 1);
        drop(listener);
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                world
                    .transcript
                    .warning(format!("connection task failed during shutdown: {error}"));
            }
        }
        // A closed channel means the handle is gone: stop for good.
        let restart = match kill.unwrap_or(KillRequest::Exit) {
            KillRequest::Exit => None,
            KillRequest::Down { restart_after } => Some(restart_after),
        };
        if let Some(restart_after) = restart {
            world.transcript.record(json!({
                "type": "event",
                "event": "server_killed",
                "restart_after_ms": restart_after.map(|d| d.as_millis() as u64),
            }));
        }
        // While down, only an Exit (or the handle going away) ends the wait early.
        let wait_for_exit = async {
            loop {
                match kill_rx.recv().await {
                    None | Some(KillRequest::Exit) => return,
                    Some(KillRequest::Down { .. }) => {}
                }
            }
        };
        match restart {
            None => {}
            Some(None) => wait_for_exit.await,
            Some(Some(delay)) => {
                let exited = tokio::select! {
                    _ = wait_for_exit => true,
                    _ = tokio::time::sleep(delay) => false,
                };
                if !exited {
                    match TcpListener::bind(addr).await {
                        Ok(l) => {
                            listener = l;
                            world
                                .transcript
                                .record(json!({"type": "event", "event": "server_restarted"}));
                            continue;
                        }
                        Err(e) => world.transcript.warning(format!("cannot listen again on {addr}: {e}")),
                    }
                }
            }
        }
        // The final record: the connections have stopped, and so has the timeline once it is aborted and
        // awaited here, so nothing is recorded after it.
        if let Some(t) = timeline {
            t.abort();
            if let Err(error) = t.await
                && !error.is_cancelled()
            {
                world
                    .transcript
                    .warning(format!("timeline task failed during shutdown: {error}"));
            }
        }
        world
            .transcript
            .record(json!({"type": "event", "event": "server_stopped"}));
        let _ = stopped.send(true);
        return;
    }
}

async fn timeline(world: Arc<World>, mut events: Vec<script::EventSpec>) {
    events.sort_by_key(|e| e.at_ms);
    let start = tokio::time::Instant::now();
    for e in events {
        tokio::time::sleep_until(start + Duration::from_millis(e.at_ms)).await;
        let r = match &e.action {
            EventAction::Connect(s) => world.connect(s),
            EventAction::Disconnect(s) => world.disconnect(s),
            EventAction::State { serial, state } => world.set_state(serial, state),
            EventAction::KillServer { restart_after_ms } => {
                let restart_after = restart_after_ms.map(Duration::from_millis);
                world
                    .kill
                    .send(KillRequest::Down { restart_after })
                    .map_err(|_| "server already stopped".to_string())
            }
        };
        if let Err(m) = r {
            world.transcript.warning(format!("event at {} ms: {m}", e.at_ms));
        }
    }
}

async fn connection(world: Arc<World>, stream: TcpStream, conn: u64) -> io::Result<()> {
    let _ = stream.set_nodelay(true);
    let (mut rd, mut wr) = stream.into_split();
    let mut transport = None;
    loop {
        let Some(req) = read_request(&mut rd).await? else {
            return Ok(());
        };
        let serial = transport.as_ref().map(|t: &world::Transport| t.serial.clone());
        world
            .transcript
            .record(json!({"type": "request", "conn": conn, "serial": serial, "service": req}));
        if transport.is_none() && host::is_host_service(&req) {
            match host::handle(&world, &req, &mut wr).await? {
                host::HostOutcome::Done => return device::close(rd, wr).await,
                host::HostOutcome::Transport(t) => transport = Some(t),
                host::HostOutcome::Track { long } => return host::track(&world, long, &mut rd, &mut wr).await,
            }
            continue;
        }
        // A device service without a transport switch goes to the only device, as with transport-any.
        let t = match transport.take() {
            Some(t) => t,
            None => match world.resolve(&Selector::Any, true) {
                Ok(t) => t,
                Err(m) => {
                    protocol::write_fail(&mut wr, &m).await?;
                    return device::close(rd, wr).await;
                }
            },
        };
        return device::handle(world, t, req, rd, wr, conn).await;
    }
}
