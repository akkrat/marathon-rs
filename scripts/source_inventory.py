#!/usr/bin/env python3
"""Map every main Kotlin source file of the reference marathon checkout to the work package (PLAN.md §7)
that ports it. Writes docs/source-inventory.csv and exits non-zero if a file has no owner.

Usage: scripts/source_inventory.py [--check] /path/to/marathon

With --check it regenerates the CSV in memory, fails if docs/source-inventory.csv differs or a file is
unowned, and writes nothing.
"""
import csv
import io
import pathlib
import re
import sys

# Ordered (regex on the repo-relative path, wp, note). The first match wins.
RULES = [
    # ---- build system and samples --------------------------------------------------------------------
    (r"^buildSrc/.*/buildsystem/", "4.6", "xcresulttool codegen generator: ported as a checked-in Rust codegen"),
    (r"^buildSrc/", "n/a", "Gradle build logic; replaced by Cargo and the 7.3 packaging"),
    (r"^sample/", "n/a", "sample apps and their build logic; used as E2E fixtures, not ported"),
    # ---- not ported -------------------------------------------------------------------------------
    (r"^analytics/usage/", "n/a", "D1: usage analytics are not sent; flags and config keys stay (2.5)"),
    (r"core/.*/exceptions/(Bugsnag|Noop)?ExceptionsReporter(Factory)?\.kt$", "n/a", "D1: bugsnag is not sent"),
    (r"/apple/cmd/remote/scp/ScpFileBridge\.kt$", "n/a", "dead code: never referenced"),
    (r"/apple/logparser/parser/DeviceFailureException\.kt$", "4.3", "exception type the 4.3 transient-failure classifier needs; 4.7 reuses it"),
    (r"/apple/ios/extensions/ByteBuffer\.kt$", "n/a", "JDK 8 ByteBuffer shim; nothing to port"),
    # ---- cli ----------------------------------------------------------------------------------------
    (r"^cli/", "2.5", ""),
    # ---- configuration --------------------------------------------------------------------------------
    (r"config/serialization/yaml/(ExecutionTime|FixedSize|ProbabilityBased|SuccessRate)", "2.2", "per-field timeLimit"),
    (r"config/serialization/yaml/(InfluxDb|Graphite|RetentionPolicy|Analytics)", "2.2", "lenient analytics deserializers"),
    (r"config/serialization/(MutableConfigurationFactory|yaml/FileListProvider)\.kt$", "n/a", "P7 approved: no callers at e47352f2; reviewer agreement pending WP 2.1"),
    (r"config/serialization/yaml/RegexSerializer\.kt$", "2.4", "leaf serialization and round-trip tests, WP 2.1 orchestrator decision"),
    (r"config/serialization/", "2.1", "Jackson binding, SerializeModule, File/Regex/time deserializers"),
    (r"config/environment/", "2.3a", "ANDROID_HOME / ANDROID_SDK_ROOT lookup"),
    (r"config/vendor/apple/AppleTestBundleConfiguration\.kt$", "2.3b", "bundle validation and extraction"),
    (r"config/vendor/", "2.3a", ""),
    (r"/apple/extensions/ConfigurationExtensions\.kt$", "2.3a", "accessors over the Apple vendor config types"),
    (r"config/(LogicalConfigurationValidator|ConfigurationValidator)\.kt$", "2.4", ""),
    (r"config/exceptions/", "2.4", ""),
    (r"^configuration/.*config/Configuration\.kt$", "2.2", "toMap() goes to 2.4"),
    (r"^configuration/.*MutableConfigurationFactory\.kt$", "2.2", "only for external consumers; port if Gradle plugin parity is needed"),
    (r"^configuration/", "2.2", ""),
    # ---- core: runtime ---------------------------------------------------------------------------------
    (r"core/.*/marathon/Marathon\.kt$", "3.10", "run flow, onFinish, core Timings lines"),
    (r"core/.*/marathon/ShutdownHook\.kt$", "3.10", ""),
    (r"core/.*/marathon/Vendor\.kt$", "0.6", ""),
    (r"core/.*/di/Modules\.kt$", "3.10", "explicit construction instead of Koin"),
    (r"core/.*/actor/", "3.1", ""),
    (r"core/.*/coroutines/", "3.1", ""),
    (r"core/.*/execution/device/", "3.6", "DeviceActor and its FSM"),
    (r"core/.*/execution/queue/TestAction\.kt$", "3.2", "the retry strategy interface returns it; 3.7 consumes it"),
    (r"core/.*/execution/queue/Test(State|Event)\.kt$", "3.5", "progress FSM declarations; 3.7 consumes them"),
    (r"core/.*/execution/queue/", "3.7", "QueueActor, retry placement on another worker"),
    (r"core/.*/execution/DevicePoolMessage\.kt$", "3.6", "messages DeviceActor sends to the pool; 3.8 consumes them"),
    (r"core/.*/execution/(DevicePoolActor|Scheduler)\.kt$", "3.8", ""),
    (r"core/.*/execution/progress/ProgressReporter\.kt$", "n/a", "WP 3.5 P1 approved: no constructor/call site at e47352f2; reviewer agreement pending"),
    (r"core/.*/execution/progress/", "3.5", ""),
    (r"core/.*/execution/result/", "3.5", ""),
    (r"core/.*/integrations/ci/Teamcity\.kt$", "3.5", "WP 3.5 P1 approved: private setKeyValue is n/a (dead helper); reviewer agreement pending"),
    (r"core/.*/integrations/ci/", "3.5", ""),
    (r"core/.*/execution/strategy/", "3.2", ""),
    (r"core/.*/execution/filter/", "3.3", ""),
    (r"core/.*/execution/command/", "3.10", "parse command; YAML output via 3.9b"),
    (r"core/.*/execution/listener/", "3.9a", "LineListener, LogListener, TestRunListener"),
    (r"core/.*/execution/Retry\.kt$", "3.1", "async withRetry; withRetrySync belongs to 4.2 (P4)"),
    (r"core/.*/execution/", "0.6", "model types and traits"),
    (r"core/.*/report/(device|test)/", "3.9b", "concrete JSON reporters; model contracts frozen in 0.6"),
    (r"core/.*/test/Test\.kt$", "0.6", "model types and traits; the name helpers (toClassName and friends) are 3.2's"),
    (r"core/.*/(test|device|config|time)/[^/]+\.kt$", "0.6", "model types and traits"),
    (r"core/.*/device/screenshot/", "3.9c", "ScreenCapturer, GifSequenceWriter"),
    (r"core/.*/device/file/", "3.9a", "transfer timing/logging; interfaces frozen in 0.6"),
    (r"core/.*/exceptions/", "0.6", "VendorError taxonomy"),
    (r"core/.*/log/", "1.6", ""),
    (r"core/.*/(io|json)/", "1.7", "FileManager rules (3.9a uses them)"),
    (r"core/.*/extension/StringExtensions\.kt$", "1.7", "escape()"),
    (r"core/.*/extension/FileExtensions\.kt$", "3.9c", "relativePathTo; called only by the HTML/Allure reporters (WP 3.10 decision 5)"),
    (r"core/.*/extension/ConfigurationExtensions\.kt$", "3.2", "strategy factories; its toTestFilter part is 3.3's"),
    (r"core/.*/extension/", "3.10", "strategy/factory extensions"),
    # ---- core: analytics and reports -----------------------------------------------------------------
    (r"core/.*/analytics/metrics/remote/graphite/QueryableGraphiteClient\.kt$", "3.4b", "HTTP query transport; WP 3.4a decision 2"),
    (r"core/.*/analytics/metrics/", "3.4a", ""),
    (r"core/.*/analytics/external/(influx|influx2|graphite)/", "3.4b", ""),
    (r"core/.*/analytics/external/", "3.4a", "MetricsProviderImpl, coverage, NoOp"),
    (r"core/.*/analytics/", "3.9a", "Track, MappingTracker, TrackerFactory, ExecutionReportGenerator"),
    (r"core/.*/report/(allure|html|timeline)/", "3.9c", ""),
    (r"core/.*/report/(logs|attachment)/", "3.9a", ""),
    (r"core/.*/report/Reporter\.kt$", "3.9a", "ExecutionReportGenerator needs it; 3.9b implements it"),
    (r"core/.*/report/", "3.9b", ""),
    (r"^report/", "3.9c", "HTML and timeline models; assets embedded"),
    # ---- apple base ------------------------------------------------------------------------------------
    (r"base/src/main/kotlin/sshj/", "4.2", "PatchedExecChannel behaviours"),
    (r"/apple/sshj/", "4.2", "PatchedSSHClient"),
    (r"/apple/cmd/remote/ssh/", "4.2", ""),
    (r"/apple/cmd/remote/rsync/RsyncFileBridge\.kt$", "4.3", "P1 approved: unused hostnameLocksMap and isDescendantOf omitted; live bridge ported"),
    (r"/apple/cmd/remote/rsync/", "4.3", ""),
    (r"/apple/cmd/(WriteFile|SharedTransfers)\.kt$", "4.3", ""),
    (r"/apple/cmd/", "4.1", ""),
    (r"/apple/device/ConnectionCounter\.kt$", "n/a", "WP 4.3 P1 approved: no callers at e47352f2; orchestrator decision 2026-10-25, agreed by both review-round-1 reviewers"),
    (r"/apple/device/BatchFailurePolicy\.kt$", "5.2", ""),
    (r"/apple/device/", "4.3", "ConnectionFactory, ConnectionFailures; ConnectionCounter is n/a under P1"),
    (r"/apple/extensions/", "4.1", ""),
    (r"/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory\.kt$", "4.6", "legacy _value/_values unwrapping"),
    (r"/apple/bin/xcrun/xcresulttool/Xcresulttool\.kt$", "4.4", "command wrapper; needs the 4.6 schema types"),
    (r"/apple/bin/", "4.4", ""),
    (r"/apple/model/", "4.4", ""),
    (r"/apple/xctestrun/TestRootFactory\.kt$", "4.5b", ""),
    # WP 4.5a P3: dead BundleInfo facets, unreachable from BundleInfo and constructed nowhere.
    (r"/apple/plist/bundle/(Help|Launch|LaunchConditions|Localization|Version)\.kt$", "n/a",
     "P3 (WP 4.5a): unreachable from BundleInfo and constructed nowhere at e47352f2; both WP 4.5a reviewers agreed (review round 1)"),
    (r"/apple/(plist|xctestrun)/", "4.5a", ""),
    (r"/apple/logparser/", "4.7", ""),
    # WP 4.4 decision 2: the xcodebuild builder needs TestRequest; 4.7 and 4.8 reuse it.
    (r"/apple/test/TestRequest\.kt$", "4.4", "xcodebuild test filter (WP 4.4 decision 2)"),
    (r"/apple/test/", "4.7", "TestEvent"),
    (r"/apple/listener/ResultBundleRunListener\.kt$", "4.9", ""),
    (r"/apple/listener/", "4.7", ""),
    (r"/apple/ArtifactProcessor\.kt$", "4.9", ""),
    (r"/apple/(AppleLogConfigurator|XcodebuildStderrLog)\.kt$", "1.6", ""),
    (r"/apple/(AppleDeviceTestRunner|XcodebuildSession|NmTestParser|XCTestParser|AppleTestBundleIdentifier)\.kt$", "4.8", ""),
    (r"/apple/DistributedDerivedData\.kt$", "4.10b", "requireDistributed (:11-20) ported in 4.5b; requireDistributedTestBundle ported in 4.10b (orchestrator decision 2026-10-25)"),
    (r"/apple/AppleApplicationInstaller\.kt$", "4.10b", ""),
    (r"/apple/(AppleDevice|RemoteFileManager)\.kt$", "4.10a", ""),
    (r"/apple/configuration/", "2.3b", "Marathondevices targets and transports: P2 types and binding; runtime use in 4.10a/5.x"),
    # ---- ios / macos -------------------------------------------------------------------------------------
    (r"/apple/ios/IosVendor\.kt$", "5.5a", "vendor construction; dynamic frame reader"),
    (r"/apple/ios/device/(AppleSimulatorProvider)\.kt$", "5.5a", "split with 5.5b; the dynamic frame decoder function is 2.3b"),
    (r"/apple/ios/device/(Detached|PendingWorker)\.kt$", "5.5b", ""),
    (r"/apple/ios/device/DeviceTracker\.kt$", "5.4", ""),
    (r"/apple/ios/device/", "5.4", ""),
    (r"/apple/ios/listener/", "5.3", ""),
    (r"/apple/ios/RecorderTypeSelector\.kt$", "5.3", ""),
    (r"/apple/ios/extensions/FileExtensions\.kt$", "n/a", "WP 5.2 P1 approved: unused resolveSibling copy; no caller at e47352f2"),
    (r"/apple/ios/model/Udid\.kt$", "n/a", "WP 5.2 P1 approved: UDID stays a plain Rust String"),
    (r"/apple/ios/model/Simulator\.kt$", "5.2", "State ported; Profile has no constructor caller (approved P1)"),
    (r"/apple/ios/", "5.2", "device, installer, media importer, models"),
    (r"/apple/macos/", "5.6", ""),
    # ---- android -------------------------------------------------------------------------------------------
    (r"/android/configuration/AndroidLogConfigurator\.kt$", "1.6", ""),
    (r"/android/adam/di/", "6.5", "factory; remote-parser slot belongs to 6.6a"),
    (r"/android/adam/event/", "6.3", "TestAnnotationParser"),
    (r"/android/adam/log/", "6.3", "logcat -v long parser"),
    (r"/android/adam/execution/", "6.6a", "ArgumentsFactory, test-runner contract"),
    (r"/android/adam/(AndroidDeviceTestRunner|AmInstrumentTestParser)\.kt$", "6.6a", ""),
    (r"/android/adam/(AdamDeviceProvider|DeviceStateTracker)\.kt$", "6.5", ""),
    (r"/android/adam/", "6.5", "AdamAndroidDevice and helpers; execute body belongs to 6.6a"),
    (r"/android/BaseAndroidDevice\.kt$", "6.5", "setup; createExecutionListeners L236-301 belongs to 6.6a and prepareRecorderListener L303-335 to 6.6b"),
    (r"/android/(DexTestParser|ApkParser|InstrumentationInfo)\.kt$", "6.4", ""),
    (r"(?i)/android/executor/listeners/(filesync|video|screenshot|AdamScreenCapture|Profiling)", "6.6b", ""),
    (r"/android/executor/", "6.6a", ""),
    (r"/android/logcat/Logcat(Listener|Producer)\.kt$", "6.3", "interfaces the 6.3 accumulating listener implements"),
    (r"/android/logcat/", "6.6a", ""),
    (r"/android/", "6.5", "device, installer, models, exceptions"),
    # ---- test support -------------------------------------------------------------------------------------
    (r"^vendor/vendor-test/", "3.10", "StubVendor / StubDevice in marathon-test-support"),
]

