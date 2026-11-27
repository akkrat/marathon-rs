mod support;

use base64::{Engine, engine::general_purpose::STANDARD};
use fake_worker::script::{Action, Matcher, Rule};
use marathon_apple::{
    bin::{
        Tools,
        xcrun::simctl::{SimctlService, service::IoService},
    },
    cmd::{
        CommandExecutor, cancelled,
        device_commands::DeviceCommands,
        remote::ssh::sshj::{SshjCommandExecutorFactory, config::SshLogging},
    },
};
use marathon_config::vendor::{AppleTimeoutParameters, Display, Mask, ScreenshotType, apple_timeout_configuration};
use marathon_core::{
    error::VendorError,
    executor::shared::Signal,
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorTimeoutBody},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use marathon_logging::Level;
use std::sync::Arc;
use support::{Clock, Runtime, context, duration, ssh, watchdog};
use tokio::sync::Notify;

const FRAME: &str = "d100-frame-fixture";

fn service(executor: Arc<dyn CommandExecutor>, logging: SshLogging) -> IoService {
    let mut timeouts = AppleTimeoutParameters::empty();
    timeouts.screenshot = Some(duration(60_000));
    IoService::new(SimctlService::new(Tools {
        executor,
        timeouts: Arc::new(apple_timeout_configuration(timeouts)),
        runtime: Arc::new(Runtime),
        logging,
    }))
}

async fn capture(io: &IoService, context: &VendorContext) -> marathon_core::error::VendorResult<Option<Vec<u8>>> {
    io.screenshot_ssh(
        FRAME,
        "/tmp/worker/frame.png",
        ScreenshotType::Png,
        Display::Internal,
        Mask::Black,
        context,
    )
    .await
}

async fn setup(action: Action, silent_exec: bool) -> (ssh::Fake, Arc<DeviceCommands>, IoService, ssh::Events) {
    let fake = ssh::fake_with(
        ssh::script(vec![Rule {
            name: FRAME.into(),
            matcher: Matcher::Pattern(format!("* marathon-screenshot * '{FRAME}' *")),
            action,
        }]),
        1,
        |config| {
            if silent_exec {
                config.script.faults.silent_exec = Some("marathon-screenshot".into());
            }
        },
    )
    .await;
    let (logging, events) = ssh::logging(true);
    let executor = ssh::factory(&logging)
        .connect("127.0.0.1", fake.port(), ssh::password(), None, true)
        .await
        .unwrap();
    let device = DeviceCommands::new(Arc::new(executor));
    let io = service(device.clone(), logging);
    (fake, device, io, events)
}

