//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutorTest.kt
//! BaseCommandExecutorTest's ten cases against the SSH executor on the fake worker, with password auth and
//! `debug = true` as in Kotlin. No matrix rows: the rows are 4.1's (4.1 decision 3).
mod support;
use support::{executor_suite as suite, ssh};

macro_rules! suite_case {
    ($name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let fake = ssh::fake(ssh::suite_script(), 4).await;
            suite::$name(ssh::connect(&fake).await).await;
        }
    };
}
suite_case!(test_stdout);
suite_case!(test_stderr);
suite_case!(test_exit_code);
suite_case!(test_buffering);
suite_case!(test_consume_output_drains_streams_concurrently);
suite_case!(test_consume_output_failure_does_not_block_session_close);
suite_case!(test_timeout);
suite_case!(test_terminate);
suite_case!(test_terminate_should_not_close_executor);
suite_case!(test_interrupt_using_kill_should_not_close_executor);
