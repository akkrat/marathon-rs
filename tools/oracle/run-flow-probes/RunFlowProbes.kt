@file:OptIn(kotlinx.coroutines.DelicateCoroutinesApi::class)

package oracle.runflow

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import com.google.gson.Gson
import com.malinskiy.marathon.Marathon
import com.malinskiy.marathon.analytics.external.Analytics
import com.malinskiy.marathon.analytics.external.MetricsProvider
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.analytics.internal.sub.TrackerInternal
import com.malinskiy.marathon.analytics.internal.sub.Event as TrackEvent
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.ExecutionCommand
import com.malinskiy.marathon.config.FilteringConfiguration
import com.malinskiy.marathon.config.MarathonRunCommand
import com.malinskiy.marathon.config.ParseCommand
import com.malinskiy.marathon.config.TestFilterConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.Device
import com.malinskiy.marathon.device.DeviceProvider
import com.malinskiy.marathon.di.marathonStartKoin
import com.malinskiy.marathon.execution.LocalTestParser
import com.malinskiy.marathon.execution.RemoteTestParser
import com.malinskiy.marathon.execution.TestParser
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.bundle.TestBundle
import com.malinskiy.marathon.execution.bundle.TestBundleIdentifier
import com.malinskiy.marathon.execution.command.parse.MarathonTestParseCommand
import com.malinskiy.marathon.log.MarathonLogConfigurator
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.time.Timer
import com.malinskiy.marathon.usageanalytics.Event
import com.malinskiy.marathon.usageanalytics.tracker.UsageTracker
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.koin.core.context.stopKoin
import org.koin.dsl.module
import org.slf4j.LoggerFactory
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.PrintStream
import java.nio.file.Files
import java.time.Instant
import kotlin.system.exitProcess

/**
 * WP 3.10 probes of Marathon.kt, ShutdownHook.kt, MarathonTestParseCommand.kt and di/Modules.kt on the reference
 * build. `main(case, out)` runs one case in this JVM (logger levels are per-JVM state, as they are per process in Rust) and
 * prints one `ROW <json>` line. `signal <debug>` runs a run that never finishes for the parent to signal.
 */
const val MARATHON = "com.malinskiy.marathon.Marathon"
const val PARSE = "com.malinskiy.marathon.execution.command.parse.MarathonTestParseCommand"
const val TIMINGS = "Timings"

val trace = mutableListOf<String>()
var failSlot: String? = null

fun slot(name: String) {
    trace.add(name)
    if (failSlot == name) throw IllegalStateException("$name failed")
}

fun test(method: String) = Test("com.example", "SimpleTest", method, emptyList())

class Options(
    val debug: Boolean = true,
    val ignoreFailures: Boolean = false,
    val tests: List<Test> = listOf(test("method1")),
    val blockAll: Boolean = false,
    val markers: Boolean = false,
    val deviceInitializationTimeoutMillis: Long = 60_000,
    val parser: TestParser? = null,
    val announce: Boolean = false,
)

fun configuration(out: File, o: Options): Configuration = Configuration.Builder("probe", out).apply {
    vendorConfiguration = VendorConfiguration.StubVendorConfiguration
    debug = o.debug
    ignoreFailures = o.ignoreFailures
    analyticsTracking = false
    deviceInitializationTimeoutMillis = o.deviceInitializationTimeoutMillis
    if (o.blockAll) {
        filteringConfiguration = FilteringConfiguration(
            blocklist = listOf(TestFilterConfiguration.SimpleClassnameFilterConfiguration(regex = ".*".toRegex()))
        )
    }
}.build()

class Provider(private val out: File, private val o: Options) : DeviceProvider {
    override suspend fun initialize() {
        trace.add("initialize")
        if (o.markers) File(out, "provider-marker").apply { parentFile.mkdirs() }.writeText("provider")
    }

    override suspend fun borrow(): Device {
        trace.add("borrow")
        throw IllegalStateException("no device to borrow")
    }

    override suspend fun terminate() {
        if (o.announce) announce("terminate")
        slot("terminate")
    }

    override fun subscribe(): Channel<DeviceProvider.DeviceEvent> {
        trace.add("subscribe")
        if (o.announce) announce("ready")
        return Channel()
    }
}

fun announce(what: String) {
    println("PROBE $what")
    System.out.flush()
}

class Parser(private val out: File, private val o: Options) : LocalTestParser {
    override suspend fun extract(): List<Test> {
        trace.add("extract")
        if (o.markers) File(out, "parser-marker").apply { parentFile.mkdirs() }.writeText("parser")
        return o.tests
    }
}

class Metrics(private val o: Options) : MetricsProvider {
    override fun successRate(test: Test, limit: Instant) = 0.0
    override fun executionTime(test: Test, percentile: Double, limit: Instant) = 0.0
    override fun observeExecutionTimes(results: Collection<TestResult>) = Unit
    override fun close() {
        if (o.announce) announce("analytics.close")
        slot("analytics.close")
    }
}

