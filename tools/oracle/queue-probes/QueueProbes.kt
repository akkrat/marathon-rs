// WP 3.7 oracle: drives the real QueueActor (reference distribution, e47352f2) with a recording pool channel, a
// recording Track, a real PoolProgressAccumulator, a scripted Timer, bundle identifier and random draws.
// One JSONL row per scenario: its inputs and, per message, the replies, the ordered events and the queue state.
import com.malinskiy.marathon.analytics.external.Analytics
import com.malinskiy.marathon.analytics.external.MetricsProvider
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.analytics.internal.pub.Tracker
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionMode
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import com.malinskiy.marathon.config.strategy.RetryStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.Device
import com.malinskiy.marathon.device.DeviceInfo
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.NetworkState
import com.malinskiy.marathon.device.OperatingSystem
import com.malinskiy.marathon.execution.DevicePoolMessage.FromQueue
import com.malinskiy.marathon.execution.TestBatchResults
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.TestShard
import com.malinskiy.marathon.execution.TestStatus
import com.malinskiy.marathon.execution.bundle.TestBundle
import com.malinskiy.marathon.execution.bundle.TestBundleIdentifier
import com.malinskiy.marathon.execution.progress.PoolProgressAccumulator
import com.malinskiy.marathon.execution.queue.QueueActor
import com.malinskiy.marathon.execution.queue.QueueMessage
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.time.Timer
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.job
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withTimeout
import java.io.File
import java.io.OutputStream
import java.io.PrintStream
import java.time.Instant
import java.util.PriorityQueue

// ---- JSON --------------------------------------------------------------------------------------------------
fun q(s: String?): String {
    if (s == null) return "null"
    val b = StringBuilder("\"")
    for (c in s) when {
        c == '"' -> b.append("\\\"")
        c == '\\' -> b.append("\\\\")
        c < ' ' -> b.append(String.format("\\u%04x", c.code))
        else -> b.append(c)
    }
    return b.append('"').toString()
}
fun t(test: Test) = "[${q(test.pkg)},${q(test.clazz)},${q(test.method)}]"
fun ts(tests: Iterable<Test>) = tests.joinToString(",", "[", "]") { t(it) }
fun obj(vararg kv: Pair<String, String>) = kv.joinToString(",", "{", "}") { "${q(it.first)}:${it.second}" }
fun arr(items: Iterable<String>) = items.joinToString(",", "[", "]")

val trace = mutableListOf<String>()
fun event(vararg parts: String) { trace += arr(parts.toList()) }
fun drain(): String { val s = arr(trace.toList()); trace.clear(); return s }

// Batch ids are random UUIDs; they are numbered in order of creation, which is the order of ExecuteBatch replies.
val batchIds = mutableMapOf<String, String>()
fun batchId(id: String) = batchIds.getOrPut(id) { if (id == "batch") id else "batch-${batchIds.count { it.value != "batch" } + 1}" }

fun field(o: Any, name: String, type: Class<*> = o.javaClass): Any? {
    val f = type.getDeclaredField(name)
    f.isAccessible = true
    return f.get(o)
}
fun setField(o: Any, name: String, value: Any?, type: Class<*> = o.javaClass) {
    val f = type.getDeclaredField(name)
    f.isAccessible = true
    f.set(o, value)
}

// ---- recording doubles ---------------------------------------------------------------------------------------
fun info(serial: String) = DeviceInfo(OperatingSystem("Fake OS"), serial, "fake model", "fake manufacturer", NetworkState.CONNECTED, emptyList(), true)

// TestBatchResults needs a Device the queue never reads.
val device: Device = java.lang.reflect.Proxy.newProxyInstance(Device::class.java.classLoader, arrayOf(Device::class.java)) { _, m, _ ->
    when (m.name) { "hashCode" -> 0; "equals" -> false; "toString" -> "device"; else -> null }
} as Device

