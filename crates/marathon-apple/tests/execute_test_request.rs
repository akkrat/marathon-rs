//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt (executeTestRequest).
mod support;
#[path = "support/test_root_device.rs"]
mod test_root_device;
use jcompat::logging::{Level, Logger, MarathonLogging};
use marathon_apple::{
    bin::{AppleBinaryEnvironment, Tools},
    cmd::{
        BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandSession, SessionControl,
        remote::ssh::sshj::config::SshLogging,
    },
    execute_test_request::{LineListeners, TestOutput, execute_test_request},
    test::{TestEvent, TestRequest},
    xcodebuild_session::XcodebuildSession,
};
use marathon_config::vendor::{AppleTimeoutParameters, apple_timeout_configuration};
use marathon_core::{
    listener::LineListener,
    runtime::VendorFuture,
    time::{ClockResult, Timer},
    vendor::{Device, VendorContext},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use test_root_device::{Fake, TestResult, v};
struct FixedTimer;
impl Timer for FixedTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn measure(&self, f: &mut dyn FnMut()) -> ClockResult<i64> {
        f();
        Ok(0)
    }
}
struct Executor {
    host: CommandHost,
    logger: Logger,
    command: Mutex<Option<CommandSession>>,
}
impl CommandExecutor for Executor {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, _: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            self.command.lock().unwrap().take().ok_or_else(|| {
                marathon_core::error::VendorError::Other(marathon_apple::cmd::failure("TestFailure", "missing command"))
            })
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Lines {
    seen: Mutex<Vec<String>>,
    active: AtomicUsize,
}
impl LineListener for Lines {
    fn on_line<'a>(&'a self, line: &'a str) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
            tokio::task::yield_now().await;
            self.seen.lock().unwrap().push(line.to_owned());
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[tokio::test]
async fn stderr_drops_blank_lines_warns_with_debug_off_and_serializes_listeners() -> TestResult {
    let registry = Arc::new(Mutex::new(MarathonLogging::default()));
    let logs = Arc::new(Mutex::new(vec![]));
    let captured = logs.clone();
    let logging = SshLogging::new(
        registry.clone(),
        Arc::new(move |logger, level, line, _| {
            captured.lock().unwrap().push((logger.name().to_owned(), level, line));
        }),
    );
    let base = BaseCommand::new(10, Arc::new(support::Runtime));
    for line in ["stdout", "Test Case '-[SampleTests testSample]' started."] {
        v(base.stdout.send(line.into()).await)?;
    }
    base.stdout.close(None);
    for line in ["", " \t", "\u{a0}", " warning "] {
        v(base.stderr.send(line.into()).await)?;
    }
    base.stderr.close(None);
    base.exit_code.complete(Ok(Some(65)));
    let command = CommandSession::new(
        base,
        SessionControl {
            is_alive: Box::new(|| false),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(|| Box::pin(async { Ok(()) })),
        },
    );
    let executor = Arc::new(Executor {
        host: CommandHost {
            id: "test".into(),
            charset: "UTF-8".into(),
        },
        logger: registry.lock().unwrap().logger("test", None),
        command: Mutex::new(Some(command)),
    });
    let mut device = Fake::new(test_root_device::input(), std::path::Path::new("/tmp"));
    device.binary = AppleBinaryEnvironment::with_tools(
        Tools {
            executor,
            runtime: Arc::new(support::Runtime),
            logging: logging.clone(),
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        },
        60_000,
        30_000,
    );
    let device = Arc::new(device);
    let listeners = Arc::new(LineListeners::default());
    let lines = Arc::new(Lines {
        seen: Mutex::default(),
        active: AtomicUsize::new(0),
    });
    listeners.add(lines.clone());
    let session = XcodebuildSession::new(Arc::new(support::Runtime), logging.clone());
    let ctx = support::context();
    let mut request = TestRequest::new("/tmp", "test", false);
    request.test_target_name = Some("RequestTarget".into());
    let channel = execute_test_request(
        device.clone(),
        request,
        session.clone(),
        ctx.clone(),
        TestOutput {
            label: "simulator udid".into(),
            args: vec![],
            timer: Arc::new(FixedTimer),
            listeners,
            logging,
        },
    );
    let mut events = vec![];
    while let Some(list) = v(channel.receive(&ctx).await)? {
        events.extend(list);
    }
    assert!(session.stop(support::duration(1000), &ctx).await.is_none());
    v(session.join_producer().await)?;
    v(device.dispose().await)?;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TestEvent::TestStarted { id } if id.pkg == String::from("RequestTarget")))
    );
    let seen = lines.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(seen.contains(&" warning ".to_owned()));
    let logs = logs.lock().unwrap();
    let warnings = logs
        .iter()
        .filter(|(name, _, _)| name == "Xcodebuild")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0],
        &(
            "Xcodebuild".into(),
            Level::Warn,
            "simulator udid: stderr= warning ".into()
        )
    );
    let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "stderr")
        .unwrap();
    assert_eq!(row["logs"][0]["message"], warnings[0].2);
    Ok(())
}

