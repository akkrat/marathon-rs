//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/BaseCommandExecutorTest.kt
//! WP 4.2 instantiates these same generic tests with its SSH executor.
use super::*;
use marathon_apple::cmd::{CommandExecutor, CommandRequest, consume_output};
use marathon_core::error::VendorError;

async fn run<E: CommandExecutor>(executor: &E, args: &[&str]) -> marathon_apple::cmd::CommandResult {
    run_with_timeout(executor, args, 5000, 1000).await
}
async fn run_with_timeout<E: CommandExecutor>(
    executor: &E,
    args: &[&str],
    total: i64,
    idle: i64,
) -> marathon_apple::cmd::CommandResult {
    let mut request = CommandRequest::new(args.iter().copied(), duration(total));
    request.idle_timeout = duration(idle);
    let session = executor.execute(request, context()).await.unwrap();
    let result = session.await_result(&context()).await.unwrap();
    session.close().await.unwrap();
    result
}
fn request<const N: usize>(args: [&str; N], total: i64, idle: i64) -> CommandRequest {
    let mut request = CommandRequest::new(args, duration(total));
    request.idle_timeout = duration(idle);
    request
}
pub async fn test_stdout<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let result = run(&executor, &["echo", "Hello world"]).await;
        assert_eq!(result.stdout, ["Hello world"]);
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(0));
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_stderr<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let result = run(&executor, &["bash", "-c", "1>/dev/stderr echo Hello world"]).await;
        assert!(result.stdout.is_empty());
        assert_eq!(result.stderr, ["Hello world"]);
        assert_eq!(result.exit_code, Some(0));
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_exit_code<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let result = run(&executor, &["sh", "-c", "exit 1"]).await;
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(1));
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_buffering<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let result = run_with_timeout(&executor, &["sh", "-c", "seq 1 100000"], 10000, 1000).await;
        assert_eq!(result.stdout.len(), 100000);
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(0));
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_consume_output_drains_streams_concurrently<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let session = executor
            .execute(
                request(["sh", "-c", "echo start; seq 1 100000 1>&2; echo end"], 30000, 5000),
                context(),
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = bounded(
            30,
            consume_output(
                &session.base,
                &context(),
                |line| {
                    out.push(line);
                    std::future::ready(Ok(()))
                },
                |line| {
                    err.push(line);
                    std::future::ready(Ok(()))
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(out, ["start", "end"]);
        assert_eq!(err, (1..=100000).map(|i| i.to_string()).collect::<Vec<_>>());
        assert_eq!(exit, Some(0));
        session.close().await.unwrap();
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_consume_output_failure_does_not_block_session_close<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let session = executor
            .execute(
                request(["sh", "-c", "echo start; exec seq 1 100000 1>&2"], 60000, 30000),
                context(),
            )
            .await
            .unwrap();
        let error = consume_output(
            &session.base,
            &context(),
            |line| async move {
                Err(VendorError::Other(failure(
                    "java.lang.IllegalStateException",
                    format!("consumer failed on {line}"),
                )))
            },
            |_| async { Ok(()) },
        )
        .await;
        session.close().await.unwrap();
        assert_eq!(
            error,
            Err(VendorError::Other(failure(
                "java.lang.IllegalStateException",
                "consumer failed on start"
            )))
        );
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_timeout<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let request = request(["sh", "-c", "sleep 100"], 10, 1000);
        let session = executor.execute(request, context()).await.unwrap();
        assert_eq!(session.await_result(&context()).await.unwrap().exit_code, None);
        session.close().await.unwrap();
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_terminate<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let session = executor
            .execute(
                request(["sh", "-c", "while [ true ]; do date && sleep 0.1; done"], 5000, 1000),
                context(),
            )
            .await
            .unwrap();
        let ctx = context();
        let awaiting = session.await_result(&ctx);
        let (result, ()) = tokio::join!(awaiting, async {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            session.terminate().unwrap();
        });
        let result = result.unwrap();
        assert!((3..=10).contains(&result.stdout.len()));
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(143));
        session.close().await.unwrap();
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_terminate_should_not_close_executor<E: CommandExecutor>(executor: E) {
    watchdog(async {
        let session = executor
            .execute(request(["sleep", "1000"], 600000, 600000), context())
            .await
            .unwrap();
        let ctx = context();
        let (result, ()) = tokio::join!(session.await_result(&ctx), async {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            session.terminate().unwrap();
        });
        assert_eq!(result.unwrap().exit_code, Some(143));
        session.close().await.unwrap();
        assert_eq!(
            run_with_timeout(&executor, &["echo", "hello"], 10000, 10000)
                .await
                .combined_stdout(),
            "hello"
        );
        executor.close().await.unwrap();
    })
    .await;
}
pub async fn test_interrupt_using_kill_should_not_close_executor<E: CommandExecutor>(executor: E) {
    watchdog(async {
        // Kept as written: this Kotlin test never calls kill; '&' is an argv item.
        let session = executor
            .execute(request(["sleep", "100000", "&"], 600000, 600000), context())
            .await
            .unwrap();
        let ctx = context();
        let (result, ()) = tokio::join!(session.await_result(&ctx), async {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        });
        assert_ne!(result.unwrap().exit_code, Some(0));
        session.close().await.unwrap();
        assert_eq!(
            run_with_timeout(&executor, &["echo", "hello"], 10000, 10000)
                .await
                .combined_stdout(),
            "hello"
        );
        executor.close().await.unwrap();
    })
    .await;
}
