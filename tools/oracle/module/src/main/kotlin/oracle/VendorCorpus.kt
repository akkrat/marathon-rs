package oracle

import com.malinskiy.marathon.config.environment.EnvironmentConfiguration
import com.malinskiy.marathon.config.environment.EnvironmentReader
import com.malinskiy.marathon.config.serialization.ConfigurationFactory
import org.apache.commons.text.StringSubstitutor
import java.io.File
import java.nio.file.Files
import java.time.Instant

/**
 * WP 2.3a vendor-configuration corpus: every row is a `vendorConfiguration`
 * YAML subtree driven through the production [ConfigurationFactory] (the P1
 * mapper with `SerializeModule`, a real marathonfileDir and a stubbed
 * [EnvironmentReader] returning `/env-sdk`), exactly as the review JVM probe
 * did. The Marathonfile lives in a fresh temp dir holding `real/`, `sub/`,
 * a `link` symlink to `real/` and an empty `kh` file; `@DIR@` in an input
 * stands for that dir (recorded as `$DIR`), and recorded paths under it are
 * written back with `$DIR` substituted, so the table is byte-stable.
 */
object VendorCorpus {
    private val dirToken = "${'$'}DIR"

    fun generate(outDir: File) {
        val dir = Files.createTempDirectory("vendor-corpus").toFile().canonicalFile
        try {
            File(dir, "real").mkdirs()
            File(dir, "sub").mkdirs()
            Files.createSymbolicLink(File(dir, "link").toPath(), File(dir, "real").toPath())
            File(dir, "kh").writeText("")
            checkMapperMatchesMarathon(dir)
            val factory = ConfigurationFactory(
                marathonfileDir = dir,
                environmentReader = object : EnvironmentReader {
                    override fun read() = EnvironmentConfiguration(File("/env-sdk"))
                },
                mapper = marathonfileMapper(FixedTimeProvider(Instant.EPOCH), dir),
                environmentVariableSubstitutor = StringSubstitutor(mapOf("ANDROID_HOME" to "/env-sdk")),
            )
            val table = JsonlTable(
                "yaml-vendor-corpus",
                "WP 2.3a: vendorConfiguration subtrees through the production ConfigurationFactory; $dirToken is the Marathonfile temp dir (real/, sub/, link -> real/, kh)",
                linkedMapOf(
                    "id" to "row name",
                    "input" to "the vendorConfiguration YAML subtree, with $dirToken for the Marathonfile dir",
                    "result" to "{ok: canonical vendorConfiguration} or {error: {class, message, causes}}",
                ),
            )
            val marathonfile = File(dir, "Marathonfile")
            for ((id, input) in cases) {
                val body = input.trimIndent().replace("@DIR@", dir.path)
                val indented = body.lines().joinToString("\n") { if (it.isEmpty()) it else "  $it" }
                marathonfile.writeText("name: t\noutputDir: out\nvendorConfiguration:\n$indented\n")
                val result = try {
                    mapOf("ok" to substituteDir(Canonical.render(factory.parse(marathonfile).vendorConfiguration), dir))
                } catch (t: Throwable) {
                    mapOf("error" to substituteDir(errorJson(t), dir))
                }
                table.add(
                    linkedMapOf(
                        "id" to id,
                        "input" to input.trimIndent().replace("@DIR@", dirToken),
                        "result" to result,
                    )
                )
            }
            table.writeTo(outDir)
        } finally {
            dir.deleteRecursively()
        }
    }

    private fun substituteDir(value: Any?, dir: File): Any? = when (value) {
        is String -> value.replace(dir.path, dirToken)
        is Map<*, *> -> value.mapKeys { (k, _) -> (k as String).replace(dir.path, dirToken) }
            .mapValues { (_, v) -> substituteDir(v, dir) }
        is Collection<*> -> value.map { substituteDir(it, dir) }
        else -> value
    }

