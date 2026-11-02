import ch.qos.logback.classic.Level
import ch.qos.logback.classic.LoggerContext
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceList
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceListDeserializer
import com.malinskiy.marathon.apple.bin.xcrun.simctl.service.ApplicationService
import com.malinskiy.marathon.apple.bin.xcrun.xcodebuild.Xcodebuild
import com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.ResultBundleFormat
import com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.Xcresulttool
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.apple.model.*
import com.malinskiy.marathon.apple.test.TestRequest
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.apple.TimeoutConfiguration
import com.malinskiy.marathon.config.vendor.apple.ios.*
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.vendor.ios.xcrun.xcresulttool.ActionsInvocationRecord
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.runBlocking
import mu.KLogger
import net.schmizz.sshj.connection.ConnectionException
import org.slf4j.LoggerFactory
import java.io.File
import java.nio.charset.Charset
import java.time.Duration

/** One scripted answer: a result, or an exception thrown by execute. */
class Answer(val stdout: List<String> = emptyList(), val stderr: List<String> = emptyList(), val exit: Int? = 0, val throws: String? = null)

class Recording(private val answers: ArrayDeque<Answer>) : CommandExecutor {
    override val host: CommandHost = KotlinProcessHost("local", Charsets.UTF_8)
    override val logger: KLogger = mu.KotlinLogging.logger("probe.Recording")
    override val connected = true
    val requests = mutableListOf<Map<String, Any?>>()
    override suspend fun execute(command: List<String>, timeout: Duration, idleTimeout: Duration, env: Map<String, String>, workdir: String?, charset: Charset): CommandSession {
        requests.add(linkedMapOf("argv" to command, "timeout" to timeout.toString(), "idle" to idleTimeout.toString(), "env" to env.entries.map { listOf(it.key, it.value) }, "workdir" to workdir))
        val answer = answers.removeFirstOrNull() ?: Answer()
        when (answer.throws) {
            null -> Unit
            "connection" -> throw ConnectionException("scripted")
            else -> throw IllegalStateException("scripted")
        }
        return Session(CommandResult(answer.stdout, answer.stderr, answer.exit))
    }
    override fun close() = Unit
}

class Session(private val result: CommandResult) : CommandSession {
    override val stdout: ReceiveChannel<String> = Channel<String>().apply { close() }
    override val stderr: ReceiveChannel<String> = Channel<String>().apply { close() }
    override val exitCode = CompletableDeferred(result.exitCode)
    override val isAlive = false
    override suspend fun await() = result
    override suspend fun drain() = Unit
    override fun terminate() = Unit
    override fun kill() = Unit
    override fun close() = Unit
}

val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
val events = ListAppender<ILoggingEvent>()
fun emit(case: String, value: Any?) = println(gson.toJson(linkedMapOf("case" to case, "value" to value)))

fun describe(e: Throwable?): Any? = e?.let { linkedMapOf("class" to it.javaClass.name, "message" to it.message, "cause" to describe(it.cause)) }

val timeouts = TimeoutConfiguration(
    shell = Duration.ofSeconds(31), shellIdle = Duration.ofSeconds(32), screenshot = Duration.ofSeconds(11), video = Duration.ofSeconds(301),
    erase = Duration.ofSeconds(33), shutdown = Duration.ofSeconds(34), delete = Duration.ofSeconds(35), create = Duration.ofSeconds(36),
    boot = Duration.ofSeconds(37), install = Duration.ofSeconds(38), uninstall = Duration.ofSeconds(39), importMedia = Duration.ofSeconds(40),
    testDestination = Duration.ofMillis(41500), xcresulttool = Duration.ofSeconds(121),
)

/** Runs [call] against a fresh environment answering with [answers]; records requests, return/error and log events. */
fun probe(case: String, vararg answers: Answer, call: suspend (AppleBinaryEnvironment) -> Any?) = runBlocking {
    val executor = Recording(ArrayDeque(answers.toList()))
    val configuration = Configuration.Builder("probe", File("/tmp/probe-out")).apply { testBatchTimeoutMillis = 1_234_567; testOutputTimeoutMillis = 7_654 }.build()
    val gsonParser = GsonBuilder().registerTypeAdapter(SimctlDeviceList::class.java, SimctlDeviceListDeserializer()).create()
    val env = AppleBinaryEnvironment(executor, configuration, timeouts, gsonParser)
    events.list.clear()
    val outcome = try {
        linkedMapOf("return" to render(call(env)))
    } catch (e: Throwable) {
        linkedMapOf("error" to describe(e))
    }
    val logs = events.list.filter { it.loggerName.startsWith("com.malinskiy.marathon.apple") }.map {
        linkedMapOf("level" to it.level.toString(), "logger" to it.loggerName, "message" to it.formattedMessage,
                    "throwable" to it.throwableProxy?.let { t -> t.className + ": " + t.message })
    }
    emit(case, linkedMapOf("requests" to executor.requests, "outcome" to outcome, "logs" to logs))
}

