@file:OptIn(ExperimentalCoroutinesApi::class)

package oracle

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule
import com.fasterxml.jackson.module.kotlin.KotlinFeature
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.malinskiy.marathon.config.ExecutionCommand
import com.malinskiy.marathon.config.MarathonRunCommand
import com.malinskiy.marathon.config.ParseCommand
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.FlakinessStrategyConfiguration
import com.malinskiy.marathon.config.strategy.PoolingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.RetryStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ShardingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import com.malinskiy.marathon.device.DeviceProvider
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.factory.ConfigurationFactory
import com.malinskiy.marathon.test.setupMarathon
import com.malinskiy.marathon.time.Timer
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestCoroutineScheduler
import org.skyscreamer.jsonassert.JSONCompare
import org.skyscreamer.jsonassert.JSONCompareMode
import java.io.File
import kotlin.system.exitProcess

/**
 * `scenario <scenario.json> <work dir>`: runs one core scenario of the Kotlin marathon under the replay driver
 * (docs/design/replay-tapes.md, PLAN.md §6.5) and writes `<work dir>/output` (marathon's outputDir) and
 * `<work dir>/transcript.json`.
 *
 * The run is built as the core scenario tests build it (vendor-test's MarathonFactory and StubDeviceProvider) and runs on a
 * test dispatcher whose clock is the scheduler clock. The driver delivers the scenario's events one at a time, in order:
 * before an event it advances the scheduler clock to the event's time, and after it it runs every task due until none is
 * runnable and checks that every registered mailbox is empty (quiescence). Device calls (prepare, execute, dispose) and
 * the events are logged in the transcript with their scheduler time.
 *
 * With the patched build and `-Doracle.hooks.tape`, OracleHooks supplies the wall and monotonic clocks, the draws and the
 * IDs, and checks the mailboxes. With the reference build the same driver runs without them; the outputs then differ only
 * by the values the tape would have injected (tools/oracle/validate-patches.sh, check c).
 *
 * Exit status: 0 when the run followed the script and met the scenario's expectations, 1 otherwise, 2 for a usage error.
 */
fun scenario(args: List<String>) {
    if (args.size != 2) {
        System.err.println("usage: oracle scenario <scenario.json> <work dir>")
        exitProcess(2)
    }
    val scenarioFile = File(args[0])
    val work = File(args[1])
    if (work.exists() && !work.list().isNullOrEmpty()) {
        System.err.println("error: $work is not empty")
        exitProcess(2)
    }
    work.mkdirs()
    val spec = ScenarioSpec.parse(scenarioFile)
    val replay = Replay(spec, work, HooksBridge.load())
    val ok = replay.run()
    exitProcess(if (ok) 0 else 1)
}

private val mapper: ObjectMapper = ObjectMapper()
    .registerModule(KotlinModule.Builder().configure(KotlinFeature.SingletonSupport, true).build())
    .registerModule(JavaTimeModule())

/** The parsed scenario file; docs/design/replay-tapes.md §Event scripts specifies the format */
class ScenarioSpec(
    val name: String,
    val entry: String,
    val command: ExecutionCommand,
    val timer: String,
    val configuration: JsonNode,
    val tests: Map<String, Test>,
    val devices: List<DeviceSpec>,
    val events: List<JsonNode>,
    val expect: JsonNode?,
    val dir: File,
) {
    class DeviceSpec(val node: JsonNode) {
        val serial: String = node.text("serial")
        val behaviour: String = node.path("behaviour").asText("stub")
    }

    companion object {
        const val FORMAT = "marathon-replay/scenario@1"
        val EVENTS = setOf("connect", "disconnect", "prepare-done", "prepare-failed", "batch-result", "batch-failed", "end")

        fun parse(file: File): ScenarioSpec {
            val root = mapper.readTree(file)
            validateScenario(root)
            root.only(file.name, "format", "name", "source", "entry", "command", "timer", "configuration", "tests", "devices",
                "events", "expect")
            require(root.path("format").asText() == FORMAT) { "${file.name}: format must be $FORMAT" }
            val command = root.path("command").let { c ->
                when (val type = c.path("type").asText("run")) {
                    "run" -> MarathonRunCommand
                    "parse" -> ParseCommand(c.path("outputFileName").textValue(), c.path("includeFlakyTests").asBoolean(false))
                    else -> throw IllegalArgumentException("${file.name}: command.type $type is not run or parse")
                }
            }
            val tests = LinkedHashMap<String, Test>()
            root.path("tests").forEach { t ->
                t.only("tests[]", "pkg", "clazz", "method", "metaProperties")
                val meta = t.path("metaProperties").map { m ->
                    MetaProperty(m.text("name"), m.path("values").fields().asSequence().associate { (k, v) -> k to v.asText() })
                }
                val test = Test(t.text("pkg"), t.text("clazz"), t.text("method"), meta)
                val id = "${test.pkg}.${test.clazz}#${test.method}"
                require(tests.put(id, test) == null) { "duplicate test $id" }
            }
            val devices = root.path("devices").map { DeviceSpec(it) }
            val events = root.path("events").toList()
            var last = 0L
            events.forEachIndexed { i, e ->
                val type = e.text("event")
                require(type in EVENTS) { "${file.name}: events[$i]: unknown event $type" }
                val at = e.path("at").longValue()
                require(at >= last) { "${file.name}: events[$i]: at must be >= $last" }
                last = at
            }
            require(events.count { it.text("event") == "end" } == 1 && events.lastOrNull()?.text("event") == "end") { "${file.name}: the last event must be end" }
            val entry = root.path("entry").asText("runAsync")
            require(entry == "runAsync" || entry == "run") { "${file.name}: entry $entry is not runAsync or run" }
            return ScenarioSpec(
                root.text("name"), entry, command, root.path("timer").asText("system"), root.path("configuration"), tests,
                devices, events, root.get("expect"), file.absoluteFile.parentFile
            )
        }
    }
}