object ZeroTimer : Timer {
    override val startTimeMillis = 0L
    override val elapsedTimeMillis get() = 0L
    override fun currentTimeMillis() = 0L
    override fun measure(block: () -> Unit): Long {
        block()
        return 0L
    }
}

fun marathon(out: File, o: Options): Marathon {
    val configuration = configuration(out, o)
    return Marathon(
        configuration,
        Provider(out, o),
        null,
        o.parser ?: Parser(out, o),
        object : MarathonLogConfigurator {
            override fun configure() {
                trace.add("configure")
            }
        },
        object : TrackerInternal {
            override fun track(event: TrackEvent) = Unit
            override fun close() {
                if (o.announce) announce("tracker.close")
                slot("tracker.close")
            }
        },
        Analytics(Metrics(o)),
        Track(),
        ZeroTimer,
        MarathonTestParseCommand(configuration.outputDir),
        object : UsageTracker {
            override fun trackEvent(event: Event) = Unit
            override fun meta(version: String, vendor: String, releaseMode: String) = Unit
            override fun close() {
                if (o.announce) announce("usage.close")
                slot("usage.close")
            }
        },
    )
}

class Capture {
    private val appenders = listOf(MARATHON, PARSE, TIMINGS).associateWith { name ->
        ListAppender<ILoggingEvent>().also {
            it.start()
            (LoggerFactory.getLogger(name) as Logger).addAppender(it)
        }
    }

    // P1: the JVM's java.*/os.* property lines after `System Information:` are not ported; drop them.
    private val property = Regex("^(java|os)[^:]*: ")

    fun events(): List<Map<String, Any?>> = appenders.flatMap { (name, appender) ->
        appender.list.filterNot { name == MARATHON && property.containsMatchIn(it.formattedMessage) }.map {
            linkedMapOf(
                "logger" to name,
                "level" to it.level.toString(),
                "message" to it.formattedMessage,
                "throwable" to it.throwableProxy?.className,
                "throwableMessage" to it.throwableProxy?.message,
            )
        }
    }

    fun levels(): Map<String, String?> = listOf(MARATHON, PARSE, TIMINGS)
        .associateWith { (LoggerFactory.getLogger(it) as Logger).level?.toString() }
}

fun manifest(root: File): List<String>? {
    if (!root.isDirectory) return null
    return root.walkTopDown().filter { it != root }.map {
        val rel = it.relativeTo(root).invariantSeparatorsPath
        if (it.isDirectory) "$rel/" else rel
    }.sorted().toList()
}

fun prepare(out: File) {
    out.mkdirs()
    File(out, "old.txt").writeText("old")
    File(out, "old-dir").mkdirs()
    File(out, "old-dir/nested.txt").writeText("nested")
}

fun outcome(block: () -> Any?): Map<String, Any?> = try {
    mapOf("value" to block())
} catch (e: Throwable) {
    mapOf("exception" to e.javaClass.name, "message" to e.message)
}