fun render(value: Any?): Any? = when (value) {
    null, is String, is Boolean, is Number -> value
    is CommandResult -> linkedMapOf("stdout" to value.stdout, "stderr" to value.stderr, "exit" to value.exitCode)
    is XcodeVersion -> listOf(value.major, value.minor, value.patch)
    is List<*> -> value.map { render(it) }
    is Enum<*> -> value.name
    is File -> value.path
    is CommandSession -> "session"
    is Unit -> "unit"
    else -> gson.toJsonTree(value)
}

fun main(args: Array<String>) {
    val ctx = LoggerFactory.getILoggerFactory() as LoggerContext
    ctx.getLogger("ROOT").level = Level.DEBUG
    events.context = ctx; events.start(); ctx.getLogger("ROOT").addAppender(events)
    val fixtures = File(args[0])
    val ok = Answer(listOf(" out1 ", "out2"), listOf("err"), 0)
    val fail = Answer(listOf("o"), listOf("e"), 3)
    val nul = Answer(listOf("o"), listOf("e"), null)
    val conn = Answer(throws = "connection")
    val other = Answer(throws = "other")
    val R = "/tmp/r p"

    // Every simple wrapper: success, nonzero exit (critical throws / safe returns), transport failure, other failure.
    val forms: List<Pair<String, suspend (AppleBinaryEnvironment) -> Any?>> = listOf(
        "chmod" to { e -> e.chmod.makeExecutable("/a b/it's") },
        "codesign.identity" to { e -> e.codesign.getIdentityFor(R) },
        "codesign.team" to { e -> e.codesign.getDevelopmentTeam(R) },
        "codesign.sign.entitlements" to { e -> e.codesign.sign(R, "/e.plist", "ID") },
        "codesign.sign.preserve" to { e -> e.codesign.sign(R, null, "") },
        "csrutil" to { e -> e.csrutil.status() },
        "getconf" to { e -> e.getconf.getDarwinUserCacheDir() },
        "getconf.key" to { e -> e.getconf.get("") },
        "ioreg.udid" to { e -> e.ioreg.getUDID() },
        "ioreg.manufacturer" to { e -> e.ioreg.getManufacturer() },
        "ioreg.model" to { e -> e.ioreg.getModel() },
        "lipo.arch" to { e -> e.lipo.getArch(R) },
        "lipo.remove" to { e -> e.lipo.removeArch(R, Arch.arm64e) },
        "nm.list" to { e -> e.nm.list(R, linkedSetOf("b", "a b", "")) },
        "nm.list.empty" to { e -> e.nm.list(R, emptySet()) },
        "nm.swift" to { e -> e.nm.swiftTests(R) },
        "nm.objc" to { e -> e.nm.objectiveCTests(R) },
        "plistbuddy.set" to { e -> e.plistBuddy.set("/f.plist", ":K", "") },
        "plistbuddy.add" to { e -> e.plistBuddy.add("/f.plist", ":K", "string", "v w") },
        "sqlite3" to { e -> e.sqlite3.query("/d b", "select 'x'", sudo = true) },
        "swvers" to { e -> e.swvers.getVersion() },
        "systemprofiler" to { e -> e.systemProfiler.getProvisioningUdid() },
        "xcodeselect.path" to { e -> e.xcodeselect.getDeveloperPath() },
        "xcodeselect.swift5" to { e -> e.xcodeselect.getSwift5FallbackLibsDir() },
        "xcrun.platform" to { e -> e.xcrun.getSdkPlatformPath(Sdk.WATCH_SIMULATOR) },
        "xcrun.version" to { e -> e.xcrun.getSdkVersion(Sdk.MACOS) },
        "xcrun.xctest" to { e -> e.xcrun.getXctesttPath(Sdk.IPHONEOS) },
        "app.install" to { e -> e.xcrun.simctl.application.install("U", R) },
        "app.container" to { e -> e.xcrun.simctl.application.containerPath("U", "b.id", ApplicationService.ContainerType.GROUPS) },
        "app.terminate" to { e -> e.xcrun.simctl.application.terminateApplication("U", "b.id") },
        "app.uninstall" to { e -> e.xcrun.simctl.application.uninstallApplication("U", "b.id") },
        "sim.create" to { e -> e.xcrun.simctl.simulator.create("n m", "type") },
        "sim.create.runtime" to { e -> e.xcrun.simctl.simulator.create("n", "type", "rt") },
        "sim.boot" to { e -> e.xcrun.simctl.simulator.boot("U", linkedMapOf("B" to "2", "A" to "")) },
        "sim.getenv" to { e -> e.xcrun.simctl.simulator.getenv("U", "K") },
        "sim.bootstatus" to { e -> e.xcrun.simctl.simulator.monitorStatus("U") },
        "sim.shutdown" to { e -> e.xcrun.simctl.simulator.shutdown("U") },
        "sim.delete" to { e -> e.xcrun.simctl.simulator.delete("U") },
        "sim.delete.unavailable" to { e -> e.xcrun.simctl.simulator.deleteUnavailable() },
        "sim.erase.all" to { e -> e.xcrun.simctl.simulator.eraseAll() },
        "sim.erase.one" to { e -> e.xcrun.simctl.simulator.erase("U") },
        "sim.erase.empty" to { e -> e.xcrun.simctl.simulator.erase(emptyList()) },
        "sim.running.15" to { e -> e.xcrun.simctl.simulator.isRunning("U", "15.5") },
        "sim.running.16" to { e -> e.xcrun.simctl.simulator.isRunning("U", "16.0") },
        "io.screenshot" to { e -> e.xcrun.simctl.io.screenshot("U", R, Type.JPEG, Display.EXTERNAL, Mask.BLACK) },
        "io.video" to { e -> e.xcrun.simctl.io.recordVideo("U", R, Codec.HEVC, Display.INTERNAL, Mask.IGNORED, "/p id") },
        "media.add" to { e -> e.xcrun.simctl.mediaService.addMedia("U", R) },
        "privacy.grant" to { e -> e.xcrun.simctl.privacy.grant("U", Permission.LocationAlways, "b.id") },
        "privacy.revoke" to { e -> e.xcrun.simctl.privacy.revoke("U", Permission.PhotosAdd, "b.id") },
        "privacy.reset" to { e -> e.xcrun.simctl.privacy.reset("U", Permission.All, "b.id") },
        "privacy.resetAll" to { e -> e.xcrun.simctl.privacy.resetAll("U", Permission.ContactsLimited) },
        "spawn.default" to { e -> e.xcrun.simctl.spawn.spawn("U", arrayOf("a", "")) },
        "spawn.timeout" to { e -> e.xcrun.simctl.spawn.spawn("U", arrayOf(), Duration.ofMillis(1)) },
        "device.list" to { e -> e.xcrun.simctl.device.list() },
        "xcodebuild.version" to { e -> e.xcrun.xcodebuild.getVersion() },
        "xcresulttool.merge" to { e -> e.xcrun.xcresulttool.merge(R, listOf("/a", "")) },
        "xcresulttool.merge.none" to { e -> e.xcrun.xcresulttool.merge(R, emptyList()) },
    )
    for ((name, call) in forms) {
        for ((variant, answer) in listOf("ok" to ok, "fail" to fail, "null" to nul, "conn" to conn, "other" to other)) {
            probe("$name/$variant", answer, call = call)
        }
    }

    // Output interpretation.
    probe("codesign.identity/stdout", Answer(listOf("Executable=/x", "Authority=Apple Development: A (B)", "Authority=Apple"), emptyList(), 0)) { it.codesign.getIdentityFor(R) }
    probe("codesign.identity/stderr-only", Answer(emptyList(), listOf("Authority=Apple Development: A (B)"), 0)) { it.codesign.getIdentityFor(R) }
    probe("codesign.sign/identity-query", Answer(listOf("Authority=Q"), emptyList(), 0), Answer()) { it.codesign.sign(R, null) }
    probe("csrutil/single-line", Answer(listOf("System Integrity Protection status: disabled."), emptyList(), 0)) { it.csrutil.status() }
    probe("csrutil/line-disabled", Answer(listOf("x", "disabled"), emptyList(), 0)) { it.csrutil.status() }
    probe("lipo/archs", Answer(listOf(" x86_64  arm64 "), emptyList(), 0)) { it.lipo.getArch(R) }
    probe("lipo/multiline", Answer(listOf("x86_64", "arm64"), emptyList(), 0)) { it.lipo.getArch(R) }
    probe("lipo/unknown", Answer(listOf("arm64 arm64_32"), emptyList(), 0)) { it.lipo.getArch(R) }
    probe("lipo/empty", Answer(emptyList(), emptyList(), 0)) { it.lipo.getArch(R) }
    probe("nm.list/quote") { it.nm.list(R, setOf("a'b")) }
    probe("nm.swift/lines", Answer(listOf("  a  ", "", " ", "b"), emptyList(), 0)) { it.nm.swiftTests(R) }
    probe("nm.list/fail-blank", Answer(listOf(" "), listOf(""), 1)) { it.nm.list(R, setOf("x")) }
    probe("nm.list/fail-multiline", Answer(listOf("a", "  b"), listOf("c"), 1)) { it.nm.list(R, setOf("x")) }
    probe("nm.objc/fail-stderr", Answer(emptyList(), listOf("  nm: error"), 1)) { it.nm.objectiveCTests(R) }
    probe("systemprofiler/udid", Answer(listOf("Hardware:", "", "      Provisioning UDID: 0000-AB:CD ", "x"), emptyList(), 0)) { it.systemProfiler.getProvisioningUdid() }
    probe("systemprofiler/none", Answer(listOf("Hardware:"), emptyList(), 0)) { it.systemProfiler.getProvisioningUdid() }
    probe("xcodeselect.swift5/test-fails", Answer(listOf(" /Dev "), emptyList(), 0), Answer(exit = 1)) { it.xcodeselect.getSwift5FallbackLibsDir() }
    probe("xcodeselect.swift5/test-conn", Answer(listOf("/Dev"), emptyList(), 0), conn) { it.xcodeselect.getSwift5FallbackLibsDir() }
    probe("sim.create/udid", Answer(listOf("No runtime specified, using 'iOS 16.2'", " FE547FCD-A71D-4B09-BE3C-97C18FD0F667 ", "trailing words"), emptyList(), 0)) { it.xcrun.simctl.simulator.create("n", "t") }
    probe("sim.create/empty-line", Answer(listOf("FE547FCD-A71D", ""), emptyList(), 0)) { it.xcrun.simctl.simulator.create("n", "t") }
    probe("sim.create/none", Answer(listOf("lower-case"), emptyList(), 0)) { it.xcrun.simctl.simulator.create("n", "t") }
    probe("sim.getenv/value", Answer(listOf(" v ", ""), emptyList(), 0)) { it.xcrun.simctl.simulator.getenv("U", "K") }
    val springboard = listOf("\tA   com.apple.springboard.services", "x D com.apple.springboard.services")
    for (runtime in listOf("", "x", "15.9", "16", "16.0", "+16.1", "-1.0", "2147483648.0", "１６.0")) {
        for ((variant, answer) in listOf("ok" to Answer(springboard, emptyList(), 0), "fail" to Answer(springboard, emptyList(), 1),
                                         "nul" to Answer(springboard, emptyList(), null), "D" to Answer(listOf("D com.apple.springboard.services"), emptyList(), 0))) {
            probe("sim.running/$runtime/$variant", answer) { it.xcrun.simctl.simulator.isRunning("U", runtime) }
        }
    }
    probe("sim.running/blank-token", Answer(listOf("A  com.apple.springboard.services", "com.apple.springboard.services"), emptyList(), 0)) { it.xcrun.simctl.simulator.isRunning("U", "17.0") }

    // xcodebuild
    for (out in listOf(listOf("Xcode 14.2", "Build version 14C18"), listOf(" Xcode 15.0 "), listOf("Xcode"), listOf("Xcode 1.2.3.4"), listOf("Xcode a.b"), emptyList(), listOf("Xcode 16.", "x"))) {
        probe("xcodebuild.version/${out.joinToString("|")}", Answer(out, listOf("warning"), 0)) { it.xcrun.xcodebuild.getVersion() }
    }
    val tests = listOf(Test("com.x", "Cls", "m1", emptyList()), Test("", "Cls", "m 2", emptyList()), Test("com.x", "Cls", "m1", emptyList()))
    val user = linkedMapOf("-destination" to "user", "-xctestrun" to "/user.xctestrun", "-empty" to "", "-enableCodeCoverage" to "maybe", "-z" to "1")
    probe("xcodebuild.test/full") { it.xcrun.xcodebuild.testWithoutBuilding("U-1", Sdk.TV_SIMULATOR, TestRequest("/w d", "/x.xctestrun", true, tests, "/r.xcresult", "Target"), user) }
    probe("xcodebuild.test/minimal") { it.xcrun.xcodebuild.testWithoutBuilding("U", Sdk.MACOS, TestRequest("/w", "/x.xctestrun", false, emptyList()), emptyMap()) }
    probe("xcodebuild.test/null-tests") { it.xcrun.xcodebuild.testWithoutBuilding("U", Sdk.IPHONEOS, TestRequest("/w", "/x.xctestrun", false), linkedMapOf("-collect-test-diagnostics" to "never")) }
    probe("xcodebuild.test/conn", conn) { it.xcrun.xcodebuild.testWithoutBuilding("U", Sdk.IPHONEOS, TestRequest("/w", "/x", false), emptyMap()) }
    for (xcode in listOf(null, "13.4.1", "14.0", "16.0")) {
        val v = xcode?.let(XcodeVersion::from)
        emit("simulatorTestArgs/$xcode", listOf(
            Xcodebuild.simulatorTestArgs(linkedMapOf("-b" to "1", "-collect-test-diagnostics" to "on-failure", "-a" to ""), v).entries.map { listOf(it.key, it.value) },
            Xcodebuild.simulatorTestArgs(emptyMap(), v).entries.map { listOf(it.key, it.value) }))
    }

    // xcresulttool get
    val sample = File(fixtures, "vendor-apple-base/fixtures/xctesttool/sample_1.json").readText()
    for (xcode in listOf(null, "15.4", "16.0", "27.0")) {
        val v = xcode?.let(XcodeVersion::from)
        probe("xcresulttool.get/$xcode/json", Answer(sample.lines(), emptyList(), 0)) { it.xcrun.xcresulttool.get(ActionsInvocationRecord::class.java, R, v)?.issues?.testFailureSummaries?.size }
        probe("xcresulttool.get/$xcode/raw-id-version", Answer(listOf("  "), emptyList(), 0)) { it.xcrun.xcresulttool.get(ActionsInvocationRecord::class.java, R, v, ResultBundleFormat.RAW, "", "3") }
    }
    for ((variant, answer) in listOf("exit64" to Answer(sample.lines(), listOf("Error: deprecated", "Usage"), 64), "null-exit" to Answer(sample.lines(), listOf("e"), null),
                                     "conn" to conn, "other" to other, "truncated" to Answer(listOf(sample.substring(0, sample.length / 2)), emptyList(), 0),
                                     "json-null" to Answer(listOf(" null "), emptyList(), 0), "malformed" to Answer(listOf("{\"issues\": 5}"), emptyList(), 0),
                                     "blank" to Answer(listOf("", " \t"), emptyList(), 0), "trailing" to Answer(listOf("{} {"), emptyList(), 0))) {
        probe("xcresulttool.get/$variant", answer) { it.xcrun.xcresulttool.get(ActionsInvocationRecord::class.java, R, XcodeVersion(26, 6, 0)) != null }
    }
    for (shell in listOf(119L, 121L)) for (x in listOf(null, 5L)) {
        val t = if (x == null) TimeoutConfiguration(shell = Duration.ofSeconds(shell)) else TimeoutConfiguration(shell = Duration.ofSeconds(shell), xcresulttool = Duration.ofSeconds(x))
        emit("describeTimeout/$shell/$x", Xcresulttool.describeTimeout(t))
    }
    emit("describeTimeout/fractional", Xcresulttool.describeTimeout(TimeoutConfiguration(shell = Duration.ofMillis(120500))))

    // simctl list decoding
    for (name in listOf("list_output.json", "list_output_2.json", "list_output_3.json")) {
        val lines = File(fixtures, "vendor-apple-base/fixtures/simctl/$name").readLines()
        probe("device.list/$name", Answer(lines, emptyList(), 0)) { e ->
            val o = e.xcrun.simctl.device.list()
            linkedMapOf("devices" to o.devices.devices.map { listOf(it.runtime, it.state.name, it.name, it.udid, it.logPath, it.isAvailable, it.availabilityError, it.dataPath, it.deviceTypeIdentifier) },
                        "runtimes" to o.runtimes.map { listOf(it.bundle, it.buildVersion, it.isAvailable, it.runtimeRoot, it.name, it.identifier, it.version) },
                        "devicetypes" to o.devicetypes.map { listOf(it.name, it.identifier) })
        }
    }
    val shapes = listOf(
        "modern" to """{"devicetypes":[{"name":"iPhone","identifier":"t","productFamily":"iPhone"}],"runtimes":[{"bundlePath":"/b","buildversion":"22A","isAvailable":true,"runtimeRoot":"/r","name":"iOS 18","identifier":"rt","version":"18.0"}],"devices":{"rt":[{"state":"Booted","name":"A","udid":"U1","logPath":"/l","isAvailable":false,"dataPath":"/d","availabilityError":"e","deviceTypeIdentifier":"t"}]}}""",
        "order" to """{"devices":{"z":[{"state":"Shutdown","name":"Z","udid":"1"}],"a":[{"state":"ShuttingDown","name":"A","udid":"2"},{"state":"Bogus","name":"B","udid":"3"}],"z":[{"state":"Creating","name":"Z2","udid":"4"}]},"devicetypes":[],"runtimes":[]}""",
        "scalars" to """{"devicetypes":[{"name":7,"identifier":true}],"runtimes":[{"name":1.50,"version":null}],"devices":{"r":[{"state":3,"name":12,"udid":"u","isAvailable":"TRUE","logPath":1e3},{"state":null,"name":"n","udid":"u2","isAvailable":1},{"state":{"x":1},"name":"o","udid":"u3"},{"state":true,"name":"b","udid":"u4"},{"name":"nostate","udid":"u5"},{"state":"Booted","udid":"noname"}]}}""",
        "missing-lists" to """{"devices":{}}""",
        "null-lists" to """{"devicetypes":null,"runtimes":null,"devices":null}""",
        "null-name" to """{"devicetypes":[],"runtimes":[],"devices":{"r":[{"state":"Booted","name":null,"udid":"u"}]}}""",
        "object-name" to """{"devicetypes":[],"runtimes":[],"devices":{"r":[{"state":"Booted","name":{},"udid":"u"}]}}""",
        "group-object" to """{"devicetypes":[],"runtimes":[],"devices":{"r":{}}}""",
        "devices-array" to """{"devicetypes":[],"runtimes":[],"devices":[]}""",
        "runtime-object-field" to """{"devicetypes":[],"runtimes":[{"name":{}}],"devices":{}}""",
        "null-element" to """{"devicetypes":[null],"runtimes":[],"devices":{}}""",
        "syntax" to """{"devices":""",
        "trailing" to """{"devicetypes":[],"runtimes":[],"devices":{}} x""",
        "lenient" to """{devicetypes:[],'runtimes':[],devices:{}}""",
        "empty" to "",
        "json-null" to "null",
        "array-root" to "[]",
    )
    for ((name, json) in shapes) {
        probe("device.list/shape/$name", Answer(json.lines(), emptyList(), 0)) { e ->
            val o = e.xcrun.simctl.device.list()
            linkedMapOf("devices" to o.devices?.devices?.map { listOf(it.runtime, it.state?.name, it.name, it.udid, it.logPath, it.isAvailable, it.deviceTypeIdentifier) },
                        "runtimes" to o.runtimes?.map { it?.let { r -> listOf(r.bundle, r.buildVersion, r.isAvailable, r.name, r.version) } },
                        "devicetypes" to o.devicetypes?.map { it?.let { t -> listOf(t.name, t.identifier) } })
        }
        probe("device.listDevices/shape/$name", Answer(json.lines(), emptyList(), 0)) { e -> e.xcrun.simctl.device.listDevices().size }
    }

    // Versions
    for (v in listOf("14", " 14 . 2 ", "1.2.3", "1.2.3.4", "", "a", "1..2", "+1.-2", "2147483648", "１４.0", "1.2.x.4", "  15.4 ")) {
        emit("XcodeVersion/$v", runCatching { XcodeVersion.from(v).let { listOf(it.major, it.minor, it.patch) } }.fold({ it }, { describe(it) }))
    }
    for ((a, b) in listOf(listOf(14, 0, 0) to listOf(13, 9, 9), listOf(Int.MIN_VALUE, 0, 0) to listOf(1, 0, 0), listOf(0, 0, 0) to listOf(0, 0, 0), listOf(1, 2, Int.MAX_VALUE) to listOf(1, 2, -1))) {
        emit("XcodeVersion.compare/$a/$b", XcodeVersion(a[0], a[1], a[2]).compareTo(XcodeVersion(b[0], b[1], b[2])))
    }
    for (v in listOf("17", "17.4", "17.4.1", " 17.4", "17. 4", "1.2.3.4", "", "x.1", "17.x.y", "17..", "+17.-1", "１７")) {
        emit("AppleOperatingSystemVersion/$v", runCatching { AppleOperatingSystemVersion(v).let { listOf(it.major, it.minor, it.patch) } }.fold({ it }, { describe(it) }))
    }
    emit("AppleOperatingSystemVersion.compare", listOf(AppleOperatingSystemVersion("17.4") > AppleOperatingSystemVersion("17.3.9"), AppleOperatingSystemVersion("17") == AppleOperatingSystemVersion("17.0").let { null }, AppleOperatingSystemVersion("17").compareTo(AppleOperatingSystemVersion("17.0.0"))))
    emit("enums", linkedMapOf(
        "Sdk" to Sdk.values().map { listOf(it.name, it.value, it.platformName, it.destination, it.toString()) },
        "Arch" to Arch.values().map { listOf(it.name, it.value) },
        "OS" to OS.values().map { listOf(it.name, it.value) },
        "Platform" to Platform.values().map { listOf(it.name, it.value) },
        "ContainerType" to ApplicationService.ContainerType.values().map { listOf(it.name, it.value) },
        "ResultBundleFormat" to ResultBundleFormat.values().map { listOf(it.name, it.value) },
    ))

    // AppleTestBundle: cases are directories prepared by run.py under args[1].
    val bundles = File(args[1])
    for (case in bundles.listFiles()!!.sortedBy { it.name }) {
        val spec = File(case, "spec.txt").readLines().associate { it.substringBefore('=') to it.substringAfter('=') }
        fun f(key: String) = spec[key]?.takeIf { it.isNotEmpty() }?.let { if (it.startsWith("/")) File(it) else File(case, it) }
        val sdk = Sdk.valueOf(spec.getValue("sdk"))
        events.list.clear()
        val bundle = try {
            AppleTestBundle(f("app"), f("runner"), f("xctest")!!, sdk)
        } catch (e: Throwable) {
            emit("bundle/${case.name}", linkedMapOf("construct" to describe(e))); continue
        }
        fun <T> get(getter: () -> T): Any? = try { render(getter()) } catch (e: Throwable) { linkedMapOf("error" to describe(e)) }
        fun rel(p: Any?) = (p as? String)?.removePrefix(case.absolutePath + "/") ?: p
        val value = linkedMapOf<String, Any?>(
            "id" to rel(bundle.id), "appId" to bundle.appId, "testBundleId" to bundle.testBundleId,
            "testBinary" to rel(get { bundle.testBinary }), "testBinaryAgain" to rel(get { bundle.testBinary }),
            "testRunnerBinary" to rel(get { bundle.testRunnerBinary }), "applicationBinary" to rel(get { bundle.applicationBinary }),
            "relativeBinaryPath" to bundle.relativeBinaryPath.toList(), "relativeRootPath" to bundle.relativeRootPath.toList(), "relativeFrameworksPath" to bundle.relativeFrameworksPath.toList(),
        )
        File(case, "after-first-access.sh").takeIf { it.exists() }?.let {
            ProcessBuilder("sh", it.path).directory(case).inheritIO().start().waitFor()
            value["testBinaryRetry"] = rel(get { bundle.testBinary })
        }
        value["logs"] = events.list.filter { it.loggerName.startsWith("com.malinskiy.marathon.apple") }.map { listOf(it.level.toString(), it.loggerName, it.formattedMessage) }
        emit("bundle/${case.name}", value)
    }
}
