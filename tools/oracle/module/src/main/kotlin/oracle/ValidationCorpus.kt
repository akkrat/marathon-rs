package oracle

import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.LogicalConfigurationValidator
import com.malinskiy.marathon.config.environment.EnvironmentConfiguration
import com.malinskiy.marathon.config.environment.EnvironmentReader
import com.malinskiy.marathon.config.serialization.ConfigurationFactory
import org.apache.commons.text.StringSubstitutor
import java.io.File
import java.nio.file.Files
import java.time.Instant

/**
 * WP 2.4 staging corpus: whole Marathonfiles driven through the production [ConfigurationFactory] (stage a) and
 * [LogicalConfigurationValidator] (stage b), as `Marathon.runAsync` runs them. Each row records the stage that rejected
 * it and the error, and for a parsed configuration its `toMap()` entries in order and, for the rows marked so, the
 * `serialize()` text.
 *
 * Rows of kind `case` run in one fresh temp dir `$DIR` holding a copy of the iOS fixture `derivedDataDir`, an empty
 * `app.apk` file, an `apkdir` directory, an empty `known_hosts` file and a `loop` symlink to itself; `@DIR@` in an input stands for that dir.
 * Rows of kind `fixture` parse a copy of `fixtures/configuration/fixture/config/<path>` with the file's parent as
 * `marathonfileDir` (ConfigurationSerializationTest's choice); `$DIR` is the copy's root. The environment reader returns
 * `/android-sdk` unless a row says `noSdk`, the substitutor knows only `A=${'$'}{B}` and `B=${'$'}{A}` (a cycle); the reference time is 1970-01-12T13:46:40Z (epoch second 1000000).
 *
 * Kotlin sets are HashSets whose order is unspecified, so set-valued inputs here have one element. A canonical path under
 * `/home` is recorded with the `/home/` spelling (macOS canonicalises it to `/System/Volumes/Data/home/`).
 *
 * Kotlin `object`s and the bundle's `tempDirFor` lambda render with run-specific identity hashes; those are recorded as
 * `@<hash>` (and a lambda's `0x…` address as `0x<addr>`), so the table is byte-stable.
 */
object ValidationCorpus {
    private val dirToken = "${'$'}DIR"
    private val now: Instant = Instant.ofEpochSecond(1000000)
    private val homeRoot = File("/home").canonicalPath + "/"

    private class Case(val id: String, val input: String, val serialize: Boolean = false, val noSdk: Boolean = false)

    fun generate(outDir: File, fixtureConfig: File) {
        val table = JsonlTable(
            "config-validation-corpus",
            "WP 2.4: Marathonfiles through ConfigurationFactory.parse (stage a) and LogicalConfigurationValidator (stage b); " +
                "$dirToken is the Marathonfile temp dir, @<hash>/0x<addr> stand for identity hashes and lambda addresses",
            linkedMapOf(
                "id" to "row name",
                "kind" to "case (input inline) or fixture (input is a path under fixtures/configuration/fixture/config)",
                "input" to "the Marathonfile text with $dirToken, or the fixture path",
                "noSdk" to "true when the environment has no Android SDK",
                "stage" to "parse, logical or ok",
                "error" to "{class, message, causes} of the rejecting stage, or null",
                "toMap" to "Configuration.toMap() as [key, value] pairs in order, or null when stage a failed",
                "serialized" to "ConfigurationFactory.serialize() text for rows marked so, else null",
            ),
        )
        val dir = Files.createTempDirectory("validation-corpus").toFile().canonicalFile
        try {
            File(fixtureConfig, "ios/derivedDataDir").copyRecursively(File(dir, "derivedDataDir"))
            File(dir, "app.apk").writeText("")
            File(dir, "apkdir").mkdirs()
            File(dir, "known_hosts").writeText("")
            Files.createSymbolicLink(File(dir, "loop").toPath(), File(dir, "loop").toPath())
            checkMapperMatchesMarathon(dir)
            val marathonfile = File(dir, "Marathonfile")
            for (case in cases) {
                marathonfile.writeText(case.input.trimIndent().replace("@DIR@", dir.path) + "\n")
                table.add(row(case.id, "case", case.input.trimIndent(), marathonfile, dir, dir, case.serialize, case.noSdk))
            }
        } finally {
            dir.deleteRecursively()
        }
        for (path in fixtures) {
            val root = Files.createTempDirectory("validation-fixture").toFile().canonicalFile
            try {
                fixtureConfig.copyRecursively(root)
                val file = File(root, path)
                table.add(row(path, "fixture", path, file, file.parentFile, root, serialize = false, noSdk = false))
            } finally {
                root.deleteRecursively()
            }
        }
        table.writeTo(outDir)
    }

