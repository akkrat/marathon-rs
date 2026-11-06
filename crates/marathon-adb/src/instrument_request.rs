//! Port of adam/src/main/kotlin/com/malinskiy/adam/request/testrunner/InstrumentOptions.kt,
//! InstrumentationSizeOption.kt, TestRunnerRequest.kt (adam 0.5.8), and
//! android-testrunner-contract/src/main/java/com/malinskiy/adam/android/contract/TestRunnerContract.java.
//! Copyright (C) 2019-2021 Anton Malinskiy; licensed under the Apache License, Version 2.0.

use crate::{
    client::{
        AdbError, Feature,
        shell::{compat::FallibleAsyncCompatShellCommandRequest, v2::ShellCommandResultChunk},
    },
    instrumentation::{InstrumentationError, InstrumentationResponseTransformer, InstrumentationWarning},
    testrunner::{MetricMap, TestEvent},
};
use jcompat::writers::JavaString;
use std::collections::VecDeque;

pub const GRPC_PORT: &str = "com.malinskiy.adam.android.GRPC_PORT";
pub const GRPC_HOST: &str = "com.malinskiy.adam.android.GRPC_HOST";
pub const ADB_PORT: &str = "com.malinskiy.adam.android.ADB_PORT";
pub const ADB_HOST: &str = "com.malinskiy.adam.android.ADB_HOST";
pub const CONSOLE_PORT: &str = "com.malinskiy.adam.android.CONSOLE_PORT";
pub const CONSOLE_HOST: &str = "com.malinskiy.adam.android.CONSOLE_HOST";
pub const AUTH_TOKEN: &str = "com.malinskiy.adam.android.AUTH_TOKEN";
pub const ADB_SERIAL: &str = "com.malinskiy.adam.android.ADB_SERIAL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentationSizeOption {
    Small,
    Medium,
    Large,
}
impl InstrumentationSizeOption {
    fn value(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentOptions {
    pub pkg: Vec<JavaString>,
    pub clazz: Vec<JavaString>,
    pub functional: Option<bool>,
    pub unit: Option<bool>,
    pub filter_size: Option<InstrumentationSizeOption>,
    pub performance: Option<bool>,
    pub debug: Option<bool>,
    pub log: Option<bool>,
    pub emma: Option<bool>,
    pub coverage_file: Option<JavaString>,
    pub overrides: MetricMap,
}
impl InstrumentOptions {
    pub fn to_java_string(&self) -> JavaString {
        let mut result = JavaString::default();
        fn add(out: &mut JavaString, key: &str, value: JavaString) {
            out.push_str(" -e ");
            out.push_str(key);
            out.push_str(" ");
            out.append(value);
        }
        for (key, list) in [("package", &self.pkg), ("class", &self.clazz)] {
            if !list.is_empty() {
                let mut value = JavaString::default();
                for (i, item) in list.iter().enumerate() {
                    if i > 0 {
                        value.push_str(",");
                    }
                    value.append(item.clone());
                }
                add(&mut result, key, value);
            }
        }
        for (key, value) in [("func", self.functional), ("unit", self.unit)] {
            if let Some(value) = value {
                add(&mut result, key, if value { "true" } else { "false" }.into());
            }
        }
        if let Some(size) = self.filter_size {
            add(&mut result, "size", size.value().into());
        }
        for (key, value) in [
            ("perf", self.performance),
            ("debug", self.debug),
            ("log", self.log),
            ("emma", self.emma),
        ] {
            if let Some(value) = value {
                add(&mut result, key, if value { "true" } else { "false" }.into());
            }
        }
        if let Some(file) = &self.coverage_file {
            add(&mut result, "coverageFile", file.clone());
        }
        for (key, value) in self.overrides.entries() {
            result.push_str(" -e ");
            result.append(key.clone());
            result.push_str(" ");
            result.append(value.clone());
        }
        result
    }
}
#[derive(Clone, Debug)]
pub struct TestRunnerRequest {
    pub test_package: JavaString,
    pub instrument_options: InstrumentOptions,
    pub supported_features: Vec<Feature>,
    pub runner_class: JavaString,
    pub no_hidden_api_checks: bool,
    pub no_window_animations: bool,
    pub no_isolated_storage: bool,
    pub user_id: Option<i32>,
    pub abi: Option<JavaString>,
    pub profiling_output_path: Option<JavaString>,
    pub output_log_path: Option<JavaString>,
    pub socket_idle_timeout: Option<i64>,
    transformer: InstrumentationResponseTransformer,
    exit_code: Option<i32>,
    ended_without_result: bool,
}
impl TestRunnerRequest {
    pub fn new(
        test_package: JavaString,
        instrument_options: InstrumentOptions,
        supported_features: Vec<Feature>,
    ) -> Self {
        Self {
            test_package,
            instrument_options,
            supported_features,
            runner_class: "android.support.test.runner.AndroidJUnitRunner".into(),
            no_hidden_api_checks: false,
            no_window_animations: false,
            no_isolated_storage: false,
            user_id: None,
            abi: None,
            profiling_output_path: None,
            output_log_path: None,
            socket_idle_timeout: Some(i64::MAX),
            transformer: InstrumentationResponseTransformer::new(),
            exit_code: None,
            ended_without_result: false,
        }
    }
    pub fn command(&self) -> JavaString {
        let mut s = JavaString::from("am instrument -w -r");
        for (enabled, flag) in [
            (self.no_hidden_api_checks, " --no-hidden-api-checks"),
            (self.no_window_animations, " --no-window-animation"),
            (self.no_isolated_storage, " --no-isolated-storage"),
        ] {
            if enabled {
                s.push_str(flag);
            }
        }
        if let Some(user) = self.user_id {
            s.push_str(" --user ");
            s.push_str(&user.to_string());
        }
        for (flag, value) in [
            (" --abi ", &self.abi),
            (" -p ", &self.profiling_output_path),
            (" -f ", &self.output_log_path),
        ] {
            if let Some(value) = value {
                s.push_str(flag);
                s.append(value.clone());
            }
        }
        s.append(self.instrument_options.to_java_string());
        s.push_str(" ");
        s.append(self.test_package.clone());
        s.push_str("/");
        s.append(self.runner_class.clone());
        s
    }
    pub fn take_warnings(&mut self) -> Vec<InstrumentationWarning> {
        self.transformer.take_warnings()
    }
    /// D25: the stream closed before the instrumentation's end, so `close` gave no final events. Those would describe
    /// the output so far as the whole of it, and for a run with every test reported, as a success.
    pub fn ended_without_result(&self) -> bool {
        self.ended_without_result
    }
    /// The exit code the shell reported; a v1 shell and a dropped connection report none.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
}
impl FallibleAsyncCompatShellCommandRequest for TestRunnerRequest {
    type Item = Vec<TestEvent>;
    fn cmd(&self) -> String {
        String::from_utf8_lossy(&self.command().to_utf8_bytes()).into_owned()
    }
    fn supported_features(&self) -> &[Feature] {
        &self.supported_features
    }
    fn socket_idle_timeout(&self) -> Option<i64> {
        self.socket_idle_timeout
    }
    fn convert_chunk(&mut self, chunk: ShellCommandResultChunk) -> Result<Option<Self::Item>, AdbError> {
        // The shell's stream carries it as one unsigned byte, which adam reads signed.
        self.exit_code = chunk.exit_code.map(|code| code & 0xff).or(self.exit_code);
        chunk
            .stdout
            .map(|bytes| {
                self.transformer.process(&bytes).map_err(|error| match error {
                    InstrumentationError::InvalidNumTests(value) => {
                        AdbError::NumberFormat(format!("For input string: \"{}\"", value.to_string_lossy()))
                    }
                    InstrumentationError::MalformedAtom(value) => AdbError::StringIndexOutOfBounds(format!(
                        "Range [0, -1) out of bounds for length {}",
                        value.0.len()
                    )),
                })
            })
            .transpose()
            .map(Option::flatten)
    }
    async fn close(&mut self, send: &mut VecDeque<Self::Item>) {
        self.ended_without_result = self.transformer.ended_without_result();
        if !self.ended_without_result {
            send.extend(self.transformer.transform());
        }
    }
}
