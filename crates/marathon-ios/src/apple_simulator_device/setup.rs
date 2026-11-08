//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use crate::device::worker_provisioning_context::WorkerFacts;
use marathon_apple::{
    cmd::{CommandResult, failure, remote::ssh::sshj::error::CONNECTION_EXCEPTION},
    plist::worker_descriptor,
};
use marathon_core::model::DeviceFeature;

pub(super) fn setup_error(message: impl AsRef<str>) -> VendorError {
    VendorError::DeviceSetup(failure(
        "com.malinskiy.marathon.exceptions.DeviceSetupException",
        message,
    ))
}
pub(super) fn env_value(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_owned())
}
impl AppleSimulatorDevice {
    pub async fn setup_record(
        &self,
        record: &SimctlDevice,
        cycle: Arc<WorkerProvisioningContext>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let facts = cycle
            .worker_facts(self.executor.clone(), context, || self.load_worker_facts(context))
            .await
            .map_err(|e| e.into_vendor())?;
        {
            let mut state = self.state();
            state.env = Some(facts.env.clone());
            state.xcode_version = Some(facts.xcode_version);
        }
        let home = match env_value(&facts.env, "HOME") {
            Some(value) => value,
            None => self
                .bin
                .xcrun
                .simctl
                .simulator
                .getenv(&self.udid, "SIMULATOR_HOST_HOME", context)
                .await?
                .unwrap_or_default(),
        };
        self.state().home = Some(home.clone());
        if home.trim().is_empty() {
            return Err(setup_error(format!(
                "simulator {}: invalid value {home} for environment variable HOME",
                self.udid
            )));
        }
        let simulator_home = self
            .bin
            .xcrun
            .simctl
            .simulator
            .getenv(&self.udid, "HOME", context)
            .await?;
        let fresh;
        let record = if simulator_home.is_none() {
            fresh = cycle
                .simulator(&self.udid, &self.bin, true, context)
                .await
                .map_err(|e| e.into_vendor())?;
            &fresh
        } else {
            record
        };
        let default = format!("{home}/Library/Developer/CoreSimulator/Devices/{}", self.udid);
        let root = simulator_home
            .map(|v| {
                if v.ends_with("/data") {
                    v.split_once("/data").map_or(v.clone(), |(root, _)| root.to_owned())
                } else {
                    v
                }
            })
            .unwrap_or_else(|| default.clone());
        {
            let mut state = self.state();
            state.log_file = format!("{}/system.log", record.log_path.as_deref().unwrap_or(&default));
            state.root_path = root.clone();
            state.descriptor_path = format!("{root}/device.plist");
        }
        self.fetch_device_descriptor(context).await?;
        let device_type = match record.device_type_identifier.clone() {
            Some(kind) => kind,
            None => match self
                .state()
                .descriptor
                .as_ref()
                .and_then(|p| p.get_cloned("deviceType"))
            {
                Some(marathon_apple::plist::PlistValue::String(kind)) => kind.trim().to_owned(),
                Some(other) => {
                    return Err(marathon_apple::xctestrun::test_root_factory::FactoryError::from(
                        marathon_apple::plist::PlistError::WrongType {
                            key: "deviceType".into(),
                            expected: "string",
                            found: other.type_name(),
                        },
                    )
                    .into_vendor());
                }
                None => {
                    return Err(setup_error(format!(
                        "simulator {}: unable to detect deviceType",
                        self.udid
                    )));
                }
            },
        };
        self.state().device_type = device_type.clone();
        let model = self
            .simple_env("SIMULATOR_MODEL_IDENTIFIER", &device_type, context)
            .await?;
        {
            let mut state = self.state();
            state.info.model = model;
            state.info.manufacturer = "Apple".into();
        }
        let runtime_build = self
            .simple_env("SIMULATOR_RUNTIME_BUILD_VERSION", "Unknown", context)
            .await?;
        self.state().runtime_build_version = runtime_build;
        let runtime_version = self.simple_env("SIMULATOR_RUNTIME_VERSION", "Unknown", context).await?;
        self.state().runtime_version = Some(runtime_version);
        let version = self.simple_env("SIMULATOR_VERSION_INFO", "Unknown", context).await?;
        // D36: on a lost connection the probes above answer null, which reads as "use the default". A simulator
        // described that way must not register: the provider retries the connection instead (A17).
        self.connection_alive()?;
        let mut state = self.state();
        state.version = version;
        state.abi = facts.arch.clone();
        state.info.device_features = facts.features.clone();
        state.supports_transcoding = facts.supports_transcoding;
        Ok(())
    }
    pub async fn load_worker_facts(&self, context: &VendorContext) -> ProvisioningResult<WorkerFacts> {
        let result = self.execute_worker_command(vec!["env".into()], context).await?;
        let env = result.filter(CommandResult::successful).ok_or_else(|| {
            setup_error(format!(
                "simulator {}: unable to detect environment variables",
                self.udid
            ))
        })?;
        let mut values: Vec<(String, String)> = vec![];
        for line in env.stdout {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Kotlin's substringAfter without a delimiter returns the original string.
            let (key, value) = line.split_once('=').unwrap_or((line, line));
            let (key, value): (String, String) = (key.trim().into(), value.trim().into());
            if let Some((_, old)) = values.iter_mut().find(|(k, _)| *k == key) {
                *old = value;
            } else {
                values.push((key, value));
            }
        }
        let xcode_version = self.bin.xcrun.xcodebuild.get_version(context).await?;
        let arch = self
            .execute_worker_command(vec!["uname".into(), "-m".into()], context)
            .await?;
        let displays = self
            .execute_worker_command(
                [
                    "/usr/sbin/system_profiler",
                    "-detailLevel",
                    "mini",
                    "-xml",
                    "SPDisplaysDataType",
                ]
                .map(str::to_owned)
                .to_vec(),
                context,
            )
            .await?;
        let transcoding = &self
            .services
            .vendor
            .screen_record_configuration
            .video_configuration
            .transcoding;
        let transcoder = if transcoding.enabled {
            self.execute_worker_command(vec![transcoding.binary.to_owned(), "-version".into()], context)
                .await?
        } else {
            None
        };
        let mut probes = vec![arch.as_ref(), displays.as_ref()];
        if transcoding.enabled {
            probes.push(transcoder.as_ref());
        }
        let mut features = vec![];
        // The Kotlin safe-call checks that success is non-null, not that it is true.
        if displays.as_ref().is_some_and(|r| {
            r.stdout
                .iter()
                .any(|l| l.contains("spdisplays_metalfeatureset") || l.contains("spdisplays_metalfamily"))
        }) {
            features.push(DeviceFeature::Video);
        }
        features.push(DeviceFeature::Screenshot);
        Ok(WorkerFacts {
            env: values,
            xcode_version,
            arch: arch
                .as_ref()
                .filter(|r| r.successful())
                .map_or_else(|| String::from("Unknown"), |r| r.combined_stdout().trim().into()),
            features,
            supports_transcoding: transcoding.enabled && transcoder.as_ref().is_some_and(|r| r.successful()),
            shareable: probes.iter().all(|p| p.is_some_and(|r| r.exit_code.is_some())),
        })
    }
    async fn simple_env(&self, key: &str, default: &str, context: &VendorContext) -> VendorResult<String> {
        Ok(self
            .bin
            .xcrun
            .simctl
            .simulator
            .getenv(&self.udid, key, context)
            .await?
            .unwrap_or_else(|| default.into()))
    }
    pub async fn read_textfile(&self, path: &str, context: &VendorContext) -> VendorResult<Option<String>> {
        Ok(self
            .execute_worker_command(vec!["cat".into(), path.into()], context)
            .await?
            .filter(CommandResult::successful)
            .map(|r| r.combined_stdout()))
    }
    pub(super) async fn fetch_device_descriptor(&self, context: &VendorContext) -> VendorResult<()> {
        let path = self.state().descriptor_path.clone();
        if path.is_empty() {
            return Err(uninitialized("devicePlistPath"));
        }
        let text = self.read_textfile(&path, context).await?;
        // D36: without this a lost connection reads as state UNKNOWN, and the simulator as one that failed to boot.
        if text.is_none() {
            self.connection_alive()?;
        }
        let descriptor = text.map(|s| worker_descriptor(s.as_bytes())).transpose()?;
        self.state().descriptor = descriptor;
        Ok(())
    }
    fn connection_alive(&self) -> VendorResult<()> {
        if self.executor.connected() {
            return Ok(());
        }
        Err(VendorError::Other(failure(CONNECTION_EXCEPTION, "Not connected")))
    }
}

pub(super) fn uninitialized(field: &str) -> VendorError {
    VendorError::Other(failure(
        "kotlin.UninitializedPropertyAccessException",
        format!("lateinit property {field} has not been initialized"),
    ))
}
