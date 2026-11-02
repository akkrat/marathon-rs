//! Transport-independent execution of the 0.4b scripted Apple world's plans.
use crate::{
    fs::VirtualFs,
    processes::Processes,
    router::{Plan, Router},
    script::{Action, Chunk},
    transcript,
};
use anyhow::{Result, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, sync::mpsc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub type SinkFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
/// Transport owns delivery and wire-only EOF/CLOSE ordering, never plan semantics.
pub trait Sink: Send + Sync {
    fn event<'a>(&'a self, event: &'a str, detail: Value) -> SinkFuture<'a>;
    fn data(&self, data: Vec<u8>, extended: Option<u32>) -> SinkFuture<'_>;
    fn eof(&self) -> SinkFuture<'_>;
    fn finish(&self, status: u32, signal: Option<String>, eof_first: bool, close_first: bool) -> SinkFuture<'_>;
}
#[derive(Clone)]
pub struct Execution {
    pub router: Router,
    pub fs: VirtualFs,
    pub processes: Processes,
    pub tasks: TaskTracker,
    pub fixtures: PathBuf,
    pub stop: CancellationToken,
    pub transport: CancellationToken,
    pub sink: Arc<dyn Sink>,
}
impl Execution {
    async fn event(&self, event: &str, detail: Value) -> Result<()> {
        self.sink.event(event, detail).await
    }
    pub async fn run(
        &self,
        plan: Plan,
        signals: &mut mpsc::Receiver<String>,
        stdin: &mut mpsc::Receiver<Vec<u8>>,
    ) -> Result<()> {
        match plan {
            Plan::Action(action) => run_action(self, action, signals, stdin).await,
            Plan::Rsync(_) => anyhow::bail!("raw rsync is unavailable in process; use WorldFileBridge"),
            Plan::SignalPid { pid, signal, wait } => {
                let success = tokio::select! {_=self.stop.cancelled()=>return Ok(()),r=self.processes.signal(pid,signal.clone(),wait)=>r?};
                self.event("pid_signal", json!({"pid":pid,"signal":signal,"accepted":success}))
                    .await?;
                finish(self, if success { 0 } else { 1 }, None, false, false).await
            }
            plan @ (Plan::Background { .. } | Plan::BackgroundLog { .. }) => {
                let (request, pidfile) = match plan {
                    Plan::Background { pidfile, request } => (request, Some(pidfile)),
                    Plan::BackgroundLog { request } => (request, None),
                    _ => unreachable!(),
                };
                let routed = tokio::select! {
                    biased;
                    _ = self.stop.cancelled() => return Ok(()),
                    result = self.router.route(&request, &self.fs) => result?,
                };
                ensure!(!routed.unmatched, "unmatched background command");
                let Plan::Action(action) = routed.plan else {
                    anyhow::bail!("background handler must be a scripted action");
                };
                let (pid, signals, done) = self.processes.register().await;
                let registration = self.processes.owned(pid);
                if let Some(pidfile) = &pidfile
                    && let Err(e) = self.fs.write(pidfile, format!("{pid}\n").into_bytes(), 0o600).await
                {
                    self.processes.completed(pid).await;
                    return Err(e);
                }
                self.event(
                    "background",
                    json!({"pid":pid,"pidfile":pidfile,"exec":transcript::bytes(&request.raw),"handler":routed.name}),
                )
                .await?;
                let bg = self.clone();
                self.tasks.spawn(async move {
                    let _registration = registration;
                    let result = background(&bg, pid, action, signals).await;
                    bg.processes.completed(pid).await;
                    let _ = done.send(true);
                    let _ = bg
                        .event(
                            "background_end",
                            json!({"pid":pid,"error":result.err().map(|e|e.to_string())}),
                        )
                        .await;
                });
                if pidfile.is_none() {
                    send_chunk(self, format!("{pid}\n").into_bytes(), None).await?;
                }
                finish(self, 0, None, false, false).await
            }
        }
    }
}
async fn send_chunk(ctx: &Execution, data: Vec<u8>, extended: Option<u32>) -> Result<()> {
    for part in data.chunks(32768) {
        tokio::select! {_=ctx.stop.cancelled()=>return Ok(()),r=ctx.sink.data(part.to_vec(),extended)=>r?};
        let mut detail = transcript::bytes(part);
        detail["extended"] = json!(extended);
        ctx.event("output", detail).await?;
    }
    Ok(())
}
async fn finish(
    ctx: &Execution,
    status: u32,
    signal: Option<String>,
    eof_first: bool,
    close_first: bool,
) -> Result<()> {
    ctx.sink.finish(status, signal, eof_first, close_first).await
}
async fn background(ctx: &Execution, pid: u64, action: Action, mut signals: mpsc::Receiver<String>) -> Result<()> {
    let (action, files, started, on_signal) = match action {
        Action::Publish {
            output,
            files,
            started,
            on_signal,
        } => (*output, files, started, on_signal),
        action => (action, vec![], vec![], None),
    };
    tokio::select! {biased; _=ctx.transport.cancelled()=>return Ok(()),result=publish_files(&ctx.fs, started)=>result?};
    let Action::Output {
        chunks,
        hang,
        ignore_signals,
        ..
    } = action
    else {
        anyhow::bail!("background requires Output action")
    };
    let work = async {
        for chunk in chunks {
            tokio::time::sleep(Duration::from_millis(chunk.delay_ms)).await;
            ctx.event(
                "background_redirect",
                json!({"pid":pid,"destination":"/dev/null","chunk":chunk}),
            )
            .await?;
        }
        if hang {
            std::future::pending::<()>().await;
        }
        Ok::<_, anyhow::Error>(())
    };
    let ended = {
        tokio::pin!(work);
        loop {
            tokio::select! {
                _=ctx.transport.cancelled()=>return Ok(()),
                result=&mut work=>{result?;break None;},
                Some(signal)=signals.recv()=>{
                    ctx.event("background_signal",json!({"pid":pid,"signal":signal,"ignored":ignore_signals.contains(&signal)})).await?;
                    if !ignore_signals.contains(&signal){break Some(signal);}
                }
            }
        }
    };
    if ended == on_signal {
        tokio::select! {biased; _=ctx.transport.cancelled()=>return Ok(()),result=publish_files(&ctx.fs, files)=>result?};
    }
    Ok(())
}
async fn publish_files(fs: &VirtualFs, files: Vec<crate::script::PublishedFile>) -> Result<()> {
    for file in files {
        fs.write(&file.path, BASE64_STANDARD.decode(file.base64)?, 0o600)
            .await?;
    }
    Ok(())
}
async fn chunks(ctx: &Execution, chunks: Vec<Chunk>) -> Result<()> {
    for chunk in chunks {
        if let Some(path) = &chunk.wait_for_file {
            ctx.fs.wait_for_file(path).await?;
        }
        tokio::time::sleep(Duration::from_millis(chunk.delay_ms)).await;
        if chunk.eof_before {
            ctx.sink.eof().await?;
            ctx.event("server_eof", json!({"trigger":"chunk_fault"})).await?;
        }
        if let Some(path) = chunk.fixture {
            ensure!(
                !std::path::Path::new(&path).is_absolute()
                    && !std::path::Path::new(&path)
                        .components()
                        .any(|p| matches!(p, std::path::Component::ParentDir)),
                "fixture traversal"
            );
            let root = tokio::fs::canonicalize(&ctx.fixtures).await?;
            let p = tokio::fs::canonicalize(root.join(path)).await?;
            ensure!(p.starts_with(root), "fixture symlink escape");
            let mut file = tokio::fs::File::open(p).await?;
            let mut buf = vec![0; 32768];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                send_chunk(ctx, buf[..n].to_vec(), chunk.extended).await?;
            }
        } else {
            send_chunk(
                ctx,
                if let Some(b) = chunk.base64 {
                    BASE64_STANDARD.decode(b)?
                } else {
                    chunk.data.into_bytes()
                },
                chunk.extended,
            )
            .await?;
        }
    }
    Ok(())
}
async fn run_action(
    ctx: &Execution,
    action: Action,
    signals: &mut mpsc::Receiver<String>,
    stdin: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    let (action, completed_files, started, on_signal) = match action {
        Action::Publish {
            output,
            files,
            started,
            on_signal,
        } => (*output, files, started, on_signal),
        action => (action, vec![], vec![], None),
    };
    tokio::select! {biased; _=ctx.stop.cancelled()=>return Ok(()),result=publish_files(&ctx.fs, started)=>result?};
    match action {
        Action::Output {
            chunks: output,
            status,
            signal,
            hang,
            ignore_signals,
            eof_before_exit,
            close_before_eof,
            ..
        } => {
            let mut signal = signal;
            {
                let work = async {
                    chunks(ctx, output).await?;
                    if hang {
                        std::future::pending::<()>().await;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                tokio::pin!(work);
                loop {
                    tokio::select! {
                        // A queued signal precedes CLOSE or work completion,
                        // even when this actor resumes after both became ready.
                        biased;
                        Some(s)=signals.recv()=>{let name=s;if !ignore_signals.contains(&name){signal=Some(name);break;}ctx.event("signal_ignored",json!({"signal":name})).await?;},
                        _=ctx.stop.cancelled()=>return Ok(()),
                        result=&mut work=>{result?;break;},
                    }
                }
            }
            if signal == on_signal {
                tokio::select! {biased; _=ctx.stop.cancelled()=>return Ok(()),result=publish_files(&ctx.fs, completed_files)=>result?};
            }
            finish(ctx, status, signal, eof_before_exit, close_before_eof).await
        }
        Action::EchoStdin => {
            loop {
                tokio::select! {_=ctx.stop.cancelled()=>return Ok(()),data=stdin.recv()=>match data{Some(data)=>send_chunk(ctx,data,None).await?,None=>break},Some(signal)=signals.recv()=>return finish(ctx,0,Some(signal),false,false).await}
            }
            finish(ctx, 0, None, false, false).await
        }
        _ => anyhow::bail!("action cannot execute here"),
    }
}