#[tokio::test]
async fn requests_match_the_frozen_fake_worker_argv() -> TestResult {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use marathon_apple::{
        bin::{
            nm::Nm,
            xcrun::xcodebuild::{Xcodebuild, simulator_test_args},
        },
        cmd::{CommandResult, test_support::FakeCommandExecutor},
        model::{Sdk, XcodeVersion},
    };
    let udid = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
    for (scene, sdk, seq, batch, pkg, class, methods) in [
        (
            "ios-smoke",
            Sdk::IphoneSimulator,
            707,
            Some("67694fa9-6d73-4bbb-aae0-123f55def30a"),
            "sample_appUnitTests",
            "SampleTests",
            vec!["testPassing", "testSkipping", "testFailing"],
        ),
        ("macos-smoke", Sdk::Macos, 397, None, "", "", vec![]),
        (
            "macos-smoke",
            Sdk::Macos,
            553,
            Some("e1589579-0f69-4037-94c2-54733f07292a"),
            "AppleWorldTests",
            "SmokeTests",
            vec!["testPass", "testSkip", "testFail"],
        ),
    ] {
        let fixture = test_root_device::fixtures()
            .join("fake-worker/apple")
            .join(scene)
            .join("expected/raw/transcript.jsonl");
        let text = std::fs::read_to_string(fixture)?;
        let rows = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect::<Vec<_>>();
        let mut registry = MarathonLogging::default();
        let executor = Arc::new(FakeCommandExecutor::new(
            Arc::new(support::Runtime),
            CommandResult {
                stdout: vec![],
                stderr: vec![],
                exit_code: Some(0),
            },
            &mut registry,
        ));
        let tools = Tools {
            executor: executor.clone(),
            timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
            runtime: Arc::new(support::Runtime),
            logging: SshLogging::new(Arc::new(Mutex::new(registry)), Arc::new(|_, _, _, _| {})),
        };
        let build = Xcodebuild::new(tools.clone(), 60_000, 30_000);
        let workdir = format!("/tmp/marathon/{udid}");
        let mut request = TestRequest::new(workdir.clone(), format!("{workdir}/marathon.xctestrun"), false);
        if let Some(batch) = batch {
            request.xcresult = Some(format!("{workdir}/{udid}.{batch}.xcresult"));
            request.tests = Some(
                methods
                    .iter()
                    .map(|method| marathon_core::model::Test {
                        pkg: pkg.into(),
                        clazz: class.into(),
                        method: (*method).into(),
                        meta_properties: vec![],
                    })
                    .collect(),
            );
        }
        let args = if sdk == Sdk::IphoneSimulator {
            simulator_test_args(&[], Some(XcodeVersion::new(14, 0, 0)))
        } else {
            vec![]
        };
        let ctx = support::context();
        let session = v(build.test_without_building(udid, sdk, &request, &args, &ctx).await)?;
        v(session.close().await)?;
        let expected = rows.iter().find(|r| r["seq"] == seq).unwrap();
        let wire = String::from_utf8(STANDARD.decode(expected["detail"]["base64"].as_str().unwrap())?)?;
        assert_eq!(executor.commands.lock().unwrap()[0].join(" "), wire);
        if sdk == Sdk::IphoneSimulator {
            let nm = Nm::new(tools);
            v(
                nm.swift_tests("/tmp/marathon/shared/marathon.xctest/sample-appUnitTests", &ctx)
                    .await,
            )?;
            v(
                nm.objective_c_tests("/tmp/marathon/shared/marathon.xctest/sample-appUnitTests", &ctx)
                    .await,
            )?;
            for (index, seq) in [173, 181].into_iter().enumerate() {
                let row = rows.iter().find(|r| r["seq"] == seq).unwrap();
                let wire = String::from_utf8(STANDARD.decode(row["detail"]["base64"].as_str().unwrap())?)?;
                assert_eq!(executor.commands.lock().unwrap()[index + 1].join(" "), wire);
            }
        }
    }
    Ok(())
}