class Metrics(val executionTimes: Map<Test, Double>) : MetricsProvider {
    override fun successRate(test: Test, limit: Instant) = 1.0
    override fun executionTime(test: Test, percentile: Double, limit: Instant) = executionTimes[test] ?: 1000.0
    override fun observeExecutionTimes(results: Collection<TestResult>) {
        event(q("observe"), arr(results.map { "[${t(it.test)},${q(it.status.name)}]" }))
    }
    override fun close() {}
}

val tracker = object : Tracker {
    override fun deviceConnected(poolId: DevicePoolId, device: DeviceInfo) {}
    override fun deviceDisconnected(poolId: DevicePoolId, device: DeviceInfo) {}
    override fun deviceProviderInit(serialNumber: String, startTime: Instant, finishTime: Instant) {}
    override fun devicePreparing(serialNumber: String, startTime: Instant, finishTime: Instant) {}
    override fun test(poolId: DevicePoolId, device: DeviceInfo, testResult: TestResult, final: Boolean) {
        event(q("track"), q(poolId.name), q(device.serialNumber), t(testResult.test), q(testResult.status.name),
              testResult.startTime.toString(), testResult.endTime.toString(), q(batchId(testResult.testBatchId)), final.toString())
    }
}

// The actor's logger: a KLogger proxy that records every message whatever its level.
fun logger(): Any {
    val type = Class.forName("mu.KLogger")
    return java.lang.reflect.Proxy.newProxyInstance(type.classLoader, arrayOf(type)) { _, m, args ->
        val level = m.name.uppercase()
        if (level in setOf("TRACE", "DEBUG", "INFO", "WARN", "ERROR") && args != null) {
            val message = when (val last = args.last()) {
                is Function0<*> -> last.invoke().toString()
                else -> args.firstOrNull { it is String }?.toString()
            }
            val cause = args.firstOrNull { it is Throwable } as Throwable?
            // Batch ids are random: the messages name them by their creation number.
            val normalized = batchIds.entries.fold(message) { text, (id, name) -> text?.replace(id, name) }
            event(q("log"), q(level), q(normalized), q(cause?.javaClass?.name))
            null
        } else when (m.returnType) {
            java.lang.Boolean.TYPE -> true
            String::class.java -> "QueueActor"
            else -> null
        }
    }
}

// The progress lines PoolProgressAccumulator prints.
val progressLine = Regex("^-?\\d+% \\| \\[.*")
val handlerLine = Regex("^\\d\\d:\\d\\d:\\d\\d\\.\\d{3} \\[[^\\]]+\\] (\\w+) +(QueueActor\\S*) - (.*)$")
class LineCapture : OutputStream() {
    private val line = java.io.ByteArrayOutputStream()
    override fun write(b: Int) {
        if (b == '\n'.code) {
            val text = line.toString(Charsets.UTF_8)
            line.reset()
            if (progressLine.matches(text)) event(q("stdout"), q(text))
            // The actor's exception handler logs through the logger it captured at construction (logback).
            handlerLine.find(text)?.let { event(q("handler"), q(it.groupValues[1]), q(it.groupValues[2]), q(it.groupValues[3])) }
        } else line.write(b)
    }
}

class ScriptedCache(val draws: ArrayDeque<Int>) : LinkedHashMap<Test, Int>() {
    override fun computeIfAbsent(key: Test, mappingFunction: java.util.function.Function<in Test, out Int>): Int {
        get(key)?.let { return it }
        val value = draws.removeFirst()
        event(q("draw"), t(key), value.toString())
        put(key, value)
        return value
    }
}

// ---- scenarios -------------------------------------------------------------------------------------------------
class Scenario(
    val name: String,
    val tests: List<Test>,
    val flaky: List<Test> = emptyList(),
    val sorting: String = "execution-time",
    val batching: String = "fixed-size:1",
    val retry: Pair<Int, Int>? = 10 to 3,
    val uncompletedQuota: Int = 3,
    val mode: ExecutionMode = ExecutionMode.ANY_SUCCESS,
    val fast: Boolean = false,
    val executionTimes: List<Pair<Test, Double>> = emptyList(),
    val draws: List<Int> = emptyList(),
    val timer: List<Long> = emptyList(),
    val bundleFailsOn: Test? = null,
    val steps: List<String>,
)

