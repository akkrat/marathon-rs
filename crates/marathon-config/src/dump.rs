//! Port of tools/oracle/module/src/main/kotlin/oracle/Canonical.kt and Json.kt (the oracle's config-dump renderer,
//! docs/design/config-dump-format.md), over the classes of configuration/src/main/kotlin/com/malinskiy/marathon/config.
//!
//! The canonical configuration renders every JVM field of the parsed object graph, not Jackson's view of it: the
//! `@JsonIgnore`d bundle results (`app`, `testApp`, `xctest`), derived defaults and the CLI-merged tracking flags
//! appear; the `tempDirFor` lambda does not. Keys are the Kotlin property names, sorted by UTF-16 code units (the
//! oracle's `TreeMap`); a polymorphic object gets `type`, the first name its supertype's `@JsonSubTypes` lists. No
//! configuration class has a `by lazy` property, so the `{"lazy": "uninitialized"}` form never arises.

use crate::{
    AnalyticsConfiguration, BatchingStrategyConfiguration, CiConfiguration, Configuration, Defaults,
    FilteringConfiguration, FlakinessStrategyConfiguration, PoolingStrategyConfiguration, RegexValuesFileFilter,
    RetryStrategyConfiguration, ShardingStrategyConfiguration, SortingStrategyConfiguration, TestFilterConfiguration,
    TestPackageFilter, VendorConfiguration, render::Names, vendor::*,
};
use jcompat::{
    JavaRegex,
    file_manager::{JavaPath, absolute_file_path_utf16},
    jnum::double_to_string,
    jtime::{Duration, Instant},
    writers::JavaString,
};

/// A JSON value as the oracle's `Json` writer sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum DumpValue {
    Null,
    Bool(bool),
    Int(i64),
    /// A number literal written verbatim (a `Double.toString` spelling).
    Number(String),
    Str(JavaString),
    Array(Vec<DumpValue>),
    /// Entries in iteration order; see [`DumpValue::sorted`].
    Object(Vec<(JavaString, DumpValue)>),
}

impl DumpValue {
    pub fn str(s: &str) -> Self {
        Self::Str(s.into())
    }

    /// An object whose entries are in insertion order (the oracle's `linkedMapOf`).
    pub fn object(entries: Vec<(&str, DumpValue)>) -> Self {
        Self::Object(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// An object with its keys sorted as Java's `String.compareTo` sorts them: by UTF-16 code units.
    pub fn sorted(mut entries: Vec<(JavaString, DumpValue)>) -> Self {
        entries.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        Self::Object(entries)
    }

    /// `Json.pretty`: two-space indentation, `": "`, empty containers as `[]`/`{}`, and a final newline. Strings are
    /// UTF-8 except for `"`, `\`, control characters, DEL and unpaired surrogates, which are escaped.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Int(n) => out.push_str(&n.to_string()),
            Self::Number(text) => out.push_str(text),
            Self::Str(s) => write_string(out, &s.0),
            Self::Array(items) => container(out, '[', ']', items, depth, |out, item, depth| item.write(out, depth)),
            Self::Object(entries) => container(out, '{', '}', entries, depth, |out, (key, value), depth| {
                write_string(out, &key.0);
                out.push_str(": ");
                value.write(out, depth);
            }),
        }
    }
}

fn container<T>(
    out: &mut String,
    open: char,
    close: char,
    items: &[T],
    depth: usize,
    item: impl Fn(&mut String, &T, usize),
) {
    out.push(open);
    if items.is_empty() {
        out.push(close);
        return;
    }
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('\n');
        out.push_str(&"  ".repeat(depth + 1));
        item(out, it, depth + 1);
    }
    out.push('\n');
    out.push_str(&"  ".repeat(depth));
    out.push(close);
}

fn write_string(out: &mut String, units: &[u16]) {
    out.push('"');
    for c in char::decode_utf16(units.iter().copied()) {
        match c {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok('\n') => out.push_str("\\n"),
            Ok('\r') => out.push_str("\\r"),
            Ok('\t') => out.push_str("\\t"),
            Ok('\u{8}') => out.push_str("\\b"),
            Ok('\u{c}') => out.push_str("\\f"),
            Ok(c) if c < ' ' || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            Ok(c) => out.push(c),
            Err(lone) => out.push_str(&format!("\\u{:04x}", lone.unpaired_surrogate())),
        }
    }
    out.push('"');
}

/// The canonical configuration (`Canonical.render(configuration)`). `process_dir` is the JVM's `user.dir`, against
/// which `File.absolutePath` resolves a relative path.
pub fn canonical_configuration(configuration: &Configuration, process_dir: &JavaPath) -> DumpValue {
    Canonical { cwd: process_dir }.configuration(configuration)
}

