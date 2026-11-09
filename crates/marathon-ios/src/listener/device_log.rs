//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/log/DeviceLogListener.kt
use super::*;
use marathon_apple::model::Sdk;
pub struct DeviceLogListener {
    c: ListenerContext,
    logger: Logger,
    pid: Mutex<Option<String>>,
}
pub fn log_stream_command(udid: &str, path: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "xcrun simctl spawn {udid} log stream --type log --color none --style compact 2>/dev/null > {path} & echo $!"
        ),
    ]
}
pub fn parse_pid(successful: bool, stdout: &str) -> Option<String> {
    let pid = super::trim(stdout);
    (successful && pid.parse::<i32>().is_ok()).then(|| pid.to_owned())
}
impl DeviceLogListener {
    pub fn new(c: ListenerContext) -> Self {
        let logger = c.logger("com.malinskiy.marathon.apple.ios.listener.log.DeviceLogListener");
        Self {
            c,
            logger,
            pid: Mutex::new(None),
        }
    }
    fn enabled(&self) -> bool {
        if !self.c.device.services().vendor.device_log {
            return false;
        }
        if !matches!(
            self.c.device.sdk(),
            Sdk::IphoneSimulator | Sdk::TvSimulator | Sdk::WatchSimulator | Sdk::VisionSimulator
        ) {
            self.c.log(
                &self.logger,
                Level::Warn,
                format!(
                    "Device {} does not support capturing device logs",
                    self.c.device.serial()
                ),
            );
            return false;
        }
        true
    }
}
impl AppleTestRunListener for DeviceLogListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if !self.enabled() {
                return Ok(());
            }
            let d = &self.c.device;
            let result = d
                .critical_listener_command(
                    log_stream_command(d.udid(), &d.remote_file_manager().remote_log()),
                    &self.c.context,
                )
                .await?;
            if let Some(pid) = parse_pid(result.successful(), &result.combined_stdout()) {
                *self.pid.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
            }
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if !self.enabled() {
                return Ok(());
            }
            let pid = self.pid.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(pid) = pid {
                self.c
                    .device
                    .execute_worker_command(
                        vec!["sh".into(), "-c".into(), format!("kill -s INT {pid}")],
                        &self.c.context,
                    )
                    .await?;
                let local = self.c.file(FileType::DeviceLog, None).await?;
                let remote = self.c.device.remote_file_manager().remote_log();
                let start = self.c.millis("ios.DeviceLogListener.pullLogfile")?;
                self.c.device.pull_file(&remote, &local, &self.c.context).await?;
                self.c.log(
                    &self.logger,
                    Level::Debug,
                    format!(
                        "Pulling finished in {}ms {remote} ",
                        self.c.millis("ios.DeviceLogListener.pullLogfile")?.wrapping_sub(start)
                    ),
                );
            }
            Ok(())
        })
    }
}