/** The mocked Timer of the Kotlin scenario tests that use one: `currentTimeMillis` counts 0, 1, 2…, the rest is Mockito's 0 */
private class CounterTimer : Timer {
    private var i = 0L
    override val startTimeMillis: Long = 0
    override val elapsedTimeMillis: Long get() = 0
    override fun currentTimeMillis(): Long = i++
    override fun measure(block: () -> Unit): Long = 0
}

class Replay(private val spec: ScenarioSpec, private val work: File, private val hooks: HooksBridge) {
    private val scheduler = TestCoroutineScheduler()
    private val dispatcher = StandardTestDispatcher(scheduler, name = "replay")
    private val scope = CoroutineScope(dispatcher + SupervisorJob())
    private val log = ArrayList<Map<String, Any?>>()
    private val errors = ArrayList<String>()
    private var providerChannel: Channel<DeviceProvider.DeviceEvent>? = null
    private val devices = LinkedHashMap<String, ReplayDevice>()
    private var completedAt: Long? = null
    private var consumedEvents = 0

    fun now(): Long = scheduler.currentTime

    fun record(entry: Map<String, Any?>) {
        log.add(linkedMapOf<String, Any?>("t" to now()) + entry)
    }

    fun error(message: String) {
        errors.add(message)
        record(mapOf("error" to message))
    }

    fun run(): Boolean {
        spec.devices.forEach { devices[it.serial] = ReplayDevice(it, spec.tests, this) }
        hooks.install(object : HooksBridge.Loop {
            override fun <T> run(label: String, block: suspend CoroutineScope.() -> T): T {
                record(mapOf("runBlocking" to label))
                val deferred = scope.async { block() }
                drive(deferred)
                @Suppress("UNCHECKED_CAST")
                return if (deferred.isCompleted) deferred.getCompleted() else throw IllegalStateException("the run has not finished")
            }
        }) { scheduler.currentTime }

        val outcome = linkedMapOf<String, Any?>()
        try {
            val marathon = build()
            when (spec.entry) {
                "runAsync" -> {
                    val deferred = scope.async { marathon.runAsync(spec.command) }
                    drive(deferred)
                    outcome += result(deferred)
                }
                else -> {
                    if (!hooks.active) throw IllegalStateException("entry run needs the patched build with a tape")
                    try {
                        outcome["completed"] = true
                        outcome["value"] = marathon.run(spec.command)
                    } catch (e: Throwable) {
                        outcome["completed"] = false
                        outcome["exception"] = e.javaClass.simpleName
                        outcome["message"] = e.message
                    }
                }
            }
        } catch (e: Throwable) {
            error("driver: ${e.javaClass.simpleName}: ${e.message}")
        } finally {
            scope.cancel()
        }
        if (consumedEvents != spec.events.size) error("unconsumed events: ${spec.events.size - consumedEvents}")
        outcome["completedAt"] = completedAt
        hooks.flush()

        val expectations = checkExpectations(outcome)
        val transcript = linkedMapOf(
            "format" to "marathon-replay/transcript@1",
            "scenario" to spec.name,
            "hooks" to hooks.active,
            "log" to log,
            "outcome" to outcome,
            "expectations" to expectations,
            "hooksReport" to hooks.report()?.let { mapper.readValue(it, Map::class.java) }?.let { sortedJson(it) },
            "errors" to errors,
        )
        File(work, "transcript.json").writeText(Json.pretty(transcript))
        val failed = expectations.filter { it["ok"] != true }
        failed.forEach { System.err.println("expectation failed: ${it["check"]}: ${it["detail"]}") }
        errors.forEach { System.err.println("error: $it") }
        return errors.isEmpty() && failed.isEmpty()
    }