fun test(c: String, m: String) = Test("pkg", c, m, emptyList())
val slow = Test("pkg", "Placement", "slow", emptyList())
val fast = Test("pkg", "Placement", "fast", emptyList())
val placementTimes = listOf(slow to 2000.0, fast to 1000.0)
val t0 = test("C0", "m0")
val t1 = test("C1", "m1")
val t2 = test("C2", "m2")
val t3 = test("C3", "m3")
val t4 = test("C4", "m4")
val a1 = test("A", "m1")
val a2 = test("A", "m2")
val times3 = listOf(t0 to 3000.0, t1 to 2000.0, t2 to 1000.0)

fun req(device: String, worker: String?) = obj("op" to q("request"), "device" to q(device), "worker" to q(worker))
fun reqs(vararg r: Pair<String, String?>) = obj("op" to q("requests"), "requests" to arr(r.map { "[${q(it.first)},${q(it.second)}]" }))
// Results for the tests of the device's last batch, by position; the status picks the list.
fun done(device: String, vararg statuses: String) = obj("op" to q("completed"), "device" to q(device), "statuses" to arr(statuses.map(::q)))
fun results(device: String, finished: List<Pair<Test, String>> = emptyList(), failed: List<Pair<Test, String>> = emptyList(), uncompleted: List<Pair<Test, String>> = emptyList()) =
    obj("op" to q("completed"), "device" to q(device),
        "finished" to arr(finished.map { "[${t(it.first)},${q(it.second)}]" }),
        "failed" to arr(failed.map { "[${t(it.first)},${q(it.second)}]" }),
        "uncompleted" to arr(uncompleted.map { "[${t(it.first)},${q(it.second)}]" }))
fun ret(device: String, reason: String) = obj("op" to q("return"), "device" to q(device), "reason" to q(reason))
fun reject(device: String, batchOf: String = device) = obj("op" to q("reject"), "device" to q(device), "batchOf" to q(batchOf))
fun confirm(device: String) = obj("op" to q("confirm"), "device" to q(device))
val isEmpty = obj("op" to q("isEmpty"))
val terminate = obj("op" to q("terminate"))

