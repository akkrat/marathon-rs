//! The command executors on the production wiring: `CoreExecutor::run`, `core.vendor()` and `core.vendor_clock()`,
//! not the test runtime. The other executor tests run under `#[tokio::test]`, whose runtime has every driver
//! enabled, so they cannot notice a vendor runtime that lacks one (WP 4.2 review round 1, R1-sol-1: sockets,
//! child pipes and SIGCHLD all need the I/O driver).
mod support;
use marathon_apple::cmd::{
    CommandExecutor, CommandRequest, local::KotlinProcessCommandExecutor, remote::ssh::sshj::SshjCommandExecutorFactory,
};
use marathon_core::{
    error::{Failure, VendorError},
    executor::{CoreExecutor, shared::failure},
};
use std::rc::Rc;
use support::{duration, ssh};

fn assertion(step: &str, error: impl std::fmt::Debug) -> Failure {
    failure("java.lang.AssertionError", &format!("{step}: {error:?}"))
}
/// Runs `echo hello` and returns its stdout and exit code.
async fn echo(core: &Rc<CoreExecutor>, executor: &dyn CommandExecutor) -> Result<(Vec<String>, Option<i32>), Failure> {
    let mut context = support::context();
    context.clock = core.vendor_clock();
    let session = executor
        .execute(
            CommandRequest::new(["echo", "hello"], duration(10_000)),
            context.clone(),
        )
        .await
        .map_err(|e: VendorError| assertion("execute", e))?;
    let output = session.await_result(&context).await;
    let _ = session.close().await;
    let output = output.map_err(|e| assertion("result", e))?;
    Ok((output.stdout, output.exit_code))
}
fn expected() -> Result<(Vec<String>, Option<i32>), marathon_core::error::OperationError> {
    Ok((vec!["hello".to_owned()], Some(0)))
}

#[test]
fn ssh_connects_and_executes_on_the_production_vendor_runtime() {
    // The fake worker needs a runtime of its own: CoreExecutor::run refuses to start inside one.
    let worker_runtime = tokio::runtime::Runtime::new().unwrap();
    let fake = worker_runtime.block_on(ssh::fake(ssh::suite_script(), 4));
    let port = fake.port();
    let result = CoreExecutor::run(move |core| {
        Box::pin(async move {
            let (logging, _) = ssh::logging(false);
            let factory = SshjCommandExecutorFactory::new(logging, core.vendor(), core.vendor_clock());
            let executor = factory
                .connect("127.0.0.1", port, ssh::password(), None, false)
                .await
                .map_err(|e| assertion("connect", e))?;
            let output = echo(&core, &executor).await;
            executor.close().await.map_err(|e| assertion("close", e))?;
            Ok(output?)
        })
    });
    assert_eq!(result, expected());
    // close() returns once the client side is done; the fake records the server-side end a
    // moment later, so wait for it instead of reading the transcript immediately.
    let ends = worker_runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ends = fake.events("connection_end").await;
            if !ends.is_empty() || tokio::time::Instant::now() >= deadline {
                return ends;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    assert_eq!(ends.len(), 1);
}

#[test]
fn local_process_executes_on_the_production_vendor_runtime() {
    let result = CoreExecutor::run(move |core| {
        Box::pin(async move {
            let executor = KotlinProcessCommandExecutor::new(core.vendor(), core.logging(), |_, _| {}, false, 64);
            Ok(echo(&core, &executor).await?)
        })
    });
    assert_eq!(result, expected());
}

#[test]
fn factory_and_shared_transfers_cross_the_production_core_vendor_bridge() {
    use marathon_apple::{
        cmd::{shared_transfers::SharedTransfers, write_file::write_file},
        device::connection_factory::ConnectionFactory,
    };
    use marathon_config::{
        devices::Transport,
        vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration},
    };
    use marathon_core::executor::bridge::call_vendor;
    let worker_runtime = tokio::runtime::Runtime::new().unwrap();
    let fake = worker_runtime.block_on(ssh::fake(ssh::suite_script(), 4));
    worker_runtime
        .block_on(fake.worker.fs.mkdir("/tmp/marathon/shared"))
        .unwrap();
    let port = fake.port();
    let result = CoreExecutor::run(move |core| {
        Box::pin(async move {
            let runtime = core.vendor();
            let clock = core.vendor_clock();
            let (logging, _) = ssh::logging(false);
            let mut context = support::context();
            context.clock = clock.clone();
            let signal = context.cancellation.clone();
            let count = call_vendor(runtime.clone().as_ref(), signal, move |cancellation| {
                Box::pin(async move {
                    context.cancellation = cancellation;
                    let config = SshConfiguration {
                        authentication: Some(SshAuthentication::Password {
                            username: "fixture".into(),
                            password: "fake-password".into(),
                        }),
                        known_hosts_path: None,
                        keep_alive_interval: duration(0),
                        keep_alive_max_count: 4,
                        debug: false,
                        share_worker_connection: true,
                    };
                    let factory = ConnectionFactory::new(
                        false,
                        config,
                        RsyncConfiguration {
                            remote_path: "/usr/bin/rsync".into(),
                        },
                        duration(1000),
                        runtime.clone(),
                        clock,
                        logging.clone(),
                    )
                    .map_err(|e| e.into_vendor())?;
                    let t = Transport::Ssh {
                        addr: "127.0.0.1".into(),
                        port: i32::from(port),
                        authentication: None,
                        check_reachability: false,
                    };
                    let executor = factory
                        .connect_worker(t, context.clone())
                        .await
                        .map_err(|e| e.into_vendor())?;
                    let transfers = SharedTransfers::new(runtime, logging.clone());
                    let mut pushes = 0;
                    for _ in 0..2 {
                        transfers
                            .send(
                                "worker:22",
                                std::path::Path::new("/generated"),
                                "/tmp/marathon/shared/generated",
                                || async { Ok(Some(true)) },
                                || async {
                                    pushes += 1;
                                    write_file(
                                        executor.as_ref(),
                                        b"production",
                                        "/tmp/marathon/shared/generated",
                                        duration(5000),
                                        context.clone(),
                                        &logging,
                                    )
                                    .await
                                },
                            )
                            .await?;
                    }
                    factory.dispose(executor).await?;
                    factory.join_pending().await?;
                    Ok(pushes)
                })
            })
            .await
            .map_err(|e| assertion("factory bridge", e))?;
            Ok(count)
        })
    });
    assert_eq!(result, Ok(1));
    assert_eq!(
        worker_runtime
            .block_on(fake.worker.fs.read("/tmp/marathon/shared/generated"))
            .unwrap(),
        b"production"
    );
    worker_runtime.block_on(fake.worker.shutdown()).unwrap();
}