    private fun build() = setupMarathon {
        val replay = this@Replay
        if (spec.timer == "counter") timer = CounterTimer()
        configuration {
            outputDir = File(work, "output")
            tests { replay.spec.tests.values.toList() }
            deviceProvider.context = replay.scope.coroutineContext
            devices { channel ->
                replay.providerChannel = channel
                replay.record(mapOf("subscribed" to true))
            }
            replay.configure(this)
        }
    }

    private fun configure(factory: ConfigurationFactory) {
        spec.configuration.fields().forEach { (key, value) ->
            when (key) {
                "name" -> factory.name = value.textValue()
                "uncompletedTestRetryQuota" -> factory.uncompletedTestRetryQuota = value.intValue()
                "testBatchTimeoutMillis" -> factory.testBatchTimeoutMillis = value.longValue()
                "testOutputTimeoutMillis" -> factory.testOutputTimeoutMillis = value.longValue()
                "deviceInitializationTimeoutMillis" -> factory.deviceInitializationTimeoutMillis = value.longValue()
                "ignoreFailures" -> factory.ignoreFailures = value.booleanValue()
                "strictMode" -> factory.strictMode = value.booleanValue()
                "includeSerialRegexes" -> factory.includeSerialRegexes = value.map { it.asText().toRegex() }
                "excludeSerialRegexes" -> factory.excludeSerialRegexes = value.map { it.asText().toRegex() }
                "retryStrategy" -> factory.retryStrategy = mapper.treeToValue(value, RetryStrategyConfiguration::class.java)
                "poolingStrategy" -> factory.poolingStrategy = mapper.treeToValue(value, PoolingStrategyConfiguration::class.java)
                "shardingStrategy" -> factory.shardingStrategy = mapper.treeToValue(value, ShardingStrategyConfiguration::class.java)
                "sortingStrategy" -> factory.sortingStrategy = mapper.treeToValue(value, SortingStrategyConfiguration::class.java)
                "batchingStrategy" -> factory.batchingStrategy = mapper.treeToValue(value, BatchingStrategyConfiguration::class.java)
                "flakinessStrategy" -> factory.flakinessStrategy =
                    mapper.treeToValue(value, FlakinessStrategyConfiguration::class.java)
                else -> throw IllegalArgumentException("configuration.$key is not supported by the replay driver")
            }
        }
    }

    /** Delivers the events in order, each at its time and followed by quiescence, until the end event */
    private fun drive(run: Deferred<*>) {
        run.invokeOnCompletion { completedAt = scheduler.currentTime }
        quiesce("start")
        for (event in spec.events) {
            consumedEvents++
            val at = event.path("at").asLong()
            if (at > now()) {
                scheduler.advanceTimeBy(at - now())
                quiesce("advance")
            }
            val type = event.text("event")
            val entry = linkedMapOf<String, Any?>("event" to type)
            event.get("device")?.let { entry["device"] = it.asText() }
            record(entry)
            if (type == "end") break
            deliver(type, event)
            quiesce(type)
        }
    }

    /** Runs every task due now until none is runnable, then checks the actor half of quiescence */
    private fun quiesce(after: String) {
        scheduler.runCurrent()
        val busy = hooks.busyMailboxes()
        if (busy.isNotEmpty()) error("not quiescent after $after: mailboxes $busy hold messages while no task is runnable")
    }

    private fun deliver(type: String, event: JsonNode) {
        val device = event.get("device")?.asText()?.let { devices[it] ?: return error("unknown device $it") }
        when (type) {
            "connect", "disconnect" -> {
                val channel = providerChannel ?: return error("$type before the scheduler subscribed to the provider")
                val message = if (type == "connect") DeviceProvider.DeviceEvent.DeviceConnected(device!!)
                else DeviceProvider.DeviceEvent.DeviceDisconnected(device!!)
                if (channel.trySend(message).isFailure) error("$type: the provider channel is closed")
            }
            else -> device!!.deliver(type, event)?.let { error(it) }
        }
    }