val scenarios = listOf(
    Scenario("placement: a retry from another worker is left to its own worker only if nothing else is queued",
        listOf(slow, fast), listOf(slow), executionTimes = placementTimes, steps = listOf(
        req("serial-A1", "worker-A"), req("serial-A2", "worker-A"), req("serial-B1", "worker-B"),
        results("serial-A1", uncompleted = listOf(slow to "INCOMPLETE")), results("serial-A2", uncompleted = listOf(slow to "INCOMPLETE")),
        results("serial-B1", failed = listOf(fast to "FAILURE")),
        req("serial-C1", "worker-C"), req("serial-A3", "worker-A"), req("serial-B2", "worker-B"),
        done("serial-C1", "PASSED"), done("serial-A3", "PASSED"), done("serial-B2", "PASSED"))),
    Scenario("placement: devices that ask together leave a retry to a device of another worker",
        listOf(slow, fast), executionTimes = placementTimes, steps = listOf(
        req("serial-A1", "worker-A"), req("serial-B1", "worker-B"),
        results("serial-A1", failed = listOf(slow to "FAILURE")), results("serial-B1", failed = listOf(fast to "FAILURE")),
        reqs("serial-B2" to "worker-B", "serial-B3" to "worker-B", "serial-A2" to "worker-A"),
        done("serial-B2", "PASSED"), done("serial-A2", "PASSED"))),
    Scenario("placement: a rejected retry keeps its worker", listOf(slow, fast), executionTimes = placementTimes, steps = listOf(
        req("serial-A1", "worker-A"), results("serial-A1", failed = listOf(slow to "FAILURE")),
        req("serial-C1", "worker-C"), reject("serial-C1"), req("serial-A2", "worker-A"), confirm("serial-A2"), terminate, req("serial-A3", "worker-A"))),
    Scenario("placement: a device gets a retry from its own worker when nothing else is queued", listOf(fast), executionTimes = placementTimes, steps = listOf(
        req("serial-A1", "worker-A"), results("serial-A1", failed = listOf(fast to "FAILURE")), req("serial-A2", "worker-A"), done("serial-A2", "PASSED"))),
    Scenario("K1: a retried attempt that completes the test still queues it again", listOf(t0, t1), mode = ExecutionMode.ALL_SUCCESS, fast = true,
        executionTimes = times3, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "FAILURE"), isEmpty, req("serial-A1", "worker-A"), done("serial-A1", "PASSED"),
        req("serial-A1", "worker-A"), done("serial-A1", "PASSED"))),
    Scenario("K2: a second failed copy of a retried test is never ended", listOf(a1, a2), listOf(a1), batching = "class-name", retry = 1 to 3, steps = listOf(
        req("serial-A1", "worker-A"), results("serial-A1", finished = listOf(a2 to "PASSED"), failed = listOf(a1 to "FAILURE", a1 to "FAILURE")),
        req("serial-A1", "worker-A"), done("serial-A1", "PASSED"))),
    Scenario("K3: two retry copies of one test in a class-name batch", listOf(a1), listOf(a1), batching = "class-name", retry = null, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "INCOMPLETE", "INCOMPLETE"),
        req("serial-B1", "worker-B"), reject("serial-B1"), req("serial-A2", "worker-A"), done("serial-A2", "PASSED", "PASSED"))),
    Scenario("K3: copies of a retried test beside an original copy", listOf(a1, a2, t0), listOf(a1), batching = "class-name", retry = null,
        executionTimes = listOf(a1 to 3000.0, a2 to 2000.0, t0 to 1000.0), steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "INCOMPLETE", "PASSED", "INCOMPLETE"),
        req("serial-B1", "worker-A"), req("serial-B2", "worker-B"), reject("serial-B2"), req("serial-B3", "worker-C"),
        done("serial-B1", "PASSED"), done("serial-B3", "PASSED", "PASSED"))),
    Scenario("K4: a rejected batch queues a test that completed meanwhile", listOf(t0, t0), retry = null, fast = true, steps = listOf(
        req("serial-A1", null), req("serial-B1", null), done("serial-A1", "PASSED"), reject("serial-B1"), req("serial-A1", null), done("serial-A1", "PASSED"))),
    Scenario("reject of a batch that is no longer active", listOf(t0, t1, t2), executionTimes = times3, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "PASSED"), reject("serial-A1", "serial-A1"), req("serial-A1", "worker-A"),
        req("serial-B1", "worker-B"), reject("serial-A1", "serial-B1"), done("serial-A1", "PASSED"), done("serial-B1", "PASSED"))),
    Scenario("ReturnBatch at Long.MAX_VALUE", listOf(t0, t1), batching = "fixed-size:2", uncompletedQuota = 1, executionTimes = times3,
        timer = listOf(Long.MAX_VALUE, Long.MAX_VALUE - 1, 7, 8), steps = listOf(
        req("serial-A1", "worker-A"), ret("serial-A1", "device lost"), req("serial-A1", "worker-A"), ret("serial-A1", "again"))),
    Scenario("uncompleted quota exceeded with duplicate copies", listOf(a1), listOf(a1), batching = "class-name", uncompletedQuota = 1, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "INCOMPLETE", "INCOMPLETE"), req("serial-A1", "worker-A"), done("serial-A1", "INCOMPLETE", "INCOMPLETE"))),
    Scenario("class-name batching with flaky copies and mixed results", listOf(a1, a2, t0), listOf(a1, t0), batching = "class-name", retry = 2 to 2,
        executionTimes = listOf(a1 to 3000.0, a2 to 2000.0, t0 to 1000.0), steps = listOf(
        req("serial-A1", "worker-A"), req("serial-B1", "worker-B"), done("serial-A1", "FAILURE", "PASSED", "FAILURE"), done("serial-B1", "INCOMPLETE", "PASSED"),
        req("serial-B1", "worker-B"), req("serial-A1", "worker-A"), done("serial-B1", "PASSED"), done("serial-A1", "PASSED"), req("serial-A1", "worker-A"))),
    Scenario("RequestBatches when every request could only get its own retries", listOf(fast), executionTimes = placementTimes, steps = listOf(
        req("serial-A1", "worker-A"), results("serial-A1", failed = listOf(fast to "FAILURE")),
        reqs("serial-A2" to "worker-A", "serial-A3" to "worker-A"), done("serial-A2", "PASSED"))),
    Scenario("RequestBatches stops once the queue terminates", emptyList(), retry = null, steps = listOf(
        reqs("serial-A1" to null, "serial-A2" to null), req("serial-A3", null))),
    Scenario("a batching exception restores the copies set aside", listOf(t0, t1, t2), executionTimes = times3, bundleFailsOn = t1, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "FAILURE"), req("serial-A2", "worker-A"), req("serial-B1", "worker-B"))),
    Scenario("C5: a phantom AddRetry through the queue", listOf(t0, t1), batching = "fixed-size:2", retry = 1 to 3, executionTimes = times3, steps = listOf(
        req("serial-A1", "worker-A"), done("serial-A1", "FAILURE", "FAILURE"), req("serial-A1", "worker-A"), done("serial-A1", "PASSED"))),
    Scenario("random-order comparisons", listOf(t0, t1, t2, t3, t4), sorting = "random-order", batching = "fixed-size:2",
        draws = listOf(17, -4, 99, 3, 42, -60, 8, 0, 25, -11), steps = listOf(
        req("serial-A1", "worker-A"), req("serial-B1", "worker-B"), done("serial-A1", "FAILURE", "PASSED"), reject("serial-B1"),
        req("serial-A2", "worker-A"), done("serial-A2", "INCOMPLETE", "FAILURE"), req("serial-B2", "worker-B"), req("serial-C1", "worker-C"),
        done("serial-B2", "PASSED", "PASSED"), done("serial-C1", "PASSED", "PASSED"), req("serial-A2", "worker-A"), done("serial-A2", "PASSED"),
        req("serial-A2", "worker-A"), done("serial-A2", "PASSED"))),
    Scenario("execution-time comparisons with ConfirmOutcome and IsEmpty", listOf(t0, t1, t2, t3), listOf(t1), batching = "fixed-size:3",
        executionTimes = listOf(t0 to 500.0, t1 to 4000.0, t2 to 4000.0, t3 to 1500.0), timer = listOf(100, 101, 102), steps = listOf(
        req("serial-A1", "worker-A"), isEmpty, done("serial-A1", "FAILURE", "INCOMPLETE", "PASSED"), confirm("serial-A1"),
        req("serial-A1", "worker-A"), ret("serial-A1", "lost"), req("serial-B1", null), done("serial-B1", "PASSED", "PASSED", "PASSED"),
        req("serial-B1", null), done("serial-B1", "PASSED"), isEmpty)),
)