#[tokio::test]
async fn ssh_screenshot_requires_success_and_keeps_output_private_d100() {
    watchdog(async {
        let payload = STANDARD.encode(b"private frame bytes");
        for status in [0, 37] {
            let wrapped = format!("{}\r\n{}\n", &payload[..8], &payload[8..]);
            let action = ssh::output(
                vec![ssh::chunk(&wrapped), ssh::err_chunk("private screenshot stderr")],
                status,
            );
            let (fake, device, io, events) = setup(action, false).await;
            assert_eq!(
                capture(&io, &context()).await.unwrap(),
                (status == 0).then_some(b"private frame bytes".to_vec())
            );
            device.close().await.unwrap();
            assert!(
                ssh::lines(&events)
                    .iter()
                    .all(|(_, _, message)| !message.contains(&payload[..8]) && !message.contains("private screenshot"))
            );
            fake.worker.shutdown().await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn ssh_screenshot_stops_mid_frame_without_waiting_for_its_exit_d100() {
    watchdog(async {
        let (fake, device, io, events) = setup(ssh::hang(vec![ssh::chunk("Zmlyc3QgYnl0ZXM=\n")]), false).await;
        let stop = Signal::new();
        let context = VendorContext {
            cancellation: stop.clone(),
            ..context()
        };
        let frame = tokio::spawn(async move { capture(&io, &context).await });
        fake.wait_for_events("output", 1, |row| row["detail"]["extended"].is_null())
            .await;
        let stopped = cancelled("fixture test ended");
        stop.cancel(stopped.clone());
        assert_eq!(frame.await.unwrap(), Err(VendorError::Cancelled(stopped)));
        fake.wait_for_events("signal", 1, |row| row["detail"]["signal"] == "TERM")
            .await;
        device.wait_handed(Signal::new()).await.unwrap();
        assert!(device.handed_left().is_empty());
        assert!(ssh::lines(&events).iter().all(|(_, level, _)| *level != Level::ERROR));
        device.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn ssh_screenshot_aborts_an_oversize_unterminated_stream_d100() {
    watchdog(async {
        // Do not finish the command or terminate the oversized output with a newline.
        let action = ssh::hang(vec![ssh::err_chunk(&"x".repeat(64 * 1024 + 1))]);
        let (fake, device, io, _) = setup(action, false).await;
        let error = capture(&io, &context()).await.unwrap_err();
        assert_eq!(error.failure().kind, FailureKind::Io);
        assert!(error.failure().to_string().contains("output limit"));
        fake.wait_for_events("signal", 1, |row| row["detail"]["signal"] == "TERM")
            .await;
        device.wait_handed(Signal::new()).await.unwrap();
        device.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[derive(Default)]
struct FrameDeadline {
    clock: Clock,
    expire: Notify,
}

impl VendorClock for FrameDeadline {
    fn now_millis(&self) -> i64 {
        self.clock.now_millis()
    }
    fn delay(&self, millis: i64, signal: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        self.clock.delay(millis, signal)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        self.clock.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        if millis != 60_000 {
            return self.clock.with_timeout_or_null(parent, millis, body);
        }
        Box::pin(async move {
            let child = Signal::new();
            let mut capture = body(child.clone());
            tokio::select! { biased;
                result = &mut capture => result.map(Some),
                _ = self.expire.notified() => {
                    let expired = cancelled("fixture deadline");
                    child.cancel(expired.clone());
                    assert_eq!(capture.await, Err(VendorError::Cancelled(expired)));
                    Ok(None)
                }
            }
        })
    }
}

#[tokio::test]
async fn ssh_screenshot_deadline_includes_waiting_for_the_exec_reply_d100() {
    watchdog(async {
        let (fake, device, io, events) = setup(ssh::hang(vec![]), true).await;
        let clock = Arc::new(FrameDeadline::default());
        let context = VendorContext {
            clock: clock.clone(),
            ..context()
        };
        let frame = tokio::spawn(async move { capture(&io, &context).await });
        fake.wait_for_events("exec", 1, |_| true).await;
        clock.expire.notify_one();
        assert_eq!(frame.await.unwrap(), Ok(None));
        fake.wait_for_events("client_close", 1, |_| true).await;
        assert!(ssh::lines(&events).iter().all(|(_, level, _)| *level != Level::ERROR));
        device.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn rejected_ssh_screenshot_does_not_wait_for_the_channel_close_d100() {
    watchdog(async {
        let fake = ssh::fake_with(ssh::script(vec![]), 1, |config| {
            config.script.faults.reject_exec = true;
            config.script.faults.withhold_close = true;
        })
        .await;
        let (logging, events) = ssh::logging(false);
        // Hold the legacy failed-exec close timeout as well as the peer's CLOSE.
        let close_timeout = Arc::new(Notify::new());
        let clock = Arc::new(Clock::with_gated_delay(5_000, close_timeout));
        let executor = Arc::new(
            SshjCommandExecutorFactory::new(logging.clone(), Arc::new(Runtime), clock)
                .connect("127.0.0.1", fake.port(), ssh::password(), None, false)
                .await
                .unwrap(),
        );
        let io = service(executor.clone(), logging);
        assert_eq!(capture(&io, &context()).await, Ok(None));
        fake.wait_for_events("client_close", 1, |row| row["detail"]["reply_withheld"] == true)
            .await;
        assert_eq!(
            ssh::lines(&events)
                .iter()
                .filter(|(_, level, message)| *level == Level::ERROR && message.starts_with("Unable to start"))
                .count(),
            1
        );
        executor.close().await.unwrap();
        fake.worker.shutdown().await.unwrap();
    })
    .await;
}