    private fun result(run: Deferred<*>): Map<String, Any?> {
        if (!run.isCompleted) return mapOf("completed" to false)
        val failure = run.getCompletionExceptionOrNull()
        return if (failure == null) mapOf("completed" to true, "value" to run.getCompleted())
        else mapOf("completed" to true, "exception" to failure.javaClass.simpleName, "message" to failure.message)
    }

    private fun checkExpectations(outcome: Map<String, Any?>): List<Map<String, Any?>> {
        val expect = spec.expect ?: return emptyList()
        val checks = ArrayList<Map<String, Any?>>()
        fun check(name: String, ok: Boolean, detail: String?) {
            checks.add(linkedMapOf("check" to name, "ok" to ok, "detail" to if (ok) null else detail))
        }
        expect.fields().forEach { (key, value) ->
            when (key) {
                "completed" -> check(key, outcome["completed"] == value.booleanValue(), "completed=${outcome["completed"]}")
                "value" -> check(key, outcome["value"] == value.booleanValue(), "value=${outcome["value"]}")
                "exception" -> check(key, outcome["exception"] == value.asText(), "exception=${outcome["exception"]}")
                "rawJson" -> {
                    val actual = File(work, "output/test_result/raw.json")
                    val expected = File(spec.dir, value.asText())
                    val detail = if (!actual.isFile) "no output/test_result/raw.json" else {
                        val r = JSONCompare.compareJSON(expected.readText(), actual.readText(), JSONCompareMode.LENIENT)
                        if (r.passed()) null else r.message
                    }
                    check("rawJson ${value.asText()}", detail == null, detail)
                }
                "files" -> value.fields().forEach { (path, text) ->
                    val actual = File(work, "output/$path")
                    val content = if (actual.isFile) actual.readText().trimIndent() else null
                    check("file $path", content == text.asText(), "content: $content")
                }
                else -> check(key, false, "unknown expectation")
            }
        }
        return checks
    }

    @Suppress("UNCHECKED_CAST")
    private fun sortedJson(value: Any?): Any? = when (value) {
        is Map<*, *> -> (value as Map<String, Any?>).toSortedMap().mapValues { sortedJson(it.value) }
        is List<*> -> value.map { sortedJson(it) }
        is Double -> if (value == Math.floor(value)) value.toLong() else JRaw(value.toString())
        else -> value
    }
}

internal fun JsonNode.text(field: String): String =
    get(field)?.takeIf { it.isTextual }?.asText() ?: throw IllegalArgumentException("missing string field $field in $this")

internal fun JsonNode.only(where: String, vararg keys: String) {
    fieldNames().asSequence().firstOrNull { it !in keys }?.let { throw IllegalArgumentException("$where: unknown key $it") }
}