// ---- driver --------------------------------------------------------------------------------------------------
fun configuration(s: Scenario) = Configuration.Builder(name = "", outputDir = File("")).apply {
    vendorConfiguration = VendorConfiguration.StubVendorConfiguration
    debug = false
    analyticsTracking = false
    sortingStrategy = when (s.sorting) {
        "execution-time" -> SortingStrategyConfiguration.ExecutionTimeSortingStrategyConfiguration(90.0, Instant.EPOCH)
        "random-order" -> SortingStrategyConfiguration.RandomOrderStrategyConfiguration
        else -> SortingStrategyConfiguration.NoSortingStrategyConfiguration
    }
    batchingStrategy = when {
        s.batching == "class-name" -> BatchingStrategyConfiguration.ClassNameBatchingStrategyConfiguration
        s.batching == "isolate" -> BatchingStrategyConfiguration.IsolateBatchingStrategyConfiguration
        else -> BatchingStrategyConfiguration.FixedSizeBatchingStrategyConfiguration(size = s.batching.removePrefix("fixed-size:").toInt())
    }
    retryStrategy = s.retry?.let { RetryStrategyConfiguration.FixedQuotaRetryStrategyConfiguration(it.first, it.second) }
        ?: RetryStrategyConfiguration.NoRetryStrategyConfiguration
    uncompletedTestRetryQuota = s.uncompletedQuota
    executionStrategy = ExecutionStrategyConfiguration(s.mode, s.fast)
}.build()

