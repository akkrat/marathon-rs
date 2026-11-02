//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/RemoteFileManager.kt
mod support;
use base64::{Engine, prelude::BASE64_STANDARD};
use jcompat::logging::{Logger, MarathonLogging};
use marathon_apple::{
    cmd::{
        CommandExecutor, CommandHost, CommandRequest, CommandSession, failure, local::KotlinProcessCommandExecutor,
        remote::ssh::sshj::config::SshLogging,
    },
    remote_file_manager::RemoteFileManager,
    worker_commands::ExecutorWorkerCommands,
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use std::sync::{Arc, Mutex};
use support::{
    context, executor,
    ssh::{chunk, connect, exact, fake, output, script},
};

fn manager(executor: Arc<dyn CommandExecutor>, storage: String) -> RemoteFileManager {
    RemoteFileManager::new(
        storage,
        "U".into(),
        Arc::new(ExecutorWorkerCommands {
            executor,
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        }),
        SshLogging::new(
            Arc::new(Mutex::new(MarathonLogging::default())),
            Arc::new(|_, _, _, _| {}),
        ),
    )
}
#[tokio::test(flavor = "multi_thread")]
async fn find_and_shared_mkdir_send_exact_ssh_wire() -> VendorResult<()> {
    let wires = [
        "find /s -mindepth 1 -maxdepth 1 '!' -name 'a*' '!' -name  -exec rm -rf '{}' +",
        "mkdir -p /tmp/marathon/shared/",
    ];
    let fake = fake(
        script(wires.iter().map(|w| exact(w, output(vec![chunk("")], 0))).collect()),
        1,
    )
    .await;
    let executor = Arc::new(connect(&fake).await);
    let fm = manager(executor.clone(), "/s".into());
    fm.remove_remote_directory(&["/x/a*".into(), "/x/b/".into()], &context())
        .await?;
    fm.create_remote_shared_directory(&context()).await?;
    let events = fake.events("exec").await;
    let actual: Vec<_> = events
        .iter()
        .map(|e| BASE64_STANDARD.decode(e["detail"]["base64"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(actual, wires.map(|w| w.as_bytes().to_vec()));
    let routes = fake.events("route").await;
    assert_eq!(routes.len(), 2);
    assert!(routes.iter().all(|r| r["detail"]["warning"].is_null()), "{routes:?}");
    drop(fm);
    executor.close().await?;
    let support::ssh::Fake { worker, dir, .. } = fake;
    let outcome = worker.shutdown().await;
    assert!(outcome.is_ok(), "{outcome:?}");
    drop(dir);
    Ok(())
}
struct RecordingLocal {
    local: KotlinProcessCommandExecutor,
    requests: Mutex<Vec<CommandRequest>>,
    // Run mkdir/find against a temporary filesystem while pinning the public argv.
    temp: String,
}
impl CommandExecutor for RecordingLocal {
    fn host(&self) -> &CommandHost {
        self.local.host()
    }
    fn logger(&self) -> &Logger {
        self.local.logger()
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, message: String) {
        self.local.log_error(message);
    }
    fn execute(&self, mut request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.requests.lock().unwrap().push(request.clone());
        for arg in &mut request.command {
            if arg == "/tmp/marathon/shared/" {
                *arg = format!("{}/shared/", self.temp);
            }
        }
        self.local.execute(request, context)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        self.local.close()
    }
}
fn io(error: std::io::Error) -> VendorError {
    VendorError::Other(failure("java.io.IOException", error.to_string()))
}
#[tokio::test]
async fn local_argv_keeps_glob_and_trailing_slash_and_uses_shell_timeouts() -> VendorResult<()> {
    let dir = tempfile::tempdir().map_err(io)?;
    let root = dir.path().join("storage");
    std::fs::create_dir(&root).map_err(io)?;
    for name in ["alpha", "beta", "gone"] {
        std::fs::write(root.join(name), b"x").map_err(io)?;
    }
    let executor = Arc::new(RecordingLocal {
        local: executor(),
        requests: Mutex::new(vec![]),
        temp: dir.path().to_string_lossy().into_owned(),
    });
    let storage = root.to_string_lossy().into_owned();
    let fm = manager(executor.clone(), storage.clone());
    fm.remove_remote_directory(&["/x/a*".into(), "/x/beta/".into()], &context())
        .await?;
    fm.create_remote_shared_directory(&context()).await?;
    assert!(root.join("alpha").exists());
    assert!(!root.join("beta").exists());
    assert!(!root.join("gone").exists());
    assert!(dir.path().join("shared").is_dir());
    {
        let requests = executor.requests.lock().unwrap();
        assert_eq!(
            requests[0].command,
            vec![
                "find",
                &storage,
                "-mindepth",
                "1",
                "-maxdepth",
                "1",
                "!",
                "-name",
                "a*",
                "!",
                "-name",
                "",
                "-exec",
                "rm",
                "-rf",
                "{}",
                "+"
            ]
        );
        assert_eq!(requests[1].command, vec!["mkdir", "-p", "/tmp/marathon/shared/"]);
        let timeouts = apple_timeout_configuration(AppleTimeoutParameters::empty());
        for request in requests.iter() {
            assert_eq!(request.timeout, timeouts.shell);
            assert_eq!(request.idle_timeout, timeouts.shell_idle);
            assert!(request.env.is_empty());
            assert!(request.workdir.is_none());
        }
    }
    executor.close().await
}