fun runCase(case: String, root: File): Map<String, Any?> {
    val out = File(root, "output")
    val row = linkedMapOf<String, Any?>("case" to case)
    fun record(o: Options, entry: (Marathon) -> Any?) {
        val capture = Capture()
        row += outcome { entry(marathon(out, o)) }
        row["trace"] = trace.toList()
        row["events"] = capture.events()
        row["levels"] = capture.levels()
        row["manifest"] = manifest(out)
    }
    when (case) {
        "wipe-run" -> {
            prepare(out); record(Options(blockAll = true, markers = true)) { runBlocking { it.runAsync(MarathonRunCommand) } }
        }
        "wipe-parse-named" -> {
            prepare(out); record(Options(markers = true)) { runBlocking { it.runAsync(ParseCommand("named")) } }
            row["named.yml"] = File(out, "named.yml").readText()
        }
        "parse-unnamed-debug-false" -> record(Options(debug = false)) { runBlocking { it.runAsync(ParseCommand(null)) } }
        "parse-unnamed-debug-true" -> record(Options(debug = true)) { runBlocking { it.runAsync(ParseCommand(null)) } }
        "parse-empty-name" -> {
            record(Options()) { runBlocking { it.runAsync(ParseCommand("")) } }
            row["file"] = File(out, ".yml").takeIf { it.isFile }?.readText()
        }
        "no-tests-runasync" -> {
            prepare(out); record(Options(tests = emptyList(), markers = true)) { runBlocking { it.runAsync(MarathonRunCommand) } }
        }
        "filtered-empty-parse" -> record(Options(blockAll = true)) { runBlocking { it.runAsync(ParseCommand("filtered")) } }
        "run-no-tests-ignore-false" -> record(Options(tests = emptyList())) { it.run() }
        "run-no-tests-ignore-true" -> record(Options(tests = emptyList(), ignoreFailures = true)) { it.run() }
        "run-no-devices-ignore-false" -> record(Options(deviceInitializationTimeoutMillis = 50)) { it.run() }
        "run-no-devices-ignore-true" -> record(Options(deviceInitializationTimeoutMillis = 50, ignoreFailures = true)) { it.run() }
        "run-no-devices-debug-false" -> record(Options(debug = false, deviceInitializationTimeoutMillis = 50)) { it.run() }
        "run-filtered-empty" -> record(Options(blockAll = true)) { it.run() }
        "run-parse" -> record(Options()) { it.run(ParseCommand("p")) }
        "finish-fail-analytics", "finish-fail-terminate", "finish-fail-tracker", "finish-fail-usage" -> {
            failSlot = mapOf(
                "finish-fail-analytics" to "analytics.close", "finish-fail-terminate" to "terminate",
                "finish-fail-tracker" to "tracker.close", "finish-fail-usage" to "usage.close",
            )[case]
            record(Options(blockAll = true)) { it.run() }
        }
        "exceptional-finish-fail" -> {
            failSlot = "terminate"
            val err = ByteArrayOutputStream()
            val original = System.err
            System.setErr(PrintStream(err, true))
            try {
                record(Options(tests = emptyList())) { it.run() }
            } finally {
                System.setErr(original)
            }
            row["stderrExceptions"] = err.toString().lineSequence()
                .filter { it.startsWith("Exception in thread") || it.contains("CompletionHandlerException") }.toList()
        }
        "remote-retry" -> record(Options(parser = object : RemoteTestParser<DeviceProvider> {
            override suspend fun extract(device: Device): List<Test> = emptyList()
        })) { runBlocking { it.runAsync(MarathonRunCommand) } }
        "remote-timeout" -> {
            val o = Options(deviceInitializationTimeoutMillis = 50, parser = object : RemoteTestParser<DeviceProvider> {
                override suspend fun extract(device: Device): List<Test> = emptyList()
            })
            val capture = Capture()
            val provider = object : DeviceProvider {
                override suspend fun initialize() = Unit
                override suspend fun borrow(): Device {
                    trace.add("borrow")
                    delay(Long.MAX_VALUE)
                    throw IllegalStateException("unreachable")
                }
                override suspend fun terminate() = Unit
                override fun subscribe() = Channel<DeviceProvider.DeviceEvent>()
            }
            val m = marathon(out, o)
            val field = Marathon::class.java.getDeclaredField("deviceProvider").apply { isAccessible = true }
            field.set(m, provider)
            row += outcome { runBlocking { m.runAsync(MarathonRunCommand) } }
            row["trace"] = trace.toList()
            row["events"] = capture.events()
        }
        "koin-missing-parser", "koin-missing-log-configurator", "koin-missing-bundle-identifier" -> {
            val o = Options()
            val configuration = configuration(out, o)
            val vendor = module {
                single<DeviceProvider> { Provider(out, o) }
                if (case != "koin-missing-bundle-identifier") single<TestBundleIdentifier> {
                    object : TestBundleIdentifier {
                        override fun identify(test: Test): TestBundle? = null
                    }
                }
                if (case != "koin-missing-parser") single<TestParser> { Parser(out, o) }
                if (case != "koin-missing-log-configurator") single<MarathonLogConfigurator> {
                    object : MarathonLogConfigurator {
                        override fun configure() = Unit
                    }
                }
            }
            val koin = marathonStartKoin(configuration, listOf(vendor))
            try {
                koin.koin.get<Marathon>()
                row["value"] = "constructed"
            } catch (e: Throwable) {
                val chain = generateSequence(e as Throwable?) { it.cause }.map { mapOf("class" to it.javaClass.name, "message" to it.message) }
                row["chain"] = chain.toList()
            } finally {
                stopKoin()
            }
        }
        "parse-command-zero", "parse-command-one", "parse-command-several", "parse-command-several-flaky" -> {
            val three = listOf(test("method1"), test("method2"), test("method3"))
            val (tests, flaky) = when (case) {
                "parse-command-zero" -> emptyList<Test>() to emptyList()
                "parse-command-one" -> listOf(test("method1")) to emptyList()
                "parse-command-several" -> three to emptyList()
                else -> three to listOf(test("method1"), test("method2"))
            }
            MarathonTestParseCommand(root).execute(tests, flaky, "result")
            row["content"] = File(root.absolutePath, "result.yml").readText()
        }
        else -> throw IllegalArgumentException("unknown case $case")
    }
    return row
}

fun main(args: Array<String>) {
    if (args[0] == "signal") {
        val debug = args[1].toBoolean()
        val root = Files.createTempDirectory("run-flow-signal").toFile()
        val m = marathon(File(root, "output"), Options(debug = debug, announce = true, deviceInitializationTimeoutMillis = 600_000))
        val result = m.run()
        announce("returned $result")
        exitProcess(0)
    }
    val root = File(args[1])
    val row = runCase(args[0], root)
    println("ROW " + Gson().toJson(row))
    System.out.flush()
    exitProcess(0)
}
