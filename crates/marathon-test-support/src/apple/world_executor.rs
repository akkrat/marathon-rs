//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/device/FakeSimulatorWorker.kt
//! Adapts the 0.4b fake_worker::apple::World through its shared router/plan runner.
use super::fake_simulator_worker::{BeforeHook, io_exception};
use fake_worker::{
    execution::{Execution, Sink, SinkFuture},
    fs::VirtualFs,
    processes::Processes,
    router::{Request, Router},
};
use jcompat::logging::{Logger, MarathonLogging};
use marathon_apple::cmd::{
    BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandSession, FileBridge, SessionControl,
    line_buffer::LineBuffer,
    remote::ssh::sshj::{DisconnectReason, error::SshFailure, executor::shell_command},
};
use marathon_core::{
    error::VendorError,
    runtime::{VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::mpsc;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Backend {
    router: Router,
    fs: VirtualFs,
    fixtures: PathBuf,
    processes: Processes,
}
struct Connection {
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
pub struct WorldExecutor {
    runtime: Arc<dyn VendorRuntime>,
    backend: Arc<Backend>,
    connection: Arc<Connection>,
    host: CommandHost,
    logger: Logger,
    pub connected: AtomicBool,
    pub before: Mutex<BeforeHook>,
    pub commands: Mutex<Vec<Vec<String>>>,
}
impl WorldExecutor {
    pub fn new(
        runtime: Arc<dyn VendorRuntime>,
        logging: &mut MarathonLogging,
        router: Router,
        fs: VirtualFs,
        fixtures: PathBuf,
    ) -> Self {
        Self {
            runtime,
            backend: Arc::new(Backend {
                router,
                fs,
                fixtures,
                processes: Processes::default(),
            }),
            connection: Arc::new(Connection {
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
            }),
            host: CommandHost {
                id: "local".into(),
                charset: "UTF-8".into(),
            },
            logger: logging.logger("marathon.test.WorldExecutor", None),
            connected: AtomicBool::new(true),
            before: Mutex::new(Arc::new(|_| Box::pin(async { Ok(None) }))),
            commands: Mutex::default(),
        }
    }
    /// A distinct connection to the same world, filesystem and virtual PID registry.
    pub fn new_connection(&self, host_id: &str) -> Self {
        Self {
            runtime: self.runtime.clone(),
            backend: self.backend.clone(),
            connection: Arc::new(Connection {
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
            }),
            host: CommandHost {
                id: host_id.into(),
                charset: "UTF-8".into(),
            },
            logger: self.logger.clone(),
            connected: AtomicBool::new(true),
            before: Mutex::new(Arc::new(|_| Box::pin(async { Ok(None) }))),
            commands: Mutex::default(),
        }
    }
    pub fn fs(&self) -> &VirtualFs {
        &self.backend.fs
    }
    pub fn file_bridge(&self) -> WorldFileBridge {
        WorldFileBridge {
            fs: self.backend.fs.clone(),
        }
    }
    pub fn processes(&self) -> &Processes {
        &self.backend.processes
    }
}
struct StopOnDrop(CancellationToken);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct WorldSink {
    stdout: LineBuffer,
    stderr: LineBuffer,
    exit: marathon_apple::cmd::ExitCode,
}
impl WorldSink {
    fn new(base: &BaseCommand) -> Self {
        let out = base.stdout.clone();
        let err = base.stderr.clone();
        Self {
            stdout: LineBuffer::new(move |line| {
                let out = out.clone();
                Box::pin(async move { out.send(line).await })
            }),
            stderr: LineBuffer::new(move |line| {
                let err = err.clone();
                Box::pin(async move { err.send(line).await })
            }),
            exit: base.exit_code.clone(),
        }
    }
}
fn harness_error(e: impl std::fmt::Display) -> VendorError {
    VendorError::Other(io_exception(&e.to_string()))
}
impl Sink for WorldSink {
    fn event<'a>(&'a self, _: &'a str, _: serde_json::Value) -> SinkFuture<'a> {
        Box::pin(async { Ok(()) })
    }
    fn data(&self, data: Vec<u8>, extended: Option<u32>) -> SinkFuture<'_> {
        Box::pin(async move {
            let buffer = if extended == Some(1) {
                &self.stderr
            } else {
                &self.stdout
            };
            buffer.append(&data);
            buffer.flush().await.map_err(|e| anyhow::anyhow!("{e:?}"))
        })
    }
    fn eof(&self) -> SinkFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn finish(&self, status: u32, signal: Option<String>, _: bool, _: bool) -> SinkFuture<'_> {
        Box::pin(async move {
            self.stdout.drain().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            self.stderr.drain().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            // Match sshj: TERM is 128+15; every other signal has no exit code (D7).
            self.exit.complete(Ok(match signal.as_deref() {
                None => Some(status as i32),
                Some("TERM") => Some(143),
                Some(_) => None,
            }));
            Ok(())
        })
    }
}
impl CommandExecutor for WorldExecutor {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst) && !self.connection.stop.is_cancelled()
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            self.commands
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request.command.clone());
            let before = self.before.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(result) = before(request.command.clone()).await? {
                return Ok(CommandSession::finished(self.runtime.clone(), result));
            }
            if self.connection.stop.is_cancelled() {
                return Err(SshFailure::not_connected().into_vendor());
            }
            if !self.connected() {
                let lost = SshFailure::transport(DisconnectReason::Unknown, "Broken transport; encountered EOF");
                return Err(SshFailure::connection_gone(&lost).into_vendor());
            }
            let raw = shell_command(&request.command);
            let parsed = Request::parse(raw.as_bytes()).map_err(harness_error)?;
            if parsed
                .argv
                .first()
                .is_some_and(|s| s == "rsync" || s == &self.backend.router.rsync_remote)
            {
                return Err(harness_error(
                    "raw rsync is unavailable in process; use WorldFileBridge",
                ));
            }
            let routed = tokio::select! {biased;
                reason=context.cancellation.cancelled()=>return Err(VendorError::Cancelled(reason)),
                result=self.backend.router.route(&parsed,&self.backend.fs)=>result.map_err(harness_error)?,
            };
            let base = BaseCommand::new(64, self.runtime.clone());
            let sink = Arc::new(WorldSink::new(&base));
            let stop = self.connection.stop.child_token();
            let guard = Arc::new(StopOnDrop(stop.clone()));
            let (signals, mut signal_rx) = mpsc::channel(16);
            let (stdin, mut stdin_rx) = mpsc::channel(1);
            drop(stdin);
            let execution = Execution {
                router: self.backend.router.clone(),
                fs: self.backend.fs.clone(),
                processes: self.backend.processes.clone(),
                tasks: self.connection.tasks.clone(),
                fixtures: self.backend.fixtures.clone(),
                stop: stop.clone(),
                transport: self.connection.stop.clone(),
                sink,
            };
            let stdout = base.stdout.clone();
            let stderr = base.stderr.clone();
            let exit = base.exit_code.clone();
            let alive = Arc::new(AtomicBool::new(true));
            let running = alive.clone();
            let worker_stop = stop.clone();
            // A tracked wrapper lets connection close join foreground and background jobs alike.
            let task = self
                .runtime
                .spawn(Box::pin(self.connection.tasks.track_future(async move {
                    let result = tokio::select! {biased;
                        reason=context.cancellation.cancelled()=>Err(VendorError::Cancelled(reason)),
                        _=worker_stop.cancelled()=>Ok(()),
                        result=execution.run(routed.plan,&mut signal_rx,&mut stdin_rx)=>result.map_err(harness_error),
                    };
                    stdout.close(None);
                    stderr.close(None);
                    exit.complete(result.clone().map(|_| None));
                    running.store(false, Ordering::SeqCst);
                    result
                })));
            let term = signals.clone();
            let kill = signals;
            Ok(CommandSession::new(
                base,
                SessionControl {
                    is_alive: Box::new(move || alive.load(Ordering::SeqCst)),
                    terminate: Box::new(move || term.try_send("TERM".into()).map_err(harness_error)),
                    kill: Box::new(move || kill.try_send("KILL".into()).map_err(harness_error)),
                    close: Box::new(move || {
                        let guard = guard.clone();
                        let task = task.clone();
                        Box::pin(async move {
                            guard.0.cancel();
                            task.join().await
                        })
                    }),
                },
            ))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.connected.store(false, Ordering::SeqCst);
            self.connection.stop.cancel();
            self.connection.tasks.close();
            self.connection.tasks.wait().await;
            Ok(())
        })
    }
}

/// Direct local/virtual copy; no rsync executable or wire protocol is involved.
#[derive(Clone)]
pub struct WorldFileBridge {
    pub fs: VirtualFs,
}
impl FileBridge for WorldFileBridge {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.fs
                .import(src.to_owned(), dst)
                .await
                .map(|_| true)
                .map_err(harness_error)
        })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.fs
                .export(src, dst.to_owned())
                .await
                .map(|_| true)
                .map_err(harness_error)
        })
    }
}