/// `class: message` of a throwable's causes as the oracle's `errorJson` writes them (a null message reads `null`).
pub fn error_json(
    class: &JavaString,
    message: Option<&JavaString>,
    causes: &[(JavaString, Option<JavaString>)],
) -> DumpValue {
    let causes = causes
        .iter()
        .map(|(class, message)| {
            let mut line = class.clone();
            line.push_str(": ");
            match message {
                Some(m) => line.append(m.clone()),
                None => line.push_str("null"),
            }
            DumpValue::Str(line)
        })
        .collect();
    DumpValue::object(vec![
        ("class", DumpValue::Str(class.clone())),
        (
            "message",
            message.map_or(DumpValue::Null, |m| DumpValue::Str(m.clone())),
        ),
        ("causes", DumpValue::Array(causes)),
    ])
}

struct Canonical<'a> {
    cwd: &'a JavaPath,
}

type Fields = Vec<(&'static str, DumpValue)>;

fn obj(fields: Fields) -> DumpValue {
    DumpValue::sorted(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
/// A polymorphic object: its fields plus the `@JsonSubTypes` name.
fn typed(name: &str, mut fields: Fields) -> DumpValue {
    fields.push(("type", DumpValue::str(name)));
    obj(fields)
}
fn text(s: &JavaString) -> DumpValue {
    DumpValue::Str(s.clone())
}
fn int(n: impl Into<i64>) -> DumpValue {
    DumpValue::Int(n.into())
}
fn boolean(b: bool) -> DumpValue {
    DumpValue::Bool(b)
}
/// `jsonDouble`: a JSON number spelled by `Double.toString` when finite, a string otherwise.
fn double(d: f64) -> DumpValue {
    if d.is_finite() {
        DumpValue::Number(double_to_string(d))
    } else {
        DumpValue::str(&double_to_string(d))
    }
}
fn duration(d: &Duration) -> DumpValue {
    DumpValue::str(&d.to_string())
}
fn instant(i: &Instant) -> DumpValue {
    DumpValue::str(&i.to_string())
}
fn regex(r: &JavaRegex) -> DumpValue {
    DumpValue::str(r.pattern())
}
fn name(e: impl Names) -> DumpValue {
    DumpValue::str(e.kotlin())
}
fn opt<T>(v: Option<&T>, f: impl FnOnce(&T) -> DumpValue) -> DumpValue {
    v.map_or(DumpValue::Null, f)
}
fn list<T>(v: &[T], f: impl FnMut(&T) -> DumpValue) -> DumpValue {
    DumpValue::Array(v.iter().map(f).collect())
}
fn string_map(v: &[(String, JavaString)]) -> DumpValue {
    DumpValue::sorted(v.iter().map(|(k, v)| (k.as_str().into(), text(v))).collect())
}

impl Canonical<'_> {
    /// `File.absolutePath`: absolute, not normalised.
    fn file(&self, p: &JavaPath) -> DumpValue {
        DumpValue::Str(JavaString(absolute_file_path_utf16(&p.0, &self.cwd.0).0))
    }
    fn files(&self, v: Option<&Vec<JavaPath>>) -> DumpValue {
        opt(v, |v| list(v, |p| self.file(p)))
    }

    fn configuration(&self, c: &Configuration) -> DumpValue {
        obj(vec![
            ("name", text(&c.name)),
            ("outputDir", self.file(&c.output_dir)),
            (
                "outputConfiguration",
                obj(vec![
                    ("maxPath", int(c.output_configuration.max_path)),
                    ("maxFilename", int(c.output_configuration.max_filename)),
                ]),
            ),
            ("analyticsConfiguration", analytics(&c.analytics_configuration)),
            ("poolingStrategy", pooling(&c.pooling_strategy)),
            ("shardingStrategy", sharding(&c.sharding_strategy)),
            ("sortingStrategy", sorting(&c.sorting_strategy)),
            ("batchingStrategy", batching(&c.batching_strategy)),
            ("flakinessStrategy", flakiness(&c.flakiness_strategy)),
            ("retryStrategy", retry(&c.retry_strategy)),
            ("filteringConfiguration", self.filtering(&c.filtering_configuration)),
            ("ignoreFailures", boolean(c.ignore_failures)),
            ("isCodeCoverageEnabled", boolean(c.is_code_coverage_enabled)),
            (
                "executionStrategy",
                obj(vec![
                    ("mode", name(c.execution_strategy.mode)),
                    ("fast", boolean(c.execution_strategy.fast)),
                ]),
            ),
            ("uncompletedTestRetryQuota", int(c.uncompleted_test_retry_quota)),
            ("includeSerialRegexes", list(&c.include_serial_regexes, regex)),
            ("excludeSerialRegexes", list(&c.exclude_serial_regexes, regex)),
            ("testBatchTimeoutMillis", int(c.test_batch_timeout_millis)),
            ("testOutputTimeoutMillis", int(c.test_output_timeout_millis)),
            ("debug", boolean(c.debug)),
            ("screenRecordingPolicy", name(c.screen_recording_policy)),
            ("vendorConfiguration", self.vendor(&c.vendor_configuration)),
            ("analyticsTracking", boolean(c.analytics_tracking)),
            ("bugsnagReporting", boolean(c.bugsnag_reporting)),
            (
                "deviceInitializationTimeoutMillis",
                int(c.device_initialization_timeout_millis),
            ),
            ("ciConfiguration", ci(&c.ci_configuration)),
        ])
    }

    fn filtering(&self, f: &FilteringConfiguration) -> DumpValue {
        obj(vec![
            ("allowlist", list(&f.allowlist, |f| self.filter(f))),
            ("blocklist", list(&f.blocklist, |f| self.filter(f))),
        ])
    }

    fn values_filter(&self, type_name: &str, f: &RegexValuesFileFilter) -> DumpValue {
        typed(
            type_name,
            vec![
                ("regex", opt(f.regex.as_ref(), regex)),
                ("values", opt(f.values.as_ref(), |v| list(v, text))),
                ("file", opt(f.file.as_ref(), |p| self.file(p))),
                ("enabled", boolean(f.enabled)),
            ],
        )
    }

    fn filter(&self, f: &TestFilterConfiguration) -> DumpValue {
        match f {
            TestFilterConfiguration::SimpleClassname(f) => self.values_filter("simple-class-name", f),
            TestFilterConfiguration::SimpleTestname(f) => self.values_filter("simple-test-name", f),
            TestFilterConfiguration::FullyQualifiedClassname(f) => self.values_filter("fully-qualified-class-name", f),
            TestFilterConfiguration::FullyQualifiedTestname(f) => self.values_filter("fully-qualified-test-name", f),
            TestFilterConfiguration::TestMethod(f) => self.values_filter("method", f),
            TestFilterConfiguration::Annotation(f) => self.values_filter("annotation", f),
            TestFilterConfiguration::TestPackage(TestPackageFilter {
                regex: r,
                values,
                file,
                subpackages,
                enabled,
            }) => typed(
                "package",
                vec![
                    ("regex", opt(r.as_ref(), regex)),
                    ("values", opt(values.as_ref(), |v| list(v, text))),
                    ("file", opt(file.as_ref(), |p| self.file(p))),
                    ("subpackages", boolean(*subpackages)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            TestFilterConfiguration::AnnotationData {
                name_regex,
                value_regex,
                enabled,
            } => typed(
                "annotationData",
                vec![
                    ("nameRegex", regex(name_regex)),
                    ("valueRegex", regex(value_regex)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            TestFilterConfiguration::Fragmentation { index, count, enabled } => typed(
                "fragmentation",
                vec![
                    ("index", int(*index)),
                    ("count", int(*count)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            TestFilterConfiguration::Composition { filters, op, enabled } => typed(
                "composition",
                vec![
                    ("filters", list(filters, |f| self.filter(f))),
                    ("op", name(*op)),
                    ("enabled", boolean(*enabled)),
                ],
            ),
            TestFilterConfiguration::Allure { enabled } => typed("allure", vec![("enabled", boolean(*enabled))]),
        }
    }

    fn vendor(&self, v: &VendorConfiguration) -> DumpValue {
        match v {
            VendorConfiguration::Android(c) => self.android(c),
            VendorConfiguration::Ios(c) => self.ios(c),
            VendorConfiguration::Macos(c) => self.macos(c),
            VendorConfiguration::Stub => typed("stub", vec![]),
            VendorConfiguration::Empty => typed("empty", vec![]),
        }
    }

    fn android(&self, c: &AndroidConfiguration) -> DumpValue {
        let video = &c.screen_record_configuration.video_configuration;
        let screenshot = &c.screen_record_configuration.screenshot_configuration;
        let timeouts = &c.timeout_configuration;
        typed(
            "Android",
            vec![
                ("androidSdk", opt(c.android_sdk.as_ref(), |p| self.file(p))),
                (
                    "applicationOutput",
                    opt(c.application_output.as_ref(), |p| self.file(p)),
                ),
                (
                    "testApplicationOutput",
                    opt(c.test_application_output.as_ref(), |p| self.file(p)),
                ),
                ("splitApks", self.files(c.split_apks.as_ref())),
                (
                    "extraApplicationsOutput",
                    self.files(c.extra_applications_output.as_ref()),
                ),
                (
                    "outputs",
                    opt(c.outputs.as_ref(), |v| {
                        list(v, |b| {
                            obj(vec![
                                ("application", opt(b.application.as_ref(), |p| self.file(p))),
                                ("testApplication", self.file(&b.test_application)),
                                ("extraApplications", self.files(b.extra_applications.as_ref())),
                                ("splitApks", self.files(b.split_apks.as_ref())),
                            ])
                        })
                    }),
                ),
                ("autoGrantPermission", boolean(c.auto_grant_permission)),
                ("instrumentationArgs", string_map(&c.instrumentation_args)),
                ("applicationPmClear", boolean(c.application_pm_clear)),
                ("testApplicationPmClear", boolean(c.test_application_pm_clear)),
                ("adbInitTimeoutMillis", int(c.adb_init_timeout_millis)),
                ("installOptions", text(&c.install_options)),
                ("serialStrategy", name(c.serial_strategy)),
                (
                    "screenRecordConfiguration",
                    obj(vec![
                        (
                            "preferableRecorderType",
                            opt(c.screen_record_configuration.preferable_recorder_type.as_ref(), |t| {
                                name(*t)
                            }),
                        ),
                        (
                            "videoConfiguration",
                            obj(vec![
                                ("enabled", boolean(video.enabled)),
                                ("width", int(video.width)),
                                ("height", int(video.height)),
                                ("bitrateMbps", int(video.bitrate_mbps)),
                                ("timeLimit", int(video.time_limit)),
                                ("timeLimitUnits", name(video.time_limit_units)),
                            ]),
                        ),
                        (
                            "screenshotConfiguration",
                            obj(vec![
                                ("enabled", boolean(screenshot.enabled)),
                                ("width", int(screenshot.width)),
                                ("height", int(screenshot.height)),
                                ("delayMs", int(screenshot.delay_ms)),
                            ]),
                        ),
                    ]),
                ),
                ("waitForDevicesTimeoutMillis", int(c.wait_for_devices_timeout_millis)),
                (
                    "allureConfiguration",
                    obj(vec![
                        ("enabled", boolean(c.allure_configuration.enabled)),
                        (
                            "relativeResultsDirectory",
                            text(&c.allure_configuration.relative_results_directory),
                        ),
                        ("pathRoot", name(c.allure_configuration.path_root)),
                    ]),
                ),
                (
                    "timeoutConfiguration",
                    obj(vec![
                        ("shell", duration(&timeouts.shell)),
                        ("listFiles", duration(&timeouts.list_files)),
                        ("pushFile", duration(&timeouts.push_file)),
                        ("pushFolder", duration(&timeouts.push_folder)),
                        ("pullFile", duration(&timeouts.pull_file)),
                        ("uninstall", duration(&timeouts.uninstall)),
                        ("install", duration(&timeouts.install)),
                        ("screenrecorder", duration(&timeouts.screenrecorder)),
                        ("screencapturer", duration(&timeouts.screencapturer)),
                        ("socketIdleTimeout", duration(&timeouts.socket_idle_timeout)),
                        ("portForward", duration(&timeouts.port_forward)),
                        ("boot", duration(&timeouts.boot)),
                    ]),
                ),
                (
                    "fileSyncConfiguration",
                    obj(vec![
                        (
                            "pull",
                            list(&c.file_sync_configuration.pull, |e| {
                                obj(vec![
                                    ("relativePath", text(&e.relative_path)),
                                    ("pathRoot", name(e.path_root)),
                                    ("aggregationMode", name(e.aggregation_mode)),
                                ])
                            }),
                        ),
                        (
                            "push",
                            list(&c.file_sync_configuration.push, |e| {
                                obj(vec![("path", text(&e.path)), ("pathRoot", name(e.path_root))])
                            }),
                        ),
                    ]),
                ),
                (
                    "threadingConfiguration",
                    obj(vec![
                        (
                            "bootWaitingThreads",
                            int(c.threading_configuration.boot_waiting_threads),
                        ),
                        ("adbIoThreads", int(c.threading_configuration.adb_io_threads)),
                        ("installThreads", int(c.threading_configuration.install_threads)),
                    ]),
                ),
                (
                    "testParserConfiguration",
                    match &c.test_parser_configuration {
                        AndroidTestParserConfiguration::Local => typed("local", vec![]),
                        AndroidTestParserConfiguration::Remote { instrumentation_args } => typed(
                            "remote",
                            vec![("instrumentationArgs", string_map(instrumentation_args))],
                        ),
                    },
                ),
                (
                    "testAccessConfiguration",
                    obj(vec![
                        ("adb", boolean(c.test_access_configuration.adb)),
                        ("grpc", boolean(c.test_access_configuration.grpc)),
                        ("console", boolean(c.test_access_configuration.console)),
                        ("consoleToken", text(&c.test_access_configuration.console_token)),
                    ]),
                ),
                (
                    "adbServers",
                    list(&c.adb_servers, |e| {
                        obj(vec![("host", text(&e.host)), ("port", int(e.port))])
                    }),
                ),
                ("disableWindowAnimation", boolean(c.disable_window_animation)),
                (
                    "profilingConfiguration",
                    obj(vec![
                        ("enabled", boolean(c.profiling_configuration.enabled)),
                        ("pbtxt", opt(c.profiling_configuration.pbtxt.as_ref(), |p| self.file(p))),
                        ("urlTemplate", text(&c.profiling_configuration.url_template)),
                    ]),
                ),
                ("mockLocation", boolean(c.mock_location)),
            ],
        )
    }

    fn ios(&self, c: &IosConfiguration) -> DumpValue {
        let video = &c.screen_record_configuration.video_configuration;
        let screenshot = &c.screen_record_configuration.screenshot_configuration;
        typed(
            "iOS",
            vec![
                ("bundle", opt(c.bundle.as_ref(), |b| self.bundle(b))),
                ("deviceProvider", self.device_provider(&c.device_provider)),
                ("devicesFile", opt(c.devices_file.as_ref(), |p| self.file(p))),
                ("ssh", self.ssh(&c.ssh)),
                (
                    "workerConnection",
                    obj(vec![
                        ("attempts", int(c.worker_connection.attempts)),
                        ("backoff", duration(&c.worker_connection.backoff)),
                        ("retryInterval", duration(&c.worker_connection.retry_interval)),
                        ("lateJoin", boolean(c.worker_connection.late_join)),
                        ("lateJoinCutoff", duration(&c.worker_connection.late_join_cutoff)),
                    ]),
                ),
                ("xcresult", xcresult(&c.xcresult)),
                (
                    "screenRecordConfiguration",
                    obj(vec![
                        (
                            "preferableRecorderType",
                            opt(c.screen_record_configuration.preferable_recorder_type.as_ref(), |t| {
                                name(*t)
                            }),
                        ),
                        (
                            "videoConfiguration",
                            obj(vec![
                                ("enabled", boolean(video.enabled)),
                                ("codec", name(video.codec)),
                                ("display", name(video.display)),
                                ("mask", name(video.mask)),
                                (
                                    "transcoding",
                                    obj(vec![
                                        ("enabled", boolean(video.transcoding.enabled)),
                                        ("binary", text(&video.transcoding.binary)),
                                        ("size", name(video.transcoding.size)),
                                    ]),
                                ),
                            ]),
                        ),
                        (
                            "screenshotConfiguration",
                            obj(vec![
                                ("enabled", boolean(screenshot.enabled)),
                                ("type", name(screenshot.screenshot_type)),
                                ("display", name(screenshot.display)),
                                ("mask", name(screenshot.mask)),
                                ("delay", duration(&screenshot.delay)),
                                ("width", int(screenshot.width)),
                                ("height", int(screenshot.height)),
                            ]),
                        ),
                    ]),
                ),
                ("xctestrunEnv", xctestrun_env(&c.xctestrun_env)),
                (
                    "lifecycleConfiguration",
                    obj(vec![
                        ("shutdownUnused", boolean(c.lifecycle_configuration.shutdown_unused)),
                        ("onPrepare", list(&c.lifecycle_configuration.on_prepare, |a| name(*a))),
                        ("onDispose", list(&c.lifecycle_configuration.on_dispose, |a| name(*a))),
                    ]),
                ),
                (
                    "permissions",
                    obj(vec![
                        ("grant", list(&c.permissions.grant, |p| name(*p))),
                        ("lifecycle", name(c.permissions.lifecycle)),
                    ]),
                ),
                ("mediaFiles", self.files(c.media_files.as_ref())),
                ("timeoutConfiguration", apple_timeouts(&c.timeout_configuration)),
                ("threadingConfiguration", apple_threading(&c.threading_configuration)),
                ("hideRunnerOutput", boolean(c.hide_runner_output)),
                ("compactOutput", boolean(c.compact_output)),
                ("rsync", obj(vec![("remotePath", text(&c.rsync.remote_path))])),
                ("xcodebuildTestArgs", string_map(&c.xcodebuild_test_args)),
                ("dataContainerClear", boolean(c.data_container_clear)),
                (
                    "testParserConfiguration",
                    apple_test_parser(&c.test_parser_configuration),
                ),
                ("deviceLog", boolean(c.device_log)),
                (
                    "signing",
                    obj(vec![(
                        "testRunnerProvisioningProfile",
                        opt(c.signing.test_runner_provisioning_profile.as_ref(), text),
                    )]),
                ),
                ("skipCopyingDerivedData", boolean(c.skip_copying_derived_data)),
                ("maxBatchesWithoutProgress", int(c.max_batches_without_progress)),
            ],
        )
    }

    fn macos(&self, c: &MacosConfiguration) -> DumpValue {
        typed(
            "macOS",
            vec![
                ("bundle", opt(c.bundle.as_ref(), |b| self.bundle(b))),
                ("deviceProvider", self.device_provider(&c.device_provider)),
                ("devicesFile", opt(c.devices_file.as_ref(), |p| self.file(p))),
                ("ssh", self.ssh(&c.ssh)),
                ("xcresult", xcresult(&c.xcresult)),
                ("xctestrunEnv", xctestrun_env(&c.xctestrun_env)),
                ("timeoutConfiguration", apple_timeouts(&c.timeout_configuration)),
                ("threadingConfiguration", apple_threading(&c.threading_configuration)),
                ("hideRunnerOutput", boolean(c.hide_runner_output)),
                ("compactOutput", boolean(c.compact_output)),
                ("rsync", obj(vec![("remotePath", text(&c.rsync.remote_path))])),
                ("xcodebuildTestArgs", string_map(&c.xcodebuild_test_args)),
                (
                    "testParserConfiguration",
                    apple_test_parser(&c.test_parser_configuration),
                ),
                (
                    "permissions",
                    obj(vec![(
                        "grant",
                        DumpValue::sorted(
                            c.permissions
                                .grant
                                .iter()
                                .map(|(k, v)| (k.as_str().into(), list(v, |p| name(*p))))
                                .collect(),
                        ),
                    )]),
                ),
            ],
        )
    }

    /// The constructor properties plus the `@JsonIgnore`d `app`/`testApp`/`xctest` that `validate()` sets; the
    /// `tempDirFor` lambda is behaviour, not configuration.
    fn bundle(&self, b: &AppleTestBundleConfiguration) -> DumpValue {
        obj(vec![
            ("application", opt(b.application.as_ref(), |p| self.file(p))),
            ("testApplication", opt(b.test_application.as_ref(), |p| self.file(p))),
            ("extraApplications", self.files(b.extra_applications.as_ref())),
            ("derivedDataDir", opt(b.derived_data_dir.as_ref(), |p| self.file(p))),
            ("testType", opt(b.test_type.as_ref(), |t| name(*t))),
            ("xctestrun", opt(b.xctestrun.as_ref(), |p| self.file(p))),
            ("app", opt(b.app.as_ref(), |p| self.file(p))),
            ("testApp", opt(b.test_app.as_ref(), |p| self.file(p))),
            ("xctest", opt(b.xctest.as_ref(), |p| self.file(p))),
        ])
    }

    fn device_provider(&self, p: &DeviceProvider) -> DumpValue {
        match p {
            DeviceProvider::Static { path } => typed("static", vec![("path", opt(path.as_ref(), |p| self.file(p)))]),
            DeviceProvider::Dynamic { host, port } => {
                typed("dynamic", vec![("host", text(host)), ("port", int(*port))])
            }
        }
    }

    fn ssh(&self, s: &SshConfiguration) -> DumpValue {
        let authentication = opt(s.authentication.as_ref(), |a| match a {
            SshAuthentication::Password { username, password } => typed(
                "password",
                vec![("username", text(username)), ("password", text(password))],
            ),
            SshAuthentication::PublicKey { username, key } => {
                typed("publicKey", vec![("username", text(username)), ("key", self.file(key))])
            }
        });
        obj(vec![
            ("authentication", authentication),
            ("knownHostsPath", opt(s.known_hosts_path.as_ref(), |p| self.file(p))),
            ("keepAliveInterval", duration(&s.keep_alive_interval)),
            ("keepAliveMaxCount", int(s.keep_alive_max_count)),
            ("debug", boolean(s.debug)),
            ("shareWorkerConnection", boolean(s.share_worker_connection)),
        ])
    }
}

fn defaults(d: &Defaults) -> DumpValue {
    obj(vec![
        ("successRate", double(d.success_rate)),
        ("duration", duration(&d.duration)),
        (
            "observedDurationMinTests",
            opt(d.observed_duration_min_tests.as_ref(), |n| int(*n)),
        ),
    ])
}

fn analytics(a: &AnalyticsConfiguration) -> DumpValue {
    match a {
        AnalyticsConfiguration::Disabled => typed("disabled", vec![]),
        AnalyticsConfiguration::InfluxDb(c) => {
            let rp = &c.retention_policy_configuration;
            typed(
                "influxdb",
                vec![
                    ("url", text(&c.url)),
                    ("user", text(&c.user)),
                    ("password", text(&c.password)),
                    ("dbName", text(&c.db_name)),
                    (
                        "retentionPolicyConfiguration",
                        obj(vec![
                            ("name", text(&rp.name)),
                            ("duration", text(&rp.duration)),
                            ("shardDuration", text(&rp.shard_duration)),
                            ("replicationFactor", int(rp.replication_factor)),
                            ("isDefault", boolean(rp.is_default)),
                        ]),
                    ),
                    ("defaults", defaults(&c.defaults)),
                    ("readOnly", boolean(c.read_only)),
                ],
            )
        }
        AnalyticsConfiguration::InfluxDb2(c) => typed(
            "influxdb2",
            vec![
                ("url", text(&c.url)),
                ("token", text(&c.token)),
                ("organization", text(&c.organization)),
                ("bucket", text(&c.bucket)),
                (
                    "retentionPolicyConfiguration",
                    obj(vec![
                        ("everySeconds", int(c.retention_policy_configuration.every_seconds)),
                        (
                            "shardGroupDurationSeconds",
                            int(c.retention_policy_configuration.shard_group_duration_seconds),
                        ),
                    ]),
                ),
                ("defaults", defaults(&c.defaults)),
                ("readOnly", boolean(c.read_only)),
            ],
        ),
        AnalyticsConfiguration::Graphite(c) => typed(
            "graphite",
            vec![
                ("host", text(&c.host)),
                ("port", opt(c.port.as_ref(), |n| int(*n))),
                ("prefix", opt(c.prefix.as_ref(), text)),
                ("defaults", defaults(&c.defaults)),
                ("readOnly", boolean(c.read_only)),
            ],
        ),
    }
}

fn pooling(p: &PoolingStrategyConfiguration) -> DumpValue {
    match p {
        PoolingStrategyConfiguration::Omni => typed("omni", vec![]),
        PoolingStrategyConfiguration::Abi => typed("abi", vec![]),
        PoolingStrategyConfiguration::Manufacturer => typed("manufacturer", vec![]),
        PoolingStrategyConfiguration::Model => typed("device-model", vec![]),
        PoolingStrategyConfiguration::OperatingSystemVersion => typed("os-version", vec![]),
        PoolingStrategyConfiguration::Combo(items) => typed("combo", vec![("list", list(items, pooling))]),
    }
}

fn sharding(s: &ShardingStrategyConfiguration) -> DumpValue {
    match s {
        ShardingStrategyConfiguration::Count { count } => typed("count", vec![("count", int(*count))]),
        ShardingStrategyConfiguration::Parallel => typed("parallel", vec![]),
    }
}

fn sorting(s: &SortingStrategyConfiguration) -> DumpValue {
    match s {
        SortingStrategyConfiguration::ExecutionTime { percentile, time_limit } => typed(
            "execution-time",
            vec![("percentile", double(*percentile)), ("timeLimit", instant(time_limit))],
        ),
        SortingStrategyConfiguration::NoSorting => typed("no-sorting", vec![]),
        SortingStrategyConfiguration::RandomOrder => typed("random-order", vec![]),
        SortingStrategyConfiguration::SuccessRate { time_limit, ascending } => typed(
            "success-rate",
            vec![("timeLimit", instant(time_limit)), ("ascending", boolean(*ascending))],
        ),
    }
}

fn batching(b: &BatchingStrategyConfiguration) -> DumpValue {
    match b {
        BatchingStrategyConfiguration::FixedSize {
            size,
            duration_millis,
            percentile,
            time_limit,
            last_mile_length,
        } => typed(
            "fixed-size",
            vec![
                ("size", int(*size)),
                ("durationMillis", opt(duration_millis.as_ref(), |n| int(*n))),
                ("percentile", opt(percentile.as_ref(), |p| double(*p))),
                ("timeLimit", opt(time_limit.as_ref(), instant)),
                ("lastMileLength", int(*last_mile_length)),
            ],
        ),
        BatchingStrategyConfiguration::ClassName => typed("class-name", vec![]),
        BatchingStrategyConfiguration::Isolate => typed("isolate", vec![]),
    }
}

fn flakiness(f: &FlakinessStrategyConfiguration) -> DumpValue {
    match f {
        FlakinessStrategyConfiguration::Ignore => typed("ignore", vec![]),
        FlakinessStrategyConfiguration::Probability {
            min_success_rate,
            max_count,
            time_limit,
        } => typed(
            "probability",
            vec![
                ("minSuccessRate", double(*min_success_rate)),
                ("maxCount", int(*max_count)),
                ("timeLimit", instant(time_limit)),
            ],
        ),
    }
}

fn retry(r: &RetryStrategyConfiguration) -> DumpValue {
    match r {
        RetryStrategyConfiguration::FixedQuota {
            total_allowed_retry_quota,
            retry_per_test_quota,
        } => typed(
            "fixed-quota",
            vec![
                ("totalAllowedRetryQuota", int(*total_allowed_retry_quota)),
                ("retryPerTestQuota", int(*retry_per_test_quota)),
            ],
        ),
        RetryStrategyConfiguration::NoRetry => typed("no-retry", vec![]),
    }
}

fn ci(c: &CiConfiguration) -> DumpValue {
    typed(
        match c {
            CiConfiguration::Auto => "auto",
            CiConfiguration::None => "none",
            CiConfiguration::Teamcity => "teamcity",
        },
        vec![],
    )
}

fn xcresult(x: &XcresultConfiguration) -> DumpValue {
    obj(vec![
        ("pull", opt(x.pull.as_ref(), |b| boolean(*b))),
        ("pullingPolicy", name(x.pulling_policy)),
        ("remoteClean", boolean(x.remote_clean)),
        (
            "attachments",
            obj(vec![
                (
                    "systemAttachmentLifetime",
                    name(x.attachments.system_attachment_lifetime),
                ),
                ("userAttachmentLifetime", name(x.attachments.user_attachment_lifetime)),
            ]),
        ),
        (
            "preferredScreenCaptureFormat",
            opt(x.preferred_screen_capture_format.as_ref(), |f| name(*f)),
        ),
        (
            "queue",
            obj(vec![
                ("capacity", int(x.queue.capacity)),
                ("concurrency", int(x.queue.concurrency)),
                ("workerCapacity", opt(x.queue.worker_capacity.as_ref(), |n| int(*n))),
            ]),
        ),
    ])
}

fn xctestrun_env(x: &XctestrunEnvConfiguration) -> DumpValue {
    obj(vec![
        ("appEnvs", string_map(&x.app_envs)),
        ("testEnvs", string_map(&x.test_envs)),
    ])
}

fn apple_timeouts(t: &AppleTimeoutConfiguration) -> DumpValue {
    obj(vec![
        ("shell", duration(&t.shell)),
        ("shellIdle", duration(&t.shell_idle)),
        ("reachability", duration(&t.reachability)),
        ("screenshot", duration(&t.screenshot)),
        ("video", duration(&t.video)),
        ("erase", duration(&t.erase)),
        ("shutdown", duration(&t.shutdown)),
        ("delete", duration(&t.delete)),
        ("create", duration(&t.create)),
        ("boot", duration(&t.boot)),
        ("install", duration(&t.install)),
        ("uninstall", duration(&t.uninstall)),
        ("importMedia", duration(&t.import_media)),
        ("testDestination", duration(&t.test_destination)),
        ("xcresulttool", duration(&t.xcresulttool)),
        ("bootWait", duration(&t.boot_wait)),
    ])
}

fn apple_threading(t: &AppleThreadingConfiguration) -> DumpValue {
    obj(vec![
        ("deviceProviderThreads", int(t.device_provider_threads)),
        ("deviceThreads", int(t.device_threads)),
        ("provisioningConcurrency", int(t.provisioning_concurrency)),
    ])
}

fn apple_test_parser(p: &AppleTestParserConfiguration) -> DumpValue {
    match p {
        AppleTestParserConfiguration::Nm { test_class_regexes } => typed(
            "nm",
            vec![(
                "testClassRegexes",
                list(test_class_regexes, |r| DumpValue::str(r.pattern())),
            )],
        ),
        AppleTestParserConfiguration::Xctest => typed("xctest", vec![]),
    }
}