# Test-side overrides for scripts/parity_matrix.py, checked before the covered main file is looked up: tests
# whose path doesn't lead to the production code they cover, or that cover a different WP than their file.
# Ordered (regex on the repo-relative test path, regex on the test name or None, wp, reason). First match wins.
# The reason is used as the rust_test text of rows whose wp is n/a.
TEST_RULES = [
    # WP 3.9c decision 4: shared screenshot rotation, nine invocations.
    (r"/android/model/RotationTest\.kt$", None, "3.9c", ""),
    (r"^analytics/usage/", None, "n/a", "D1: usage analytics not ported"),
    (r"^core/src/test/.*/execution/DevicePoolActor\w*Test\.kt$", None, "3.8", ""),
    (r"^core/src/test/.*/marathon/StartupTimingsTest\.kt$", r"analytics queries", "3.4a", ""),
    (r"^core/src/test/.*/marathon/StartupTimingsTest\.kt$", None, "3.10", ""),
    (r"^core/src/test/.*/marathon/scenario/", None, "3.10", ""),
    (r"^core/src/integrationTest/.*/BaseMetricsProviderIntegrationTest\.kt$", None, "3.4b", "WP 3.4a decision 1: containers and trackers"),
    (r"^core/src/integrationTest/.*/InfluxDb\w*ProviderIntegrationTest\.kt$", None, "3.4b", ""),
    (r"^core/src/test/.*/ExecutionTimeCoverageTest\.kt$", None, "3.4a", ""),
    # WP 3.2 decision 1: tests that need MetricsProviderImpl or the real PoolProgressAccumulator FSM.
    (r"^core/src/test/.*/strategy/impl/batching/FixedSizeBatchingStrategyTest\.kt$", r"^without history", "3.4a", ""),
    (r"^core/src/test/.*/strategy/impl/retry/fixedquota/FixedQuotaRetryStrategyTest\.kt$", r"reached terminal state", "3.7", ""),
    (r"^configuration/src/test/.*/vendor/ios/AppleTestBundleConfigurationTest\.kt$", None, "2.3b", ""),
    # WP 2.3b decision 2: pure P2 parsing / id expansion, nothing of 4.10a.
    (r"^vendor/vendor-apple/base/src/test/.*/apple/configuration/(StaticTest|AppleTargetTest)\.kt$", None, "2.3b", "P2 parsing in 2.3b"),
    # WP 2.2 decision 1: AnalyticsDefaultsTest rows split by what they need.
    (r"^configuration/src/test/.*/analytics/AnalyticsDefaultsTest\.kt$", r"two-argument JVM constructor", "n/a", "tests the Kotlin @JvmOverloads JVM constructor ABI, which has no Rust counterpart"),
    (r"^configuration/src/test/.*/analytics/AnalyticsDefaultsTest\.kt$", r"below one is rejected", "2.4", ""),
    (r"^configuration/src/test/.*/analytics/AnalyticsDefaultsTest\.kt$", None, "2.2", ""),
    # whole-Configuration parse and round-trip tests need the 2.2/2.3 schemas, post-processing and writers
    (r"^configuration/src/test/.*/(ConfigurationFactory|ConfigurationSerialization|IosConfigurationFactory)Test\.kt$", None, "2.4", ""),
    (r"/apple/(ArtifactProcessorFairness|ArtifactQueueSizing)Test\.kt$", None, "4.9", ""),
    # WP 5.4 decision 2: consumer integration belongs to the provider/device adapter.
    (r"/apple/ios/device/WorkerProvisioningContextTest\.kt$", r"four unchanged|harness command counts|new profiles are planned|a simulator deleted|unavailable simulators|a slow inventory", "5.5a", ""),
    (r"/apple/ios/device/WorkerProvisioningContextTest\.kt$", r"an inventory requested earlier", "5.4", ""),
    (r"/apple/ios/device/WorkerProvisioningContextTest\.kt$", None, "5.2", ""),
    # StartupPhasesTest drives provider, parser and preparation together; each method goes to the WP it checks.
    # nm symbol scans of test parsing (NmTestParser)
    (r"/apple/ios/StartupPhasesTest\.kt$", r"symbol scan", "4.8", ""),
    # provisioning concurrency per worker (SimulatorProvisioner)
    (r"/apple/ios/StartupPhasesTest\.kt$", r"provisioning concurrency allows", "5.5a", ""),
    # provisioning and preparation timed together by the provider
    (r"/apple/ios/StartupPhasesTest\.kt$", r"provisioning and preparation log", "5.5a", ""),
    # late-join cut-off and bounded provider recovery
    (r"/apple/ios/StartupPhasesTest\.kt$", r"late join cut-off|set up again by its provider", "5.5b", ""),
    # xctestrun generation and push (TestRootFactory)
    (r"/apple/ios/StartupPhasesTest\.kt$", r"xctestrun", "4.5b", ""),
    # boot and preparation of a simulator
    (r"/apple/ios/StartupPhasesTest\.kt$", None, "5.2", ""),
    # DexTestParser drives the parsing; the literal test needs 2.3a's
    # AndroidConfiguration and 6.5's AndroidTestBundleIdentifier, so the
    # row moved to 6.5 (WP 6.4 decision 2 ports its expectation).
    (r"/android/AndroidTestParserTest\.kt$", None, "6.5", ""),
    # configuration model only
    (r"/android/configuration/AllureConfigurationTest\.kt$", None, "2.3a", ""),
    # WP 2.3a R1: the back-off row is pure WorkerConnectionConfiguration, no bundle validate()
    (r"^configuration/src/test/.*/vendor/IOSConfigurationTest\.kt$", r"worker connection back-off doubles", "2.3a", ""),
    # WP 2.3a decision 1: every other IOSConfigurationTest parse routes through bundle validate()
    (r"^configuration/src/test/.*/vendor/IOSConfigurationTest\.kt$", None, "2.3b", ""),
    # WP 2.3a decision 1: these assert LogicalConfigurationValidator / serialization round trips
    (r"^configuration/src/test/.*/vendor/apple/ThreadingConfigurationTest\.kt$", None, "2.4", ""),
    (r"^configuration/src/test/.*/vendor/apple/ios/XcresultQueueConfigurationTest\.kt$", None, "2.4", ""),
    (r"^configuration/src/test/.*/vendor/apple/TimeoutConfigurationTest\.kt$", r"serialization round trip", "2.4", ""),
    # testDefaults checks toScreenRecorderCommand (the video listener), like its siblings
    (r"/android/VideoConfigurationTest\.kt$", None, "6.5", "WP 6.5 orchestrator decision 3"),
    # WP 4.5a decision 2: the whole file is commented out and tests a deleted legacy API.
    (r"/apple/xctestrun/XctestrunTest\.kt$", None, "n/a",
     "commented-out tests of the deleted legacy xctestrun API (targetName/isSkipped/clone/environment/toXMLString), absent at e47352f2; the 3 still-meaningful ideas are re-expressed against v1 in crates/marathon-apple/tests/xctestrun_v1.rs without parity rows; both WP 4.5a reviewers agreed (review round 1)"),
    (r"/apple/simctl/model/SimctlDeviceListDeserializerTest\.kt$", None, "4.4", ""),
    (r"/apple/xcodebuild/XcodebuildTest\.kt$", None, "4.4", ""),
    (r"/apple/xcresulttool/XcresulttoolGetTest\.kt$", None, "4.4", ""),
    (r"/apple/xcresulttool/XcresulttoolTest\.kt$", None, "4.6", "legacy JSON model"),
    (r"/XCTestParserTest\.kt$", None, "4.8", ""),
    # WP 4.9 decision 1: real runner integrations wait for both 4.8 and 4.9.
    (r"/ResultBundleRunListenerTest\.kt$", r"closed queue does not fail|next real runner batch|policies results traces|failed delivery does not fail|bundle skipped by a failing|bundles registered around", "5.2", ""),
    (r"/ResultBundleRunListenerTest\.kt$", None, "4.9", ""),
    (r"/apple/listener/TestResultsListenerTest\.kt$", None, "4.7", ""),
    (r"/apple/ios/SkipCopyingDerivedDataTest\.kt$", r"^parsing ", "4.8", "NmTestParser rows; WP 4.10b decision 2"),
    (r"/apple/ios/SkipCopyingDerivedDataTest\.kt$", None, "4.10b", "installer and distributed derived data"),
    (r"/android/.*AndroidTestStatusTest\.kt$", None, "6.5", ""),
    (r"/ios/device/AppleSimulatorProvider(Reconnect|WorkerRetry)Test\.kt$", None, "5.5b", "recovery and late join"),
    (r"/ios/device/AppleSimulatorProvider\w*Test\.kt$", None, "5.5a", ""),
    (r"/marathon/ios/AppleDeviceProviderTest\.kt$", None, "5.5a", ""),
    (r"/marathon/ios/DerivedDataManagerTest\.kt$", None, "4.10b", ""),
]


