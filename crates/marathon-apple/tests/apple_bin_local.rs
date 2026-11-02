//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/AppleBinaryEnvironment.kt
//! Additional wire tests (no Kotlin matrix rows): the wrappers over the merged local executor, with fake tools
//! first on PATH. The local executor strips one pair of outer quotes per argument and runs no shell, so the
//! `sh -c '<pipeline>'` wrappers run their pipeline and bash-escaped arguments arrive with their inner escapes.
//! The fake tools are found through a PATH the test adds to each request, after recording that the wrapper
//! itself sent no env. Only the `sh -c` wrappers are exercised: the executor resolves its program with the
//! parent's PATH (as Java's ProcessBuilder does), the shell resolves the pipeline with the request's.
mod support;
use jcompat::logging::{Logger, MarathonLogging};
use marathon_apple::{
    bin::{AppleBinaryEnvironment, Tools},
    cmd::{
        CommandExecutor, CommandHost, CommandRequest, CommandSession, local::KotlinProcessCommandExecutor,
        remote::ssh::sshj::config::SshLogging,
    },
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use support::{Runtime, context, executor};

struct WithPath {
    local: KotlinProcessCommandExecutor,
    path: String,
    envs: Mutex<Vec<usize>>,
}
impl CommandExecutor for WithPath {
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
        self.local.log_error(message)
    }
    fn execute(&self, mut request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.envs.lock().unwrap().push(request.env.len());
        request.env.insert("PATH".into(), self.path.clone());
        self.local.execute(request, context)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        self.local.close()
    }
}

#[tokio::test]
async fn wrappers_run_these_commands_locally() {
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("argv.log");
    let tool = |name: &str, body: &str| {
        let path = bin.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let record = format!(
        "for a in \"$@\"; do printf '<%s>' \"$a\" >> '{}'; done; echo >> '{}'",
        log.display(),
        log.display()
    );
    tool("ioreg", "echo '<plist/>'");
    tool("plutil", &format!("{record}\ncat > /dev/null; echo ' LOCAL-UUID '"));
    tool(
        "nm",
        &format!("{record}\nprintf '_$s5Tests1AC5testAyyF\\n_$s5Tests1AC5testByyF\\n'"),
    );
    // `xcrun swift-demangle <symbols>` prints `<mangled> ---> <demangled>` per argument.
    tool(
        "xcrun",
        "shift; for s in \"$@\"; do echo \"$s ---> Tests.A.test${s#*test}\"; done",
    );
    tool(
        "system_profiler",
        "printf 'Hardware:\\n\\n      Provisioning UDID: 00008112-ABC\\n'",
    );
    let executor = Arc::new(WithPath {
        local: executor(),
        path: format!("{}:/usr/bin:/bin", bin.path().display()),
        envs: Mutex::new(vec![]),
    });
    let tools = Tools {
        executor: executor.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(Runtime),
        logging: SshLogging::new(
            Arc::new(Mutex::new(MarathonLogging::default())),
            Arc::new(|_, _, _, _| {}),
        ),
    };
    let env = AppleBinaryEnvironment::with_tools(tools, 1_800_000, 300_000);
    let c = &context();

    assert_eq!(env.ioreg.get_udid(c).await.unwrap(), "LOCAL-UUID");
    assert_eq!(
        env.nm.swift_tests("/b/x y", c).await.unwrap(),
        vec!["Tests.A.testAyyF", "Tests.A.testByyF"]
    );
    assert_eq!(
        env.system_profiler.get_provisioning_udid(c).await.unwrap(),
        "00008112-ABC"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "<-extract><IORegistryEntryChildren.0.IOPlatformUUID><raw><->\n<-gUj></b/x y>\n"
    );
    assert_eq!(*executor.envs.lock().unwrap(), vec![0; 3]);
}