    private fun row(
        id: String, kind: String, input: String, file: File, marathonfileDir: File, tokenDir: File, serialize: Boolean,
        noSdk: Boolean,
    ): Map<String, Any?> {
        val factory = ConfigurationFactory(
            marathonfileDir = marathonfileDir,
            environmentReader = object : EnvironmentReader {
                override fun read() = EnvironmentConfiguration(if (noSdk) null else File("/android-sdk"))
            },
            mapper = marathonfileMapper(FixedTimeProvider(now), marathonfileDir),
            environmentVariableSubstitutor = StringSubstitutor(mapOf("A" to "${'$'}{B}", "B" to "${'$'}{A}")),
            analyticsTracking = true,
            bugsnagReporting = true,
        )
        var stage = "ok"
        var error: Throwable? = null
        val config: Configuration? = try {
            factory.parse(file)
        } catch (t: Throwable) {
            stage = "parse"
            error = t
            null
        }
        if (config != null) {
            try {
                LogicalConfigurationValidator().validate(config)
            } catch (t: Throwable) {
                stage = "logical"
                error = t
            }
        }
        fun clean(s: String) = normalize(s.replace(tokenDir.path, dirToken).replace(homeRoot, "/home/"))
        return linkedMapOf(
            "id" to id,
            "kind" to kind,
            "input" to input,
            "noSdk" to noSdk,
            "stage" to stage,
            "error" to error?.let { e ->
                errorJson(e).mapValues { (_, v) ->
                    when (v) {
                        is String -> clean(v)
                        is List<*> -> v.map { clean(it as String) }
                        else -> v
                    }
                }
            },
            "toMap" to config?.toMap()?.map { (k, v) -> listOf(k, clean(v)) },
            "serialized" to if (serialize && config != null) clean(factory.serialize(config)) else null,
        )
    }

    private val identityHash = Regex("(?<=[A-Za-z0-9_$])@[0-9a-f]{1,8}(?![0-9A-Za-z])")
    private val lambdaAddress = Regex("\\$\\\$Lambda/0x[0-9a-f]+")

    private fun normalize(s: String) = s.replace(lambdaAddress, "\\$\\\$Lambda/0x<addr>").replace(identityHash, "@<hash>")

    private val fixtures = listOf(
        "sample_1.yaml", "sample_1_rp.yaml", "sample_2.yaml", "sample_6.yaml", "sample_7.yaml", "sample_8.yaml",
        "sample_9.yaml", "sample_10.yaml", "sample_11.yaml", "sample_12.yaml", "sample_1_no_tracking.yaml",
        "android/sample_1.yaml", "android/sample_2.yaml", "android/sample_3.yaml", "android/sample_4.yaml",
        "android/sample_5.yaml", "android/sample_6.yaml",
        "ios/sample_1.yaml", "ios/sample_2.yaml", "ios/sample_3.yaml", "ios/sample_4.yaml", "ios/sample_5.yaml",
        "ios/sample_6.yaml", "ios/sample_7.yaml",
    )

    private const val ANDROID = """
vendorConfiguration:
  type: Android
  applicationApk: app.apk
  testApplicationApk: app.apk"""
    private const val IOS = """
vendorConfiguration:
  type: iOS
  bundle:
    derivedDataDir: derivedDataDir"""
    private const val MACOS = """
vendorConfiguration:
  type: macOS
  bundle:
    derivedDataDir: derivedDataDir"""

