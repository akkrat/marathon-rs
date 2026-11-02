package oracle

import ch.qos.logback.classic.Level
import ch.qos.logback.classic.LoggerContext
import ch.qos.logback.classic.PatternLayout
import ch.qos.logback.classic.pattern.TargetLengthBasedClassNameAbbreviator
import ch.qos.logback.classic.spi.LoggingEvent
import ch.qos.logback.classic.spi.ThrowableProxy
import com.malinskiy.marathon.apple.test.TestFailed
import com.malinskiy.marathon.apple.test.TestIgnored
import com.malinskiy.marathon.apple.test.TestPassed
import com.malinskiy.marathon.apple.test.TestStarted
import com.malinskiy.marathon.config.AnalyticsConfiguration
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.FilteringConfiguration
import com.malinskiy.marathon.config.ScreenRecordingPolicy
import com.malinskiy.marathon.config.TestFilterConfiguration
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionMode
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import com.malinskiy.marathon.config.strategy.FlakinessStrategyConfiguration
import com.malinskiy.marathon.config.strategy.PoolingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.RetryStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ShardingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.DeviceInfo
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.NetworkState
import com.malinskiy.marathon.device.OperatingSystem
import com.malinskiy.marathon.extension.escape
import com.malinskiy.marathon.io.FileManager
import com.malinskiy.marathon.io.FileType
import com.malinskiy.marathon.io.FolderType
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.test.Test
import java.io.File
import java.time.Duration
import java.time.Instant
import java.util.TimeZone

