//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/WriteFileTest.kt
use marathon_error::FailureKind;
mod support;
use jcompat::logging::Logger;
use marathon_apple::cmd::{
    CommandExecutor, CommandHost, CommandRequest, CommandSession, failure,
    local::KotlinProcessCommandExecutor,
    remote::ssh::sshj::{config::SshLogging, shell_command},
    write_file::{MAX_WRITTEN_FILE_SIZE, write_file},
};
use marathon_core::{error::VendorError, runtime::VendorFuture, vendor::VendorContext};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use support::{Runtime, duration, ssh};
struct Recording {
    local: KotlinProcessCommandExecutor,
    commands: Mutex<Vec<CommandRequest>>,
    shell: bool,
    path: Option<String>,
    fail: bool,
}
impl CommandExecutor for Recording {
    fn host(&self) -> &CommandHost {
        self.local.host()
    }
    fn logger(&self) -> &Logger {
        self.local.logger()
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, _: String) {}
    fn close(&self) -> VendorFuture<'_, ()> {
        self.local.close()
    }
    fn execute(&self, mut request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.commands.lock().unwrap().push(request.clone());
        if self.fail {
            return Box::pin(async { Err(VendorError::Other(failure(FailureKind::IllegalState, "Not connected"))) });
        }
        if self.shell {
            let profile = self
                .path
                .as_ref()
                .map(|p| format!("export PATH={p}:$PATH; "))
                .unwrap_or_default();
            request.command = vec!["sh".into(), "-c".into(), profile + &shell_command(&request.command)];
        }
        self.local.execute(request, context)
    }
}
fn executor(shell: bool) -> (Recording, SshLogging) {
    let logging = ssh::logging(true).0;
    let local = KotlinProcessCommandExecutor::new(Arc::new(Runtime), logging.registry.clone(), |_, _| {}, false, 64);
    (
        Recording {
            local,
            commands: Mutex::new(Vec::new()),
            shell,
            path: None,
            fail: false,
        },
        logging,
    )
}
fn content() -> Vec<u8> {
    (0..=255)
        .chain(b"<key>ENV</key><string>$HOME '\"`;|&><*?</string>\n".iter().copied())
        .collect()
}
async fn write(ex: &dyn CommandExecutor, logging: &SshLogging, bytes: &[u8], dst: &str) -> bool {
    write_file(ex, bytes, dst, duration(30_000), support::context(), logging)
        .await
        .unwrap()
}
#[tokio::test]
async fn a_file_is_written_with_one_command_and_readable_only_by_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("marathon.xctestrun");
    let (ex, log) = executor(false);
    assert!(write(&ex, &log, &content(), dst.to_str().unwrap()).await);
    assert_eq!(std::fs::read(&dst).unwrap(), content());
    assert_eq!(std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777, 0o600);
    let requests = ex.commands.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(&requests[0].command[..2], ["sh", "-c"]);
    assert!(requests[0].env.is_empty());
    assert!(requests[0].workdir.is_none());
}
#[tokio::test]
async fn the_command_survives_the_quoting_of_an_ssh_command_line_and_replaces_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("marathon.xctestrun");
    std::fs::write(&dst, b"previous run").unwrap();
    let (ex, log) = executor(true);
    assert!(write(&ex, &log, &content(), dst.to_str().unwrap()).await);
    assert_eq!(std::fs::read(dst).unwrap(), content());
}
#[tokio::test]
async fn files_up_to_the_limit_are_written_and_larger_ones_are_refused_without_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("largest");
    let (ex, log) = executor(true);
    let mut bytes = (0..MAX_WRITTEN_FILE_SIZE).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    assert!(write(&ex, &log, &bytes, dst.to_str().unwrap()).await);
    assert_eq!(std::fs::read(dst).unwrap(), bytes);
    bytes.push(0);
    assert!(!write(&ex, &log, &bytes, dir.path().join("too-large").to_str().unwrap()).await);
    assert_eq!(ex.commands.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn paths_that_would_need_quoting_are_refused_without_a_command() {
    let (ex, log) = executor(false);
    for dst in [
        "relative/marathon.xctestrun",
        "/tmp/with space",
        "/tmp/quote'd",
        "/tmp/$HOME",
        "/tmp/a;b",
        "",
    ] {
        assert!(!write(&ex, &log, &content(), dst).await);
    }
    assert!(ex.commands.lock().unwrap().is_empty());
}
#[tokio::test]
async fn a_failed_write_leaves_the_destination_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("marathon.xctestrun");
    std::fs::write(&dst, b"previous run").unwrap();
    std::fs::create_dir(dir.path().join("marathon.xctestrun.marathon-tmp")).unwrap();
    let (ex, log) = executor(false);
    assert!(!write(&ex, &log, &content(), dst.to_str().unwrap()).await);
    assert_eq!(std::fs::read(dst).unwrap(), b"previous run");
    assert!(
        !write(
            &ex,
            &log,
            &content(),
            dir.path().join("missing/marathon.xctestrun").to_str().unwrap()
        )
        .await
    );
    assert!(!dir.path().join("missing").exists());
}
#[tokio::test]
async fn a_file_that_was_not_decoded_completely_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let shim = bin.join("base64");
    std::fs::write(&shim, b"#!/bin/sh\nprintf truncated\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let dst = dir.path().join("marathon.xctestrun");
    std::fs::write(&dst, b"previous run").unwrap();
    let (mut ex, log) = executor(true);
    ex.path = Some(bin.to_string_lossy().into_owned());
    assert!(!write(&ex, &log, &content(), dst.to_str().unwrap()).await);
    assert_eq!(std::fs::read(dst).unwrap(), b"previous run");
    assert!(!dir.path().join("marathon.xctestrun.marathon-tmp").exists());
}
#[tokio::test]
async fn an_executor_that_can_not_run_the_command_reports_a_failed_write() {
    let (mut ex, log) = executor(false);
    ex.fail = true;
    assert!(!write(&ex, &log, &content(), "/tmp/marathon.xctestrun").await);
}
#[tokio::test]
async fn generated_file_ios_routing_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (ex, log) = executor(false);
    for (ssh, debug, size, valid) in [
        (true, false, 1, true),
        (true, true, 1, true),
        (true, false, 32769, true),
        (true, false, 1, false),
        (false, false, 1, true),
    ] {
        let dst = if valid {
            dir.path().join("generated").to_string_lossy().into_owned()
        } else {
            dir.path().join("missing/file").to_string_lossy().into_owned()
        };
        let bytes = vec![1; size];
        let before = ex.commands.lock().unwrap().len();
        let mut pushes = 0;
        let success = if ssh && !debug && size <= MAX_WRITTEN_FILE_SIZE && write(&ex, &log, &bytes, &dst).await {
            true
        } else {
            pushes += 1;
            true
        };
        assert!(success);
        assert_eq!(
            pushes,
            usize::from(!(ssh && !debug && size <= MAX_WRITTEN_FILE_SIZE && valid))
        );
        assert_eq!(
            ex.commands.lock().unwrap().len() - before,
            usize::from(ssh && !debug && size <= MAX_WRITTEN_FILE_SIZE)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_writefile_preserves_every_byte_limit_and_failed_install() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    fake.worker.fs.mkdir("/tmp/marathon/shared").await.unwrap();
    let executor = ssh::connect(&fake).await;
    let log = ssh::logging(false).0;
    let dst = "/tmp/marathon/shared/bytes";
    assert!(write(&executor, &log, &content(), dst).await);
    assert_eq!(fake.worker.fs.read(dst).await.unwrap(), content());
    let bytes = (0..MAX_WRITTEN_FILE_SIZE).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    assert!(write(&executor, &log, &bytes, dst).await);
    assert_eq!(fake.worker.fs.read(dst).await.unwrap(), bytes);
    assert!(!write(&executor, &log, &vec![0; MAX_WRITTEN_FILE_SIZE + 1], dst).await);
    fake.worker
        .fs
        .mkdir("/tmp/marathon/shared/bytes.marathon-tmp")
        .await
        .unwrap();
    assert!(!write(&executor, &log, &content(), dst).await);
    assert_eq!(fake.worker.fs.read(dst).await.unwrap(), bytes);
    executor.close().await.unwrap();
    fake.worker.shutdown().await.unwrap();
}