fun reply(message: FromQueue, last: MutableMap<String, TestBatch>): String = when (message) {
    is FromQueue.Notify -> arr(listOf(q("Notify"), arr(message.retryWorkers.map(::q))))
    is FromQueue.NoBatch -> arr(listOf(q("NoBatch"), q(message.device.serialNumber)))
    is FromQueue.Terminated -> arr(listOf(q("Terminated")))
    is FromQueue.ExecuteBatch -> {
        last[message.device.serialNumber] = message.batch
        arr(listOf(q("ExecuteBatch"), q(message.device.serialNumber), q(batchId(message.batch.id)), ts(message.batch.tests)))
    }
    is FromQueue.OutcomeProcessed -> arr(listOf(q("OutcomeProcessed"), q(message.device.serialNumber)))
}

val stepPattern = Regex("\"op\":\"([a-zA-Z]+)\"")
fun str(json: String, key: String): String? {
    val m = Regex("\"$key\":(null|\"([^\"]*)\")").find(json) ?: return null
    return if (m.groupValues[1] == "null") null else m.groupValues[2]
}

@Suppress("UNCHECKED_CAST")
fun snapshot(actor: QueueActor): String {
    val queue = field(actor, "queue") as PriorityQueue<Test>
    val retryWorkers = field(actor, "retryWorkers") as Map<Test, List<String>>
    val activeBatches = field(actor, "activeBatches") as Map<String, TestBatch>
    return obj(
        "queue" to ts(queue.toArray().map { it as Test }),
        "retryWorkers" to arr(retryWorkers.map { "[${t(it.key)},${arr(it.value.map(::q))}]" }),
        "activeBatches" to arr(activeBatches.keys.map(::q)),
        "closed" to actor.isClosedForSend.toString(),
    )
}

@Suppress("UNCHECKED_CAST")
fun progress(accumulator: PoolProgressAccumulator): String {
    val tests = field(accumulator, "tests") as Map<String, Any>
    return arr(tests.entries.sortedBy { it.key }.map { (name, data) ->
        val machine = field(data, "state")!!
        val state = machine.javaClass.getMethod("getState").invoke(machine)
        val total = state.javaClass.getMethod("getTotal").invoke(state)
        "[${q(name)},${q(state.javaClass.simpleName)},$total]"
    })
}

// Rows of the steps' JSON hold only what the driver reads.
fun parseResults(json: String, key: String): List<Pair<Test, TestStatus>> {
    val start = json.indexOf("\"$key\":[")
    if (start < 0) return emptyList()
    var depth = 0
    var end = start + key.length + 3
    while (true) {
        when (json[end]) { '[' -> depth++; ']' -> { depth--; if (depth == 0) break } }
        end++
    }
    val section = json.substring(start, end + 1)
    return Regex("\\[\\[\"([^\"]*)\",\"([^\"]*)\",\"([^\"]*)\"\\],\"([A-Z_]+)\"\\]").findAll(section).map {
        Test(it.groupValues[1], it.groupValues[2], it.groupValues[3], emptyList()) to TestStatus.valueOf(it.groupValues[4])
    }.toList()
}