/** The jcompat tables that serve WP 1.5 (Kotlin toString), WP 1.6 (logback) and WP 1.7 (escape, FileManager). */
object JcompatTables2 {
    fun generate(dir: File) {
        fileManager().writeTo(dir)
        FileManagerEdges.generate(dir)
        logback().writeTo(dir)
        kotlinToString(dir).writeTo(dir)
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // escape() and FileManager

    /** A fixed output directory, so that the path-length truncation is the same on every run. */
    private const val OUTPUT = "/tmp/oracle-file-manager/out"

    private fun fileManager(): JsonlTable {
        val table = JsonlTable(
            "file_manager",
            "core extension String.escape() (replace [^a-zA-Z0-9.#] with -) and io.FileManager: file names from tests, " +
                "batches and devices, filename truncation (maxFilename) and absolute path truncation (maxPath), with the " +
                "output directory fixed at $OUTPUT (FileManager creates the directories).",
            linkedMapOf(
                "op" to "escape | createFile | createFolder",
                "input" to "escape: the string",
                "maxPath" to "createFile/createFolder: FileManager maxPath (0 = unlimited)",
                "maxFilename" to "createFile/createFolder: FileManager maxFilename (0 = unlimited)",
                "args" to "createFile: {fileType, pool, serial, test: [pkg, clazz, method] | null, testBatchId, id, chunk}; " +
                    "createFolder: {folderType, pool, serial}",
                "result" to "escape: the string; otherwise {ok: absolute path} or {error}",
            ),
        )
        listOf(
            "com.example.FooTest#testBar", "com.example.FooTest#test bar[1]", "emulator-5554", "127.0.0.1:5555", "a/b\\c", "",
            "пакет.Класс#метод", "😀", "a\tb\nc", "x.y#z-w_v", "UPPER.lower#123", "(param=1, other=\"x\")",
        ).forEach { table.add(linkedMapOf("op" to "escape", "input" to it, "result" to it.escape())) }

        File(OUTPUT).parentFile.deleteRecursively()
        try {
            val tests = listOf(
                Test("com.example", "FooTest", "testBar", emptyList()),
                Test("", "BarTest", "test with spaces[0]", emptyList()),
                Test("com.example.very.long.package.name.that.goes.on", "AVeryLongTestClassNameForTruncation", "testAVeryLongMethodNameThatKeepsGoingAndGoing", emptyList()),
            )
            fun device(serial: String) = DeviceInfo(OperatingSystem("14"), serial, "m", "x", NetworkState.CONNECTED, emptyList(), true)
            val limits = listOf(0 to 255, 0 to 20, 60 to 255, 100 to 40, 1024 to 255)
            for ((maxPath, maxFilename) in limits) {
                val fm = FileManager(maxPath, maxFilename, File(OUTPUT))
                for (fileType in listOf(FileType.TEST, FileType.LOG, FileType.VIDEO, FileType.SCREENSHOT_PNG, FileType.DEVICE_INFO, FileType.BILL)) {
                    for (serial in listOf("emulator-5554", "127.0.0.1:5555")) {
                        val calls = mutableListOf<Triple<Test?, String?, Pair<String?, String?>>>()
                        tests.forEach { calls += Triple(it, null, null to null) }
                        calls += Triple(tests[0], "batch-1", "id1" to "3")
                        calls += Triple(null, "batch-2", null to null)
                        calls += Triple(null, "batch-3", "id2" to "7")
                        calls += Triple(null, null, null to null)
                        calls += Triple(null, null, "id3" to null)
                        for ((test, batch, idChunk) in calls) {
                            val (id, chunk) = idChunk
                            table.add(
                                linkedMapOf(
                                    "op" to "createFile", "maxPath" to maxPath, "maxFilename" to maxFilename,
                                    "args" to linkedMapOf(
                                        "fileType" to fileType.name, "pool" to "omni", "serial" to serial,
                                        "test" to test?.let { listOf(it.pkg, it.clazz, it.method) }, "testBatchId" to batch,
                                        "id" to id, "chunk" to chunk,
                                    ),
                                    "result" to attempt {
                                        fm.createFile(fileType, DevicePoolId("omni"), device(serial), test, batch, id, chunk).absolutePath
                                    },
                                )
                            )
                        }
                    }
                }
                for (folderType in FolderType.values()) {
                    for ((pool, serial) in listOf(null to null, "omni" to null, "omni" to "127.0.0.1:5555", "a-pool-with-a-long-name" to "emulator-5554")) {
                        table.add(
                            linkedMapOf(
                                "op" to "createFolder", "maxPath" to maxPath, "maxFilename" to maxFilename,
                                "args" to linkedMapOf("folderType" to folderType.name, "pool" to pool, "serial" to serial),
                                "result" to attempt {
                                    fm.createFolder(folderType, pool?.let { DevicePoolId(it) }, serial?.let { device(it) }).absolutePath
                                },
                            )
                        )
                    }
                }
            }
        } finally {
            File(OUTPUT).parentFile.deleteRecursively()
        }
        return table
    }

    private fun attempt(block: () -> Any?): Map<String, Any?> = try {
        mapOf("ok" to block())
    } catch (t: Throwable) {
        mapOf("error" to linkedMapOf("class" to t.javaClass.name, "message" to t.message))
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // logback

    private val PATTERNS = listOf(
        "%d{HH:mm:ss.SSS} [%thread] %-5level %logger{36} - %msg%n",
        "%highlight(%.-1level %d{HH:mm:ss.SSS} [%thread] <%logger{40}> %msg%n)",
        "%highlight(%.-1level [%thread] <%logger{48}> %msg%n)",
    )

    private fun logback(): JsonlTable {
        val table = JsonlTable(
            "logback",
            "logback-classic 1.5.8 PatternLayout with marathon's three patterns (cli/src/main/resources/logback.xml; " +
                "AndroidLogConfigurator; AppleLogConfigurator compact and normal) over synthetic events, with the JVM default " +
                "time zone set to UTC; and TargetLengthBasedClassNameAbbreviator, which %logger{N} uses. %highlight writes ANSI " +
                "escapes; the vendor appenders go through jansi, which may strip them when stdout is not a terminal (not covered).",
            linkedMapOf(
                "op" to "layout | abbreviate",
                "pattern" to "layout: the pattern",
                "event" to "layout: {level, logger, thread, timeMillis, message, args, throwable: null | {class, message, frames, cause}}",
                "logger" to "abbreviate: the logger name",
                "length" to "abbreviate: N of %logger{N}",
                "output" to "the layout's output, or the abbreviation",
            ),
        )
        val saved = TimeZone.getDefault()
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
        try {
            val context = LoggerContext()
            val layouts = PATTERNS.map { p -> PatternLayout().apply { pattern = p; this.context = context; start() } }
            val throwable = IllegalStateException("boom", RuntimeException("cause")).apply {
                stackTrace = arrayOf(StackTraceElement("com.example.Foo", "bar", "Foo.kt", 12), StackTraceElement("com.example.Main", "main", "Main.kt", 3))
                cause!!.stackTrace = arrayOf(StackTraceElement("com.example.Baz", "qux", "Baz.kt", 7))
            }
            data class Ev(val level: Level, val logger: String, val thread: String, val time: Long, val message: String, val args: List<Any?>?, val t: Throwable?)
            fun throwableInput(t: Throwable): Map<String, Any?> = linkedMapOf(
                "class" to t.javaClass.name, "message" to t.message,
                "frames" to t.stackTrace.map { it.toString() }, "cause" to t.cause?.let { throwableInput(it) }
            )
            val events = listOf(
                Ev(Level.INFO, "com.malinskiy.marathon.cli.ApplicationView", "main", 0L, "Starting marathon v0.10.1", null, null),
                Ev(Level.DEBUG, "com.malinskiy.marathon.execution.queue.QueueActor", "DefaultDispatcher-worker-1", 1_760_270_400_123L, "Queue {} with {} tests", listOf("omni", 3), null),
                Ev(Level.WARN, "FileManager", "pool-1-thread-2 @coroutine#5", 86_399_999L, "warning ü", null, null),
                Ev(Level.ERROR, "com.malinskiy.marathon.apple.ios.AppleSimulatorProvider", "main", 1_700_000_000_000L, "failed", null, throwable),
                Ev(Level.TRACE, "a.b.c.D", "t", 1L, "trace {} {}", listOf(null, 1.5), null),
                Ev(Level.INFO, "ROOT", "main", 999L, "multi\nline", null, null),
            )
            for (e in events) {
                val event = LoggingEvent().apply {
                    level = e.level
                    loggerName = e.logger
                    threadName = e.thread
                    timeStamp = e.time
                    message = e.message
                    argumentArray = e.args?.toTypedArray()
                    setLoggerContext(context)
                    e.t?.let { setThrowableProxy(ThrowableProxy(it)) }
                }
                PATTERNS.zip(layouts).forEach { (p, layout) ->
                    table.add(
                        linkedMapOf(
                            "op" to "layout", "pattern" to p,
                            "event" to linkedMapOf(
                                "level" to e.level.toString(), "logger" to e.logger, "thread" to e.thread, "timeMillis" to e.time,
                                "message" to e.message, "args" to e.args?.map { it?.toString() },
                                "throwable" to e.t?.let { throwableInput(it) },
                            ),
                            "output" to layout.doLayout(event),
                        )
                    )
                }
            }
            val names = listOf(
                "com.malinskiy.marathon.cli.ApplicationView", "com.malinskiy.marathon.execution.queue.QueueActor",
                "com.malinskiy.marathon.apple.ios.AppleSimulatorProvider", "com.malinskiy.marathon.android.adam.AdamAndroidDevice",
                "FileManager", "a.b.c", "a.b.c.D", "com.example.Outer\$Inner", "com..double.Dot", ".leading", "trailing.", "ROOT",
                "com.malinskiy.marathon.apple.logparser.parser.TestRunProgressParser", "x".repeat(60),
            )
            for (n in names) for (len in listOf(0, 1, 5, 10, 15, 16, 20, 25, 30, 36, 40, 48, 100)) {
                val output = if (len == 0) n.substringAfterLast('.') else TargetLengthBasedClassNameAbbreviator(len).abbreviate(n)
                table.add(linkedMapOf("op" to "abbreviate", "logger" to n, "length" to len, "output" to output))
            }
        } finally {
            TimeZone.setDefault(saved)
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // Kotlin data-class toString

    private val IDENTITY_HASH = Regex("@[0-9a-f]{1,8}\\b")

    fun generateWriterModels(dir: File) { kotlinToString(dir) }

    private fun kotlinToString(dir: File): JsonlTable {
        val writers = JsonlTable("kotlin_writer_models", "Lossless Kotlin writer inputs and JVM strings", mapOf("input" to "KotlinValue tree; raw scalar overrides remain text", "toString" to "JVM result"))
        val table = JsonlTable(
            "kotlin_to_string",
            "Kotlin toString of the values marathon prints: Configuration.toMap() (logged and sent to analytics), the " +
                "TestEvent data classes of the vendor-apple .expected fixtures, and data-class edge cases. Kotlin objects " +
                "without a toString print an identity hash, which reads @<hash>.",
            linkedMapOf(
                "id" to "row id",
                "description" to "what the row covers",
                "toString" to "toString(), or for toMap rows the map of key -> value",
            ),
        )
        fun norm(s: String) = IDENTITY_HASH.replace(s, "@<hash>")
        val base = Configuration.Builder(name = "sample", outputDir = File("/out/marathon"))
        val full = base.copy().apply {
            analyticsConfiguration = AnalyticsConfiguration.GraphiteConfiguration("graphite.local", 2003, "marathon")
            poolingStrategy = PoolingStrategyConfiguration.ComboPoolingStrategyConfiguration(
                listOf(PoolingStrategyConfiguration.ModelPoolingStrategyConfiguration, PoolingStrategyConfiguration.AbiPoolingStrategyConfiguration)
            )
            shardingStrategy = ShardingStrategyConfiguration.CountShardingStrategyConfiguration(2)
            sortingStrategy = SortingStrategyConfiguration.ExecutionTimeSortingStrategyConfiguration(90.0, Instant.parse("2026-01-01T00:00:00Z"))
            batchingStrategy = BatchingStrategyConfiguration.FixedSizeBatchingStrategyConfiguration(5, null, 0.5, null, 2)
            flakinessStrategy = FlakinessStrategyConfiguration.ProbabilityBasedFlakinessStrategyConfiguration(0.7, 3, Instant.EPOCH)
            retryStrategy = RetryStrategyConfiguration.FixedQuotaRetryStrategyConfiguration(10, 2)
            filteringConfiguration = FilteringConfiguration(
                listOf(TestFilterConfiguration.SimpleClassnameFilterConfiguration(Regex(".*Test")), TestFilterConfiguration.TestPackageFilterConfiguration(values = listOf("a", "b"))),
                listOf(TestFilterConfiguration.FragmentationFilterConfiguration(1, 3)),
            )
            executionStrategy = ExecutionStrategyConfiguration(ExecutionMode.ALL_SUCCESS, false)
            includeSerialRegexes = listOf(Regex("emulator-.*"), Regex("(a|b)"))
            screenRecordingPolicy = ScreenRecordingPolicy.ON_ANY
            vendorConfiguration = VendorConfiguration.AndroidConfigurationBuilder().apply {
                androidSdk = File("/opt/android-sdk")
                applicationOutput = File("app.apk")
                instrumentationArgs = linkedMapOf("debug" to "false", "x" to "a, b=c")
            }.build()
        }
        for ((id, builder) in listOf("toMap-default" to base, "toMap-full" to full)) {
            val config = builder.build()
            val names = mapOf("filtering" to "filteringConfiguration", "pooling" to "poolingStrategy", "sharding" to "shardingStrategy", "sorting" to "sortingStrategy", "batching" to "batchingStrategy", "flakiness" to "flakinessStrategy", "retry" to "retryStrategy")
            val properties = config::class.members.filterIsInstance<kotlin.reflect.KProperty1<Any, *>>().associateBy { it.name }
            writers.add(linkedMapOf("id" to id, "inputs" to config.toMap().mapValues { (key, _) ->
                val value = properties[names[key] ?: key]!!.get(config)
                kotlinWriterInput(if (key == "outputDir") (value as File).absolutePath else value)
            }, "toString" to config.toMap().mapValues { (_, v) -> norm(v) }))
            table.add(
                linkedMapOf(
                    "id" to id, "description" to "Configuration.toMap()",
                    "toString" to builder.build().toMap().mapValues { (_, v) -> norm(v) },
                )
            )
        }
        val test = Test("sample_appUITests", "FlakyTests", "testTextFlaky1", emptyList())
        val rows = listOf(
            Triple("TestStarted", "a .expected line", TestStarted(test)),
            Triple("TestPassed", "a .expected line", TestPassed(test, 1537187691585, 1537187696000)),
            Triple("TestFailed", "a .expected line with a multi-line trace", TestFailed(test, 1L, 2L, "a\nb, c=(d)")),
            Triple("TestFailed-null", "null trace", TestFailed(test, 1L, 2L, null)),
            Triple("TestIgnored", "TestIgnored", TestIgnored(test, 0L, 0L)),
            Triple("Test-meta", "Test with meta properties", Test("p", "C", "m", listOf(MetaProperty("A"), MetaProperty("B", linkedMapOf("v" to 1, "n" to null, "l" to listOf(1.0, "x")))))),
            Triple("Duration-Instant-File", "a data class holding java types", AnalyticsConfiguration.InfluxDb2Configuration("u", "t", "o", "b")),
            Triple("object", "a Kotlin object without toString", SortingStrategyConfiguration.NoSortingStrategyConfiguration),
            Triple("misc", "Pair/Triple, Char, Float, Double, nested collections, arrays",
                listOf(1 to 'c', Triple(1.0f, 1e7, -0.0), mapOf("k" to listOf(setOf(1))), arrayOf(1, 2), Duration.ofMillis(1500), File("rel/x"), Regex("a|b"))),
        )
        for ((id, description, value) in rows) {
            writers.add(linkedMapOf("id" to id, "input" to kotlinWriterInput(value), "toString" to norm(value.toString())))
            table.add(linkedMapOf("id" to id, "description" to description, "toString" to norm(value.toString())))
        }
        writers.writeTo(dir)
        return table
    }
}