    private fun mf(body: String, vendor: String = ANDROID) = "name: t\noutputDir: out\n" + body.trimIndent() + vendor

    private val cases: List<Case> = listOf(
        // Stage (a): unwrapped factory errors versus the wrapped binding failure.
        Case("a-empty-vendor", "name: t\noutputDir: out"),
        Case("a-null-document", "--- null\n---\nname: t"),
        Case("a-substitution-cycle", "name: ${'$'}{A}\noutputDir: out"),
        Case("a-no-sdk", mf(""), noSdk = true),
        Case("a-binding-wrapped", mf("shardingStrategy:\n  type: count\n  count: many")),
        Case("a-unknown-vendor-type", "name: t\noutputDir: out\nvendorConfiguration:\n  type: Windows"),
        Case("a-bundle-missing-derived", "name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle:\n    derivedDataDir: nowhere"),
        // `canonicalFile` through a self-referencing symlink, and a bundle failure preceding it (step 2 before step 4).
        Case("a-devices-symlink-loop", "name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle:\n    derivedDataDir: derivedDataDir\n  devices: loop/Marathondevices"),
        Case("a-known-hosts-symlink-loop", "name: t\noutputDir: out\nvendorConfiguration:\n  type: macOS\n  ssh:\n    knownHostsPath: loop/kh"),
        Case("a-bundle-before-devices", "name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle:\n    derivedDataDir: nowhere\n  devices: loop/Marathondevices"),
        Case("a-stub", "name: t\noutputDir: out\nvendorConfiguration:\n  type: stub", serialize = true),
        // Stage (b), statement 1: complex flakiness with complex sharding.
        Case("b-flakiness-sharding", mf(
            """
            flakinessStrategy:
              type: probability
              minSuccessRate: 0.7
              maxCount: 3
              timeLimit: "2015-03-14T09:26:53.590Z"
            shardingStrategy:
              type: count
              count: 2
            """)),
        Case("b-flakiness-parallel-ok", mf(
            """
            flakinessStrategy:
              type: probability
              minSuccessRate: 0.7
              maxCount: 3
              timeLimit: PT1H
            shardingStrategy:
              type: parallel
            """), serialize = true),
        Case("b-ignore-count-ok", mf("shardingStrategy:\n  type: count\n  count: 3\nexecutionStrategy:\n  mode: ALL_SUCCESS")),
        // Statement 2: filters, allowlist then blocklist.
        Case("b-filter-allowlist-none", mf("filteringConfiguration:\n  allowlist:\n    - type: simple-class-name")),
        Case("b-filter-allowlist-two", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: fully-qualified-test-name
                  regex: ".*"
                  values: [a]
            """)),
        Case("b-filter-blocklist-after-allowlist", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: method
                  regex: ".*"
                - type: package
              blocklist:
                - type: annotation
            """)),
        Case("b-filter-blocklist-only", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: method
                  regex: ".*"
              blocklist:
                - type: fragmentation
                  index: 3
                  count: 2
            """)),
        Case("b-filter-composition-nested", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: composition
                  op: INTERSECTION
                  filters:
                    - type: simple-test-name
                      values: [x]
                    - type: fragmentation
                      index: -1
                      count: 2
            """)),
        Case("b-precedence-flakiness-before-filter", mf(
            """
            flakinessStrategy:
              type: probability
              minSuccessRate: 0.7
              maxCount: 3
              timeLimit: PT1H
            shardingStrategy:
              type: count
              count: 2
            filteringConfiguration:
              allowlist:
                - type: simple-class-name
            """)),
        // Statement 3: analytics defaults, after filters, before the vendor.
        Case("b-analytics-influx", mf(
            """
            analyticsConfiguration:
              type: influxdb
              url: http://influx
              user: u
              password: p
              dbName: d
              defaults:
                observedDurationMinTests: 0
            """)),
        Case("b-analytics-graphite-negative", mf(
            """
            analyticsConfiguration:
              type: graphite
              host: g
              defaults:
                observedDurationMinTests: -5
            """)),
        Case("b-precedence-filter-before-analytics", mf(
            """
            filteringConfiguration:
              blocklist:
                - type: annotation
            analyticsConfiguration:
              type: graphite
              host: g
              defaults:
                observedDurationMinTests: 0
            """)),
        Case("b-precedence-analytics-before-vendor", mf(
            """
            analyticsConfiguration:
              type: influxdb2
              url: http://influx2
              token: t
              organization: o
              bucket: b
              defaults:
                observedDurationMinTests: 0
            """, IOS + "\n  maxBatchesWithoutProgress: -1")),
        // Statement 4: the vendor dispatch.
        Case("b-android-missing-apk", mf("", "\nvendorConfiguration:\n  type: Android\n  applicationApk: missing.apk\n  testApplicationApk: app.apk")),
        Case("b-android-apk-is-dir", mf("", "\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n  testApplicationApk: apkdir")),
        Case("b-android-split-missing", mf("", "$ANDROID\n  splitApks: [nothere.apk]")),
        Case("b-android-ok", mf("", "$ANDROID\n  outputs:\n    - application: app.apk\n      testApplication: app.apk")),
        Case("b-ios-max-batches", mf("", "$IOS\n  maxBatchesWithoutProgress: -1")),
        Case("b-ios-ssh-key-missing", mf("", "$IOS\n  ssh:\n    authentication:\n      type: publicKey\n      username: u\n      key: nokey")),
        Case("b-ios-known-hosts-missing", mf("", "$IOS\n  ssh:\n    knownHostsPath: nohosts")),
        Case("b-ios-worker-connection", mf("", "$IOS\n  workerConnection:\n    attempts: 0")),
        Case("b-ios-queue", mf("", "$IOS\n  xcresult:\n    queue:\n      concurrency: 9")),
        Case("b-ios-threading", mf("", "$IOS\n  threadingConfiguration:\n    provisioningConcurrency: 0")),
        Case("b-ios-no-bundle", mf("", "\nvendorConfiguration:\n  type: iOS")),
        Case("b-ios-precedence-max-batches-before-ssh", mf("", "$IOS\n  maxBatchesWithoutProgress: -1\n  ssh:\n    knownHostsPath: nohosts")),
        Case("b-ios-tvos-ok", mf("", "\nvendorConfiguration:\n  type: tvOS\n  bundle:\n    derivedDataDir: derivedDataDir"), serialize = true),
        Case("b-macos-queue", mf("", "$MACOS\n  xcresult:\n    queue:\n      capacity: 16")),
        Case("b-macos-provisioning", mf("", "$MACOS\n  threadingConfiguration:\n    provisioningConcurrency: 4")),
        Case("b-macos-provisioning-zero", mf("", "$MACOS\n  threadingConfiguration:\n    provisioningConcurrency: 0")),
        Case("b-macos-queue-before-provisioning", mf("", "$MACOS\n  xcresult:\n    queue:\n      workerCapacity: 2\n  threadingConfiguration:\n    provisioningConcurrency: 1")),
        // F6: MacosConfiguration.validate() would reject these (SSH key, missing bundle), stage (b) does not call it.
        Case("b-macos-f6-ssh-key-missing", mf("", "$MACOS\n  ssh:\n    authentication:\n      type: publicKey\n      username: u\n      key: nokey")),
        Case("b-macos-f6-no-bundle", mf("", "\nvendorConfiguration:\n  type: macOS"), serialize = true),
        Case("b-macos-device-threads-ok", mf("", "$MACOS\n  threadingConfiguration:\n    deviceThreads: 4")),
        // Statement 5: execution-mode rules, last.
        Case("b-any-success-count", mf("shardingStrategy:\n  type: count\n  count: 2")),
        Case("b-any-success-count-one", mf("shardingStrategy:\n  type: count\n  count: 1")),
        Case("b-all-success-flakiness", mf(
            """
            executionStrategy:
              mode: ALL_SUCCESS
            flakinessStrategy:
              type: probability
              minSuccessRate: 0.7
              maxCount: 3
              timeLimit: PT1H
            retryStrategy:
              type: fixed-quota
            """)),
        Case("b-all-success-retry", mf("executionStrategy:\n  mode: ALL_SUCCESS\nretryStrategy:\n  type: fixed-quota")),
        Case("b-all-success-ok", mf("executionStrategy:\n  mode: ALL_SUCCESS\n  fast: false")),
        Case("b-precedence-vendor-before-execution", mf("shardingStrategy:\n  type: count\n  count: 2", "\nvendorConfiguration:\n  type: Android\n  applicationApk: missing.apk\n  testApplicationApk: app.apk")),
        // toMap/serialize witnesses: masking, filters, strategies, vendors.
        Case("m-influxdb2", mf(
            """
            analyticsConfiguration:
              type: influxdb2
              url: http://influx2
              token: secret
              organization: o
              bucket: b
              retentionPolicyConfiguration:
                everySeconds: 3600
                shardGroupDurationSeconds: 60
              defaults:
                successRate: 0.25
                duration: PT40S
                observedDurationMinTests: 3
            """), serialize = true),
        Case("m-graphite", mf(
            """
            analyticsConfiguration:
              type: graphite
              host: g
              port: 2003
              prefix: pre
            """), serialize = true),
        Case("m-influxdb-rp", mf(
            """
            analyticsConfiguration:
              type: influxdb
              url: http://influx
              user: u
              password: p
              dbName: d
              retentionPolicyConfiguration:
                name: rp
                duration: 90d
                shardDuration: 1h
                replicationFactor: 5
                isDefault: false
            """), serialize = true),
        Case("m-filters", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: simple-class-name
                  regex: "Foo.*"
                - type: simple-test-name
                  values: [a, b]
                  enabled: false
                - type: fully-qualified-class-name
                  file: filterfile
                - type: fully-qualified-test-name
                  regex: "x"
                - type: package
                  values: [com.example]
                  subpackages: true
                - type: annotationData
                  nameRegex: "n"
                  valueRegex: "v"
                - type: fragmentation
                  index: 0
                  count: 2
                - type: allure
              blocklist:
                - type: composition
                  op: SUBTRACT
                  filters:
                    - type: method
                      regex: "m"
                    - type: annotation
                      values: [A]
            """), serialize = true),
        Case("m-filter-null-value", mf(
            """
            filteringConfiguration:
              allowlist:
                - type: simple-class-name
                  values: [a, null, b]
            """)),
        Case("m-strategies", mf(
            """
            poolingStrategy:
              type: combo
              list:
                - type: omni
                - type: abi
                - type: manufacturer
                - type: device-model
                - type: os-version
            sortingStrategy:
              type: execution-time
              percentile: 90.5
              timeLimit: "-PT1H"
            batchingStrategy:
              type: fixed-size
              size: 5
              durationMillis: 100000
              percentile: 80.0
              timeLimit: "-PT1H"
              lastMileLength: 10
            retryStrategy:
              type: fixed-quota
              totalAllowedRetryQuota: 100
              retryPerTestQuota: 2
            includeSerialRegexes: ["emulator-500[2,4]", "x"]
            excludeSerialRegexes: ["y"]
            uncompletedTestRetryQuota: 5
            outputConfiguration:
              maxPath: 100
              maxFilename: 50
            ciConfiguration:
              type: teamcity
            screenRecordingPolicy: ON_ANY
            analyticsTracking: false
            """), serialize = true),
        Case("m-strategies-singletons", mf(
            """
            sortingStrategy:
              type: random-order
            batchingStrategy:
              type: class-name
            ciConfiguration:
              type: auto
            """), serialize = true),
        Case("m-sorting-success-rate", mf(
            """
            sortingStrategy:
              type: success-rate
              timeLimit: "2015-03-14T09:26:53.590Z"
              ascending: true
            poolingStrategy:
              type: device-model
            """), serialize = true),
        Case("m-android-full", mf("", """
vendorConfiguration:
  type: Android
  androidSdk: /sdk
  applicationApk: app.apk
  testApplicationApk: app.apk
  splitApks: [app.apk]
  extraApplicationsApk: [app.apk]
  outputs:
    - application: app.apk
      testApplication: app.apk
      extraApplications: [app.apk]
      splitApks: [app.apk]
  autoGrantPermission: true
  instrumentationArgs:
    debug: "false"
    b: "1"
  applicationPmClear: true
  testApplicationPmClear: true
  adbInitTimeoutMillis: 1000
  installOptions: "-d"
  serialStrategy: hostname
  screenRecordConfiguration:
    preferableRecorderType: screenshot
    videoConfiguration:
      enabled: false
      timeLimit: 3
      timeLimitUnits: MINUTES
    screenshotConfiguration:
      delayMs: 100
  waitForDevicesTimeoutMillis: 5000
  allureConfiguration:
    enabled: true
    pathRoot: EXTERNAL_STORAGE
  timeoutConfiguration:
    shell: PT30S
    install: P1DT12H30M5S
  fileSyncConfiguration:
    pull:
      - relativePath: a
        aggregationMode: TEST_RUN
    push:
      - path: b
  threadingConfiguration:
    installThreads: 2
  testParserConfiguration:
    type: remote
    instrumentationArgs:
      listener: x
  testAccessConfiguration:
    adb: true
    consoleToken: tok
  adbServers:
    - host: h
      port: 1
    - {}
  disableWindowAnimation: false
  profilingConfiguration:
    enabled: true
    pbtxt: app.apk
  mockLocation: true"""), serialize = true),
        Case("m-ios-full", mf("", """
vendorConfiguration:
  type: iOS
  bundle:
    derivedDataDir: derivedDataDir
    testType: xctest
  deviceProvider:
    type: dynamic
    host: h
    port: 1
  devices: Testdevices
  ssh:
    authentication:
      type: password
      username: u
      password: secret
    knownHostsPath: known_hosts
    keepAliveInterval: PT1M
    shareWorkerConnection: true
  workerConnection:
    attempts: 5
    lateJoin: false
  xcresult:
    pull: false
    attachments:
      systemAttachmentLifetime: KEEP_NEVER
    preferredScreenCaptureFormat: SCREEN_RECORDING
    queue:
      workerCapacity: 3
  screenRecordConfiguration:
    preferableRecorderType: video
    videoConfiguration:
      codec: hevc
      display: external
      mask: ignored
      transcoding:
        enabled: true
        size: hd1080
    screenshotConfiguration:
      type: png
      delay: PT1S
  xctestrunEnv:
    app:
      A: "1"
    test:
      B: "2"
  lifecycle:
    shutdownUnused: false
    onPrepare: [ERASE]
    onDispose: [TERMINATE]
  permissions:
    grant: [user-tracking]
    lifecycle: BEFORE_EACH_BATCH
  importMedia: [derivedDataDir/sample.app/empty]
  timeoutConfiguration:
    shell: PT10S
    boot: PT5M
  threadingConfiguration:
    deviceThreads: 3
  hideRunnerOutput: true
  compactOutput: true
  rsync:
    remotePath: /r
  xcodebuildTestArgs:
    "-a": b
  dataContainerClear: true
  testParserConfiguration:
    type: xctest
  deviceLog: true
  signing:
    testRunnerProvisioningProfile: prof
  skipCopyingDerivedData: true
  maxBatchesWithoutProgress: 2"""), serialize = true),
        Case("m-ios-nm-explicit", mf("", "$IOS\n  testParserConfiguration:\n    type: nm\n    testClassRegexes: [\"Foo\", \"Bar\"]\n  deviceProvider:\n    type: static\n    path: devs"), serialize = true),
        Case("m-macos-full", mf("", """
vendorConfiguration:
  type: macOS
  bundle:
    derivedDataDir: derivedDataDir
  ssh:
    authentication:
      type: publicKey
      username: u
      key: known_hosts
  xcresult:
    pull: true
    remoteClean: false
  timeoutConfiguration:
    shell: PT5M
  permissions:
    grant:
      com.example: [developer-tool]
  rsync:
    remotePath: /r"""), serialize = true),
        Case("m-explicit-null-queue-worker-capacity", mf("", "$IOS\n  xcresult:\n    queue:\n      workerCapacity: null"), serialize = true),
    )
}