/** Strict shared input schema; Jackson coercion is never used to validate replay inputs. */
private fun validateScenario(root: JsonNode) {
    fun obj(node: JsonNode, where: String) = require(node.isObject) { "$where must be an object" }
    fun array(node: JsonNode, where: String) = require(node.isArray) { "$where must be an array" }
    fun integer(node: JsonNode, where: String, max: Long = Long.MAX_VALUE) {
        require(node.isIntegralNumber && node.canConvertToLong() && node.longValue() in 0..max) {
            "$where must be a nonnegative integer <= $max"
        }
    }
    fun fields(node: JsonNode, strings: Set<String> = emptySet(), bools: Set<String> = emptySet(), ints: Set<String> = emptySet()) {
        node.fields().forEach { (key, value) ->
            when (key) {
                in strings -> require(value.isTextual) { "$key must be a string" }
                in bools -> require(value.isBoolean) { "$key must be a boolean" }
                in ints -> integer(value, key)
            }
        }
    }
    obj(root, "scenario")
    root.text("name")
    fields(root, setOf("format", "name", "source", "entry", "timer"))
    require(root.path("timer").asText("system") in setOf("system", "counter")) { "unknown timer" }
    root.get("command")?.let { c ->
        obj(c, "command"); c.only("command", "type", "outputFileName", "includeFlakyTests")
        fields(c, setOf("type", "outputFileName"), setOf("includeFlakyTests"))
        if (c.path("type").asText("run") == "run") c.only("command run", "type")
    }
    root.get("configuration")?.let { c ->
        obj(c, "configuration")
        val ints = setOf("uncompletedTestRetryQuota", "testBatchTimeoutMillis", "testOutputTimeoutMillis", "deviceInitializationTimeoutMillis")
        c.only("configuration", *(ints + setOf("name", "ignoreFailures", "strictMode", "includeSerialRegexes", "excludeSerialRegexes", "retryStrategy",
            "poolingStrategy", "shardingStrategy", "sortingStrategy", "batchingStrategy", "flakinessStrategy")).toTypedArray())
        fields(c, strings = setOf("name"), bools = setOf("ignoreFailures", "strictMode"), ints = ints)
        c.get("uncompletedTestRetryQuota")?.let { integer(it, "uncompletedTestRetryQuota", Int.MAX_VALUE.toLong()) }
        for (key in listOf("includeSerialRegexes", "excludeSerialRegexes")) c.get(key)?.let {
            array(it, key); require(it.all { v -> v.isTextual }) { "$key must contain strings" }
        }
    }
    root.get("expect")?.let { e ->
        obj(e, "expect")
        e.only("expect", "completed", "value", "exception", "rawJson", "files")
        fields(e, strings = setOf("exception", "rawJson"), bools = setOf("completed", "value"))
        e.get("files")?.let { files ->
            obj(files, "expect.files")
            require(files.all { it.isTextual }) { "expect.files values must be strings" }
        }
    }
    array(root.path("tests"), "tests")
    root.path("tests").forEach { t ->
        obj(t, "test"); t.text("pkg"); t.text("clazz"); t.text("method")
        t.get("metaProperties")?.let { meta ->
            array(meta, "metaProperties")
            meta.forEach { m ->
                obj(m, "metaProperty"); m.only("metaProperty", "name", "values"); m.text("name")
                obj(m.path("values"), "metaProperty.values")
                require(m.path("values").all { it.isTextual }) { "metadata values must be strings" }
            }
        }
    }
    array(root.path("devices"), "devices")
    val serials = HashSet<String>()
    root.path("devices").forEach { d ->
        obj(d, "device"); require(serials.add(d.text("serial"))) { "duplicate device ${d.text("serial")}" }
        d.only("device", "serial", "behaviour", "model", "manufacturer", "os", "abi", "healthy", "networkState",
            "prepareTimeMillis", "testTimeMillis", "crashWithTestBatchException", "crashWithDeviceLostException", "reportResultsBeforeCrash", "results")
        fields(d, setOf("serial", "behaviour", "model", "manufacturer", "os", "abi", "networkState"),
            setOf("healthy", "crashWithTestBatchException", "crashWithDeviceLostException", "reportResultsBeforeCrash"),
            setOf("prepareTimeMillis", "testTimeMillis"))
        require(d.path("behaviour").asText("stub") in setOf("stub", "scripted")) { "unknown device behaviour" }
        d.get("results")?.let { r -> obj(r, "device.results"); r.forEach { array(it, "statuses"); it.forEach { v -> require(v.isTextual); com.malinskiy.marathon.execution.TestStatus.valueOf(v.asText()) } } }
    }
    array(root.path("events"), "events")
    root.path("events").forEach { e ->
        obj(e, "event"); integer(e.path("at"), "event.at")
        val type = e.text("event")
        val extra = when (type) {
            "connect", "disconnect", "prepare-done" -> emptySet()
            "prepare-failed" -> setOf("message")
            "batch-result" -> setOf("results")
            "batch-failed" -> setOf("exception", "message", "reportEmptyResultsFirst")
            "end" -> emptySet()
            else -> throw IllegalArgumentException("unknown event $type")
        }
        e.only("event $type", *(setOf("at", "event") + (if (type == "end") emptySet() else setOf("device")) + extra).toTypedArray())
        fields(e, setOf("device", "message", "exception"), setOf("reportEmptyResultsFirst"))
        if (type != "end") require(e.text("device") in serials) { "unknown device ${e.text("device")}" }
        if (type == "batch-failed") require(e.text("exception") in setOf("TestBatchExecutionException", "DeviceLostException", "RuntimeException")) { "unknown exception" }
        if (type == "batch-result") {
            array(e.path("results"), "batch-result.results")
            e.path("results").forEach { r ->
                obj(r, "result"); r.only("result", "test", "status", "start", "end", "stacktrace")
                r.text("test"); com.malinskiy.marathon.execution.TestStatus.valueOf(r.text("status"))
                fields(r, setOf("test", "status", "stacktrace"), ints = setOf("start", "end"))
                if (r.has("start") && r.has("end")) require(r.path("end").longValue() >= r.path("start").longValue()) { "result end precedes start" }
            }
        }
    }
}