fun run(s: Scenario): String = runBlocking {
    trace.clear()
    batchIds.clear()
    val configuration = configuration(s)
    val poolId = DevicePoolId("pool")
    val shard = TestShard(s.tests, s.flaky)
    val pool = Channel<FromQueue>(Channel.UNLIMITED)
    val job = Job()
    val times = ArrayDeque(s.timer)
    val timer = object : Timer {
        override val startTimeMillis = 0L
        override val elapsedTimeMillis = 0L
        override fun currentTimeMillis(): Long { val v = times.removeFirst(); event(q("time"), v.toString()); return v }
        override fun measure(block: () -> Unit): Long { block(); return 0 }
    }
    val identifier = s.bundleFailsOn?.let { failing ->
        object : TestBundleIdentifier {
            override fun identify(test: Test): TestBundle? {
                event(q("identify"), t(test))
                if (test == failing) throw IllegalStateException("no bundle for ${test.method}")
                return null
            }
        }
    }
    val accumulator = PoolProgressAccumulator(poolId, shard, configuration, Track().add(tracker))
    val actor = QueueActor(configuration, shard, Analytics(Metrics(s.executionTimes.toMap())), pool, poolId, timer, identifier,
                           accumulator, job, Dispatchers.Unconfined)
    setField(actor, "logger", logger(), com.malinskiy.marathon.actor.Actor::class.java)
    // Rebuild the queue around a recording comparator (and scripted draws), as the constructor built it.
    val sorting = field(actor, "sortingStrategy")!!
    if (s.sorting == "random-order") setField(sorting, "cache", ScriptedCache(ArrayDeque(s.draws)))
    @Suppress("UNCHECKED_CAST")
    val original = (field(actor, "queue") as PriorityQueue<Test>).comparator() as Comparator<Test>
    val queue = PriorityQueue<Test>(Comparator { a, b -> event(q("compare"), t(a), t(b)); original.compare(a, b) })
    queue.addAll(s.tests + s.flaky)
    setField(actor, "queue", queue)
    val construction = drain()
    val last = mutableMapOf<String, TestBatch>()
    val stdout = System.out
    System.setOut(PrintStream(LineCapture(), true, "UTF-8"))
    val steps = s.steps.map { step ->
        val op = stepPattern.find(step)!!.groupValues[1]
        var answer: String? = null
        val deferred = CompletableDeferred<Boolean>()
        val message: QueueMessage = when (op) {
            "request" -> QueueMessage.RequestBatch(info(str(step, "device")!!), str(step, "worker"))
            "requests" -> QueueMessage.RequestBatches(Regex("\\[\"([^\"]+)\",(null|\"([^\"]*)\")\\]").findAll(step).map {
                QueueMessage.RequestBatch(info(it.groupValues[1]), it.groupValues[3].takeIf { _ -> it.groupValues[2] != "null" })
            }.toList())
            "completed" -> {
                val serial = str(step, "device")!!
                val d = info(serial)
                fun result(test: Test, status: TestStatus) = TestResult(test, d, "batch", status, 0, 1)
                if (step.contains("\"statuses\"")) {
                    val statuses = Regex("\"([A-Z_]+)\"").findAll(step.substringAfter("\"statuses\":")).map { TestStatus.valueOf(it.groupValues[1]) }.toList()
                    val tests = (last[serial] ?: error("no batch for $serial at $step in ${s.name}: ${drain()}")).tests
                    val all = tests.zip(statuses).map { (test, status) -> result(test, status) }
                    QueueMessage.Completed(d, TestBatchResults(device,
                        all.filter { it.status == TestStatus.PASSED || it.status == TestStatus.IGNORED },
                        all.filter { it.status == TestStatus.FAILURE }, all.filter { it.status == TestStatus.INCOMPLETE }))
                } else QueueMessage.Completed(d, TestBatchResults(device,
                    parseResults(step, "finished").map { result(it.first, it.second) },
                    parseResults(step, "failed").map { result(it.first, it.second) },
                    parseResults(step, "uncompleted").map { result(it.first, it.second) }))
            }
            "return" -> { val serial = str(step, "device")!!; QueueMessage.ReturnBatch(info(serial), last[serial]!!, str(step, "reason")!!) }
            "reject" -> QueueMessage.RejectBatch(info(str(step, "device")!!), last[str(step, "batchOf")!!]!!)
            "confirm" -> QueueMessage.ConfirmOutcome(info(str(step, "device")!!))
            "isEmpty" -> QueueMessage.IsEmpty(deferred)
            else -> QueueMessage.Terminate
        }
        val sent = actor.trySend(message).isSuccess
        // A barrier: IsEmpty is answered after every message sent before it, unless the queue ends first.
        val barrier = if (op == "isEmpty") deferred else CompletableDeferred()
        if (sent && op != "isEmpty") actor.trySend(QueueMessage.IsEmpty(barrier))
        if (sent) withTimeout(10_000) {
            select<Unit> {
                barrier.onAwait {}
                actor.coroutineContext.job.onJoin {}
            }
        }
        if (op == "isEmpty" && deferred.isCompleted) answer = deferred.getCompleted().toString()
        val replies = generateSequence { pool.tryReceive().getOrNull() }.map { reply(it, last) }.toList()
        obj("step" to step, "sent" to sent.toString(), "replies" to arr(replies), "isEmpty" to (answer ?: "null"),
            "events" to drain(), "state" to snapshot(actor))
    }
    System.setOut(stdout)
    job.cancel()
    obj(
        "case" to q(s.name), "sorting" to q(s.sorting), "batching" to q(s.batching),
        "retry" to (s.retry?.let { "[${it.first},${it.second}]" } ?: "null"), "uncompletedQuota" to s.uncompletedQuota.toString(),
        "mode" to q(s.mode.name), "fast" to s.fast.toString(), "tests" to ts(s.tests), "flaky" to ts(s.flaky),
        "executionTimes" to arr(s.executionTimes.map { "[${t(it.first)},${it.second}]" }), "draws" to arr(s.draws.map { it.toString() }),
        "timer" to arr(s.timer.map { it.toString() }), "bundleFailsOn" to (s.bundleFailsOn?.let(::t) ?: "null"),
        "construction" to construction, "steps" to arr(steps), "progress" to progress(accumulator),
    )
}