def test_owner(rel: str, name: str):
    """Return (wp, reason) of the first TEST_RULES entry matching a test, or None."""
    for path, test, wp, reason in TEST_RULES:
        if re.search(path, rel) and (test is None or re.search(test, name)):
            return wp, reason
    return None


OUT =pathlib.Path(__file__).resolve().parent.parent / "docs" / "source-inventory.csv"


def owner(rel: str):
    """Return (wp, note) of the first rule matching a repo-relative path, or None."""
    for pattern, wp, note in RULES:
        if re.search(pattern, rel):
            return wp, note
    return None


def generate(root: pathlib.Path):
    """Return (csv text, number of files, unowned files) for the Kotlin checkout at root."""
    files = sorted(
        str(p.relative_to(root))
        for p in root.rglob("*.kt")
        if "/src/main/" in str(p) and "/build/" not in str(p)
    )
    unowned = []
    buf = io.StringIO(newline="")
    w = csv.writer(buf)
    w.writerow(["kotlin_file", "lines", "wp", "note"])
    for rel in files:
        lines = sum(1 for _ in (root / rel).open(encoding="utf-8", errors="replace"))
        match = owner(rel)
        if match is None:
            unowned.append(rel)
            match = ("UNOWNED", "")
        w.writerow([rel, lines, *match])
    return buf.getvalue(), len(files), unowned


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    # An empty argument is the current directory to pathlib, and is what an unset variable expands to.
    if (
        len(args) != 1
        or not args[0]
        or args[0].startswith("-")
        or not (pathlib.Path(args[0]) / "build.gradle.kts").is_file()
        or not (pathlib.Path(args[0]) / "vendor").is_dir()
    ):
        # A wrong argument must not reach generate: it would find no files, and the inventory would lose their rows.
        print(__doc__, file=sys.stderr)
        return 2
    text, count, unowned = generate(pathlib.Path(args[0]).resolve())
    for rel in unowned:
        print("UNOWNED", rel)
    if check:
        current = OUT.read_bytes().decode("utf-8") if OUT.exists() else ""
        if current != text:
            print(f"{OUT} is out of date; rerun without --check", file=sys.stderr)
            return 1
        print(f"source inventory up to date ({count} files)")
    else:
        OUT.write_bytes(text.encode("utf-8"))
        print(f"{count} files -> {OUT}")
    return 1 if unowned else 0


if __name__ == "__main__":
    sys.exit(main())
