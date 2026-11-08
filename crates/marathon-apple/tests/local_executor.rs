//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/local/KotlinProcessCommandExecutorTest.kt
mod support;
use support::{executor, executor_suite};
#[tokio::test(flavor = "multi_thread")]
async fn test_stdout() {
    support::watchdog(async {
        executor_suite::test_stdout(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_stderr() {
    support::watchdog(async {
        executor_suite::test_stderr(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_exit_code() {
    support::watchdog(async {
        executor_suite::test_exit_code(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_buffering() {
    support::watchdog(async {
        executor_suite::test_buffering(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_consume_output_drains_streams_concurrently() {
    support::watchdog(async {
        executor_suite::test_consume_output_drains_streams_concurrently(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_consume_output_failure_does_not_block_session_close() {
    support::watchdog(async {
        executor_suite::test_consume_output_failure_does_not_block_session_close(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_timeout() {
    support::watchdog(async {
        executor_suite::test_timeout(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_terminate() {
    support::watchdog(async {
        executor_suite::test_terminate(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_terminate_should_not_close_executor() {
    support::watchdog(async {
        executor_suite::test_terminate_should_not_close_executor(executor()).await;
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn test_interrupt_using_kill_should_not_close_executor() {
    support::watchdog(async {
        executor_suite::test_interrupt_using_kill_should_not_close_executor(executor()).await;
    })
    .await;
}

use marathon_apple::cmd::{CommandExecutor, CommandRequest, cancelled};
use marathon_core::{
    error::{Cancellation, VendorError},
    vendor::VendorContext,
};
use std::sync::Arc;
use support::{context, duration};

// Ours: the Kotlin test with "Kill" in its name never invokes kill().
#[tokio::test(flavor = "multi_thread")]
async fn actual_kill_returns_137_and_executor_remains_connected() {
    support::watchdog(async {
        let executor = executor();
        let session = executor
            .execute(CommandRequest::new(["sleep", "100"], duration(5000)), context())
            .await
            .unwrap();
        session.kill().unwrap();
        assert_eq!(session.await_result(&context()).await.unwrap().exit_code, Some(137));
        session.close().await.unwrap();
        assert!(executor.connected());
        assert!(
            executor
                .critical_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
                .await
                .unwrap()
                .successful()
        );
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn total_timeout_leaves_process_alive_until_explicit_close() {
    support::watchdog(async {
        let executor = executor();
        let session = executor
            .execute(CommandRequest::new(["sleep", "100"], duration(10)), context())
            .await
            .unwrap();
        assert_eq!(session.await_result(&context()).await.unwrap().exit_code, None);
        assert!(session.is_alive());
        session.close().await.unwrap();
        // Polls the real child's exit; the deadline only catches a hang on a slow runner.
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while session.is_alive() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn cancelled_awaiter_keeps_collectors_running_and_process_alive() {
    support::watchdog(async {
        let executor = executor();
        let root = tempfile::tempdir().unwrap();
        let completed = root.path().join("completed");
        let script = format!(
            "sleep 0.1; seq 1 10000; echo done > '{}'; exec sleep 100",
            completed.display()
        );
        let session = Arc::new(
            executor
                .execute(CommandRequest::new(["sh", "-c", &script], duration(5000)), context())
                .await
                .unwrap(),
        );
        let signal = Arc::new(marathon_apple::cmd::SessionScope::default());
        let ctx = VendorContext {
            cancellation: signal.clone(),
            ..context()
        };
        let waiting = session.clone();
        let task = tokio::spawn(async move { waiting.await_result(&ctx).await });
        tokio::task::yield_now().await;
        signal.cancel(cancelled("awaiter stopped"));
        assert_eq!(
            task.await.unwrap(),
            Err(VendorError::Cancelled(cancelled("awaiter stopped")))
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while !completed.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(session.is_alive());
        session.close().await.unwrap();
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn real_idle_timeout_is_cancellation_with_original_timeout_as_cause() {
    support::watchdog(async {
    let executor = executor();
    let mut request = CommandRequest::new(["sleep", "100"], duration(5000));
    request.idle_timeout = duration(20);
    let session = executor.execute(request, context()).await.unwrap();
    let result = session.await_result(&context()).await;
    session.close().await.unwrap();
    assert!(
        matches!(result, Err(VendorError::Cancelled(Cancellation::Requested(f))) if f.cause.as_ref().is_some_and(|c| c.class == "java.util.concurrent.TimeoutException"))
    );
    }).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn argv_env_workdir_and_quote_layers_are_observed_by_a_controlled_binary() {
    support::watchdog(async {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        let binary = canonical_root.join("record-command");
        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s\\n' \"$MARATHON_LOCAL_VALUE\" \"$PWD\" \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut request = CommandRequest::new(
            [binary.to_str().unwrap(), "\"'x'\"", "'", "\"", "''", "x'", ""],
            duration(5000),
        );
        request.env.insert("PATH".into(), root.path().to_str().unwrap().into());
        request.env.insert("MARATHON_LOCAL_VALUE".into(), "value".into());
        request.workdir = Some(root.path().to_str().unwrap().into());
        let executor = executor();
        // On Linux another test thread's fork can briefly hold the just-written script open
        // for writing (until its exec closes the inherited descriptor), and exec then fails
        // with ETXTBSY. Retry the launch; the script content is the same.
        let mut attempt = 0;
        let session = loop {
            match executor.execute(request.clone(), context()).await {
                Err(marathon_core::error::VendorError::Other(f))
                    if attempt < 20
                        && f.message
                            .as_ref()
                            .is_some_and(|m| m.to_owned().contains("Text file busy")) =>
                {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                other => break other.unwrap(),
            }
        };
        let result = session.await_result(&context()).await.unwrap();
        session.close().await.unwrap();
        assert_eq!(
            result.stdout,
            ["value", canonical_root.to_str().unwrap(), "x", "'", "\"", "", "x'", ""]
        );
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn execute_start_errors_propagate_and_signed_timeouts_remain_signed() {
    support::watchdog(async {
    let executor = executor();
    assert!(
        executor
            .safe_execute(
                CommandRequest::new(["/marathon-rs-nonexistent-binary"], duration(5000)),
                context()
            )
            .await
            .is_err()
    );
    let mut request = CommandRequest::new(["echo", "hello"], duration(5000));
    request.workdir = Some("/marathon-rs-nonexistent-workdir".into());
    assert!(executor.execute(request, context()).await.is_err());
    for timeout in [duration(-1), duration(0)] {
        let session = executor
            .execute(CommandRequest::new(["sleep", "100"], timeout), context())
            .await
            .unwrap();
        assert_eq!(session.await_result(&context()).await.unwrap().exit_code, None);
        session.close().await.unwrap();
    }
    let mut request = CommandRequest::new(["sleep", "100"], jcompat::jtime::Duration::of_seconds(i64::MAX));
    request.idle_timeout = duration(5000);
    let session = executor.execute(request, context()).await.unwrap();
    assert!(
        matches!(session.await_result(&context()).await, Err(VendorError::Other(f)) if f.class == "java.lang.ArithmeticException")
    );
    session.close().await.unwrap();
    }).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn executable_lookup_uses_parent_path_while_child_receives_the_override() {
    support::watchdog(async {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let name = "marathon-rs-wp4.1-path-probe";
        let binary = root.path().join(name);
        std::fs::write(&binary, "#!/bin/sh\necho found\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let executor = executor();
        let mut request = CommandRequest::new([name], duration(5000));
        request.env.insert("PATH".into(), root.path().to_str().unwrap().into());
        assert!(executor.execute(request, context()).await.is_err());
        let mut request = CommandRequest::new(["echo", "parent lookup"], duration(5000));
        request.env.insert("PATH".into(), root.path().to_str().unwrap().into());
        assert_eq!(
            executor.critical_execute(request, context()).await.unwrap().stdout,
            ["parent lookup"]
        );
    })
    .await;
}

// Saved TailProbe witnesses, using real pipes and the frozen local canRead predicate.
#[tokio::test(flavor = "multi_thread")]
async fn idle_timeout_is_the_first_receive_after_partial_output_or_live_pipe_eof() {
    support::watchdog(async {
        let executor = executor();
        for script in ["printf partial; exec sleep 5", "exec 1>&- 2>&-; exec sleep 5"] {
            let mut request = CommandRequest::new(["sh", "-c", script], duration(5000));
            request.idle_timeout = duration(20);
            let session = executor.execute(request, context()).await.unwrap();
            let first = session.base.stdout.receive().await;
            let alive = session.is_alive();
            session.close().await.unwrap();
            assert_eq!(
                first,
                Err(VendorError::Other(marathon_apple::cmd::failure(
                    "java.util.concurrent.TimeoutException",
                    "idle timeout PT0.02S reached",
                ))),
                "{script}"
            );
            assert!(alive, "{script}");
        }
    })
    .await;
}

// The review's GateRuntime witness, with an explicit release instead of a sleep.
struct SpawnGateRuntime {
    started: Arc<tokio::sync::Notify>,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    delivery_polled: Arc<tokio::sync::Notify>,
    spawn_count: Arc<std::sync::atomic::AtomicUsize>,
    tasks: std::sync::Mutex<Vec<Arc<dyn marathon_core::runtime::VendorTask>>>,
}
impl marathon_core::runtime::VendorRuntime for SpawnGateRuntime {
    fn spawn_cancellable(
        &self,
        body: marathon_core::runtime::VendorTaskBody,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        support::Runtime.spawn_cancellable(body)
    }
    fn spawn(
        &self,
        body: marathon_core::runtime::VendorFuture<'static, ()>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        use std::{sync::atomic::Ordering, task::Poll};
        let first = self.spawn_count.fetch_add(1, Ordering::SeqCst) == 0;
        let count = self.spawn_count.clone();
        let delivered = self.delivery_polled.clone();
        let mut body = body;
        let task = support::Runtime.spawn(Box::pin(std::future::poll_fn(move |cx| {
            let result = body.as_mut().poll(cx);
            // The launch task next suspends after creating reaper, exit and two
            // readers: its successful delivery is then queued but unreceived.
            if first && count.load(Ordering::SeqCst) == 5 && matches!(result, Poll::Pending) {
                delivered.notify_one();
            }
            result
        })));
        self.tasks.lock().unwrap().push(task.clone());
        task
    }
    fn spawn_blocking(
        &self,
        body: Box<dyn FnOnce() -> marathon_core::error::VendorResult<()> + Send>,
    ) -> Arc<dyn marathon_core::runtime::VendorTask> {
        let started = self.started.clone();
        let gate = self.release.lock().unwrap().take().unwrap();
        support::Runtime.spawn_blocking(Box::new(move || {
            started.notify_one();
            gate.recv().unwrap();
            body()
        }))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_execute_during_spawn_or_delivery_closes_and_joins_the_undeliverable_session() {
    support::watchdog(async {
        for drop_after_delivery in [false, true] {
            let started = Arc::new(tokio::sync::Notify::new());
            let (release, gate) = std::sync::mpsc::channel();
            let runtime = Arc::new(SpawnGateRuntime {
                started: started.clone(),
                release: std::sync::Mutex::new(Some(gate)),
                tasks: std::sync::Mutex::new(Vec::new()),
                delivery_polled: Arc::new(tokio::sync::Notify::new()),
                spawn_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            });
            let executor = marathon_apple::cmd::local::KotlinProcessCommandExecutor::new(
                runtime.clone(),
                Arc::new(std::sync::Mutex::new(jcompat::logging::MarathonLogging::default())),
                |_, _| {},
                false,
                64,
            );
            let root = tempfile::tempdir().unwrap();
            let pidfile = root.path().join("pid");
            let script = format!("echo $$ > {}; exec sleep 120", pidfile.display());
            let mut caller = executor.execute(CommandRequest::new(["sh", "-c", &script], duration(50)), context());
            std::future::poll_fn(|cx| {
                assert!(caller.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            started.notified().await;
            if drop_after_delivery {
                release.send(()).unwrap();
                runtime.delivery_polled.notified().await;
                drop(caller);
            } else {
                drop(caller);
                release.send(()).unwrap();
            }
            // The launch task must join cleanup, and the independent reaper must
            // finish well before the orphan witness's two-minute sleep ends. Both rounds fit in
            // the 60 s watchdog, so a hang still reaches the kill below.
            let closed = tokio::time::timeout(std::time::Duration::from_secs(25), async {
                let launch = runtime.tasks.lock().unwrap()[0].clone();
                launch.join().await.unwrap();
                let tasks = runtime.tasks.lock().unwrap().clone();
                for task in tasks {
                    task.join().await.unwrap();
                }
            })
            .await;
            // The child can be killed after the shell creates the pidfile and before it
            // writes the pid; an empty file means the child is already gone, like no file.
            if let Some(pid) = std::fs::read_to_string(pidfile)
                .ok()
                .and_then(|pid| pid.trim().parse::<i32>().ok())
            {
                let pid = nix::unistd::Pid::from_raw(pid);
                let alive = nix::sys::signal::kill(pid, None);
                if alive.is_ok() {
                    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                }
                assert_eq!(alive, Err(nix::errno::Errno::ESRCH));
            }
            closed.expect("undeliverable child outlived explicit close/join");
        }
    })
    .await;
}

// Saved DrainProbe: even a later idle failure stays inside the detached collector.
#[tokio::test(flavor = "multi_thread")]
async fn drain_returns_immediately_before_idle_failure_in_its_detached_collectors() {
    support::watchdog(async {
        let executor = executor();
        let mut request = CommandRequest::new(["sleep", "5"], duration(5000));
        request.idle_timeout = duration(20);
        let session = executor.execute(request, context()).await.unwrap();
        let ctx = context();
        let mut draining = Box::pin(session.drain(&ctx));
        std::future::poll_fn(|cx| {
            assert!(matches!(draining.as_mut().poll(cx), std::task::Poll::Ready(Ok(()))));
            std::task::Poll::Ready(())
        })
        .await;
        // The collectors close after a real 20 ms idle timeout; poll rather than spin, which holds
        // a worker thread on a small runner.
        while !session.base.stdout.is_closed() || !session.base.stderr.is_closed() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(session.is_alive());
        session.close().await.unwrap();
    })
    .await;
}