fun main(args: Array<String>) {
    val out = File(args[0])
    out.mkdirs()
    val rows = scenarios.map(::run)
    val header = obj(
        "table" to q("queue"),
        "description" to q("QueueActor driven message by message: replies, ordered events and queue state after each message"),
        "schema" to obj(
            "case" to q("scenario name"), "sorting" to q("execution-time (percentile 90, limit EPOCH) or random-order"),
            "batching" to q("fixed-size:N, class-name or isolate"), "retry" to q("[totalAllowedRetryQuota, retryPerTestQuota] or null for no-retry"),
            "uncompletedQuota" to q("uncompletedTestRetryQuota"), "mode" to q("execution mode"), "fast" to q("execution fast flag"),
            "tests" to q("shard tests"), "flaky" to q("shard flaky tests"), "executionTimes" to q("metrics executionTime per test, default 1000.0"),
            "draws" to q("random-order draw script"), "timer" to q("currentTimeMillis script"), "bundleFailsOn" to q("test whose identify throws"),
            "construction" to q("events of the queue construction (addAll)"),
            "steps" to q("message, whether trySend succeeded, replies, IsEmpty answer, events (compare, draw, identify, time, observe, log, stdout, track, handler) and state"),
            "progress" to q("accumulator states and totals at the end"),
        ),
        "rows" to rows.size.toString(), "jdk" to q(System.getProperty("java.runtime.version")),
        "generator" to q("tools/oracle/queue-probes/run"),
    )
    File(out, "queue.jsonl").writeText((listOf(header) + rows).joinToString("") { it + "\n" })
}
