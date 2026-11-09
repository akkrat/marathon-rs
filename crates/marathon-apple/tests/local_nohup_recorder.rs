//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt
//! A nohup-started worker process must outlive the command session that started it, until the stop command signals it.
#![cfg(target_os = "macos")]
// The stop command runs on macOS workers, whose sh treats `&>` as a redirect;
// dash (Linux /bin/sh) backgrounds the command instead, so this check is macOS-only.
mod support;
use marathon_apple::cmd::{CommandExecutor, CommandRequest};
use support::*;

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn nohup_process_survives_its_session_until_signalled() {
    watchdog(async {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("rec.pid");
        let pidfile = pidfile.to_str().unwrap().to_owned();
        let executor = executor();
        let context = context();
        // Handles SIGINT like simctl recordVideo does, then needs a moment to finalize.
        let recorder = [
            "perl",
            "-e",
            "'$SIG{INT}=sub{select(undef,undef,undef,0.4);exit 0};sleep 30'",
        ];
        let started = executor
            .safe_execute_nohup(&pidfile, CommandRequest::new(recorder, duration(5000)), context.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(started.exit_code, Some(0));
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(alive(&pid), "recorder died after its start session ended");
        let stopped = executor
            .safe_execute(
                CommandRequest::new(
                    [
                        "sh".to_owned(),
                        "-c".to_owned(),
                        format!("kill -s INT $(cat {pidfile}) && lsof -p $(cat {pidfile}) +r 1 &>/dev/null"),
                    ],
                    duration(10_000),
                ),
                context,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.exit_code, Some(0), "{stopped:?}");
        assert!(!alive(&pid));
    })
    .await;
}