    // Seeded from the WP 2.3a review probes (production-factory cases and the
    // plain-mapper pins): dispatch, singleton extra keys, explicit-null
    // sections and leaves, the nullable Android parameters, creators,
    // TimeUnit, wrong-case enums, timeout order/null/number forms, duplicate
    // map keys, duplicate set entries, regex forms and path resolution.
    private val cases: List<Pair<String, String>> = listOf(
        "android-recorder-lower" to """
            type: Android
            screenRecordConfiguration:
              preferableRecorderType: video
            """,
        "android-recorder-i" to """
            type: Android
            screenRecordConfiguration:
              preferableRecorderType: ScreenShot
            """,
        "ios-recorder-lower" to """
            type: iOS
            screenRecordConfiguration:
              preferableRecorderType: video
            """,
        "android-serial-lower" to """
            type: Android
            serialStrategy: hostname
            """,
        "android-serial-num" to """
            type: Android
            serialStrategy: 1
            """,
        "android-serial-null" to """
            type: Android
            serialStrategy: null
            """,
        "ios-devices-dotdot" to """
            type: iOS
            devices: link/../sub/../link/Marathondevices
            """,
        "ios-devices-abs-dotdot" to """
            type: iOS
            devices: @DIR@/sub/../link/Marathondevices
            """,
        "ios-devices-missing" to """
            type: iOS
            """,
        "ios-ssh-key-kh" to """
            type: iOS
            ssh:
              authentication:
                type: publicKey
                username: u
                key: link/key
              knownHostsPath: sub/../kh
            """,
        "ios-media" to """
            type: iOS
            importMedia: [link/a.jpg, @DIR@/sub/../b.jpg]
            """,
        "ios-ssh-null" to """
            type: iOS
            ssh: null
            """,
        "ios-timeout-null" to """
            type: iOS
            timeoutConfiguration: null
            """,
        "android-timeout-section-null" to """
            type: Android
            timeoutConfiguration: null
            """,
        "ios-shell-null" to """
            type: iOS
            timeoutConfiguration:
              shell: null
            """,
        "android-shell-null" to """
            type: Android
            timeoutConfiguration:
              shell: null
            """,
        "ios-keepalive-null" to """
            type: iOS
            ssh:
              keepAliveInterval: null
            """,
        "ios-xcresult-pull-false" to """
            type: iOS
            xcresult:
              pullingPolicy: ALWAYS
              pull: false
            """,
        "ios-nm-explicit" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: ["^Foo"]
            """,
        "ios-nm-object" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: [{pattern: "^Foo"}]
            """,
        "nm-obj-empty" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: [{}]
            """,
        "nm-obj-options" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: [{pattern: "^foo", options: [IGNORE_CASE]}]
            """,
        "ios-nm-null" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: null
            """,
        "ios-nm-scalar" to """
            type: iOS
            testParserConfiguration:
              type: nm
              testClassRegexes: "^Foo"
            """,
        "ios-lifecycle-dups" to """
            type: iOS
            lifecycle:
              onPrepare: [ERASE, SHUTDOWN, ERASE]
            """,
        "ios-lifecycle-order" to """
            type: iOS
            lifecycle:
              onPrepare: [TERMINATE, SHUTDOWN, ERASE]
              onDispose: [ERASE, SHUTDOWN, TERMINATE]
            permissions:
              grant: [siri, all, calendar, contacts, location, photos, microphone, motion, reminders, user-tracking, pasteboard]
            """,
        "macos-perm" to """
            type: macOS
            permissions:
              grant:
                com.a: [accessibility, developer-tool, accessibility]
                com.a: [developer-tool]
            """,
        "watchOS" to """
            type: watchOS
            """,
        "tvOS" to """
            type: tvOS
            """,
        "visionOS" to """
            type: visionOS
            """,
        "iPadOS" to """
            type: iPadOS
            """,
        "ios-lower" to """
            type: ios
            """,
        "android-lower" to """
            type: android
            """,
        "dynamic-port-null" to """
            type: iOS
            deviceProvider:
              type: dynamic
              port: null
            """,
        "ios-deviceprovider-null" to """
            type: iOS
            deviceProvider: null
            """,
        "marathondevices-provider" to """
            type: iOS
            deviceProvider:
              type: marathondevices
              path: devs
            """,
        "android-timeouts" to """
            type: Android
            timeoutConfiguration:
              portForward: PT1S
              shell: PT2M
            """,
        "android-timeout-number" to """
            type: Android
            timeoutConfiguration:
              shell: 45
            """,
        "apple-timeouts-order" to """
            type: iOS
            timeoutConfiguration:
              xcresulttool: PT20S
              shutdown: PT5M
              boot: PT1M
            """,
        "android-instr-dup" to """
            type: Android
            instrumentationArgs:
              a: "1"
              b: "2"
              a: "3"
            """,
        "android-filesync-dup" to """
            type: Android
            fileSyncConfiguration:
              pull:
                - relativePath: a
                - relativePath: b
                - relativePath: a
            """,
        "android-push" to """
            type: Android
            fileSyncConfiguration:
              pull:
                - relativePath: zeta
                - relativePath: alpha
                - relativePath: mid
              push:
                - path: z
                - path: a
            """,
        "android-adbservers-null" to """
            type: Android
            adbServers: null
            """,
        "android-outputs-empty" to """
            type: Android
            outputs: []
            """,
        "android-outputs-testapp" to """
            type: Android
            outputs:
              - testApplication: test.apk
            """,
        "android-outputs-missing-testapp" to """
            type: Android
            outputs: [{}]
            """,
        "android-outputs-null-testapp" to """
            type: Android
            outputs:
              - testApplication: null
            """,
        "android-sdk-null" to """
            type: Android
            androidSdk: null
            """,
        "android-adbinit-null" to """
            type: Android
            adbInitTimeoutMillis: null
            """,
        "android-timeunit-lower" to """
            type: Android
            screenRecordConfiguration:
              videoConfiguration:
                timeLimitUnits: minutes
            """,
        "android-timeunit-ordinal" to """
            type: Android
            screenRecordConfiguration:
              videoConfiguration:
                timeLimitUnits: 4
            """,
        "android-video-timelimit-null" to """
            type: Android
            screenRecordConfiguration:
              videoConfiguration:
                timeLimit: null
            """,
        "ios-screenshot-upper" to """
            type: iOS
            screenRecordConfiguration:
              screenshotConfiguration:
                type: PNG
            """,
        "ios-perm-lifecycle" to """
            type: iOS
            permissions:
              grant: [user-tracking, calendar, user-tracking]
              lifecycle: BEFORE_EACH_BATCH
            """,
        "ios-queue-wc-null" to """
            type: iOS
            xcresult:
              queue:
                workerCapacity: null
            """,
        "ios-attempts-null" to """
            type: iOS
            workerConnection:
              attempts: null
            """,
        "ios-maxbatches-null" to """
            type: iOS
            maxBatchesWithoutProgress: null
            """,
        "stub" to """
            type: stub
            """,
        "stub-extra" to """
            type: stub
            foo: 1
            """,
        "empty" to """
            type: empty
            """,
        "empty-extra" to """
            type: empty
            foo: 1
            """,
        "ios-xctestrunenv" to """
            type: iOS
            xctestrunEnv:
              app: {A: "1"}
              test: {B: "2", B: "3"}
            """,
        "ios-signing-null" to """
            type: iOS
            signing:
              testRunnerProvisioningProfile: ~
            """,
        "local-parser-extra" to """
            type: Android
            testParserConfiguration:
              type: local
              foo: 1
            """,
        "remote-parser" to """
            type: Android
            testParserConfiguration:
              type: remote
              instrumentationArgs: {x: y}
            """,
        "apple-xctest-extra" to """
            type: iOS
            testParserConfiguration:
              type: xctest
              foo: 1
            """,
        "ios-lateJoinCutoff-num" to """
            type: iOS
            workerConnection:
              lateJoinCutoff: 1.5
            """,
        "ios-xcresult-null-sub" to """
            type: iOS
            xcresult:
              attachments: null
            """,
        "ios-bundle-null" to """
            type: iOS
            bundle: null
            """,
        "macos-xcodebuild-args" to """
            type: macOS
            xcodebuildTestArgs: {a: b}
            """,
        "ios-type-late" to """
            devices: x
            type: iOS
            """,
        "ios-screen-capture-format" to """
            type: iOS
            xcresult:
              preferredScreenCaptureFormat: screenRecording
            """,
        "bundle-tempdirfor" to """
            type: macOS
            bundle:
              tempDirFor: x
            """,
        "knownhosts-missing-validate" to """
            type: macOS
            """,
        "ios-pulling-lower" to """
            type: iOS
            xcresult:
              pullingPolicy: always
            """,
    )
}
