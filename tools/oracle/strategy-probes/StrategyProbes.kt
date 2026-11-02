// WP 3.2 JVM probes: the e47352f2 strategies (core jar of the reference installDist) over scripted inputs.
// Every row carries its inputs and the recorded JVM behaviour; the Rust tests replay the inputs and compare.
// Doubles are written as Java Double.toString strings so NaN/Infinity survive JSON.
import com.malinskiy.marathon.analytics.external.Analytics
import com.malinskiy.marathon.analytics.external.MetricsProvider
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionMode
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import com.malinskiy.marathon.config.strategy.FlakinessStrategyConfiguration
import com.malinskiy.marathon.config.strategy.RetryStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.DeviceInfo
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.NetworkState
import com.malinskiy.marathon.device.OperatingSystem
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.TestShard
import com.malinskiy.marathon.execution.TestStatus
import com.malinskiy.marathon.execution.bundle.TestBundle
import com.malinskiy.marathon.execution.bundle.TestBundleIdentifier
import com.malinskiy.marathon.execution.progress.PoolProgressAccumulator
import com.malinskiy.marathon.execution.strategy.SortingStrategy
import com.malinskiy.marathon.execution.strategy.impl.retry.fixedquota.FixedQuotaRetryStrategy
import com.malinskiy.marathon.extension.toBatchingStrategy
import com.malinskiy.marathon.extension.toFlakinessStrategy
import com.malinskiy.marathon.extension.toRetryStrategy
import com.malinskiy.marathon.extension.toSortingStrategy
import com.malinskiy.marathon.test.Test
import java.io.File
import java.time.Instant
import java.util.PriorityQueue

// ---- JSON --------------------------------------------------------------------------------------------------
fun q(s: String): String {
    val b = StringBuilder("\"")
    for (c in s) when {
        c == '"' -> b.append("\\\"")
        c == '\\' -> b.append("\\\\")
        c < ' ' -> b.append(String.format("\\u%04x", c.code))
        else -> b.append(c)
    }
    return b.append('"').toString()
}
fun d(v: Double) = q(java.lang.Double.toString(v))
fun t(test: Test) = "[${q(test.pkg)},${q(test.clazz)},${q(test.method)}]"
fun ts(tests: Iterable<Test>) = tests.joinToString(",", "[", "]") { t(it) }
fun obj(vararg kv: Pair<String, String>) = kv.joinToString(",", "{", "}") { "${q(it.first)}:${it.second}" }
fun arr(items: Iterable<String>) = items.joinToString(",", "[", "]")

val rows = mutableListOf<String>()
val trace = mutableListOf<String>()
fun event(vararg parts: String) { trace += arr(parts.toList()) }
fun drain(): String { val s = arr(trace.toList()); trace.clear(); return s }

fun test(i: Int) = Test("pkg$i", "clazz$i", "method$i", emptyList())
fun tests(n: Int) = (0 until n).map(::test)
fun classTest(c: Int, m: Int, pkg: String = "pkg") = Test(pkg, "clazz$c", "method$m", emptyList())

val LIMIT: Instant = Instant.parse("2026-01-01T00:00:00Z")

class RecordingMetrics(
    val successRates: MutableMap<Test, Double> = mutableMapOf(),
    val executionTimes: MutableMap<Test, Double> = mutableMapOf(),
    val successRate: Double = 0.5,
    val executionTime: Double = 1500.0,
) : MetricsProvider {
    override fun successRate(test: Test, limit: Instant): Double {
        event(q("successRate"), t(test))
        return successRates[test] ?: successRate
    }
    override fun executionTime(test: Test, percentile: Double, limit: Instant): Double {
        event(q("executionTime"), t(test))
        return executionTimes[test] ?: executionTime
    }
    override fun close() {}
    fun json() = obj(
        "successRates" to arr(successRates.map { "[${t(it.key)},${d(it.value)}]" }),
        "executionTimes" to arr(executionTimes.map { "[${t(it.key)},${d(it.value)}]" }),
        "successRate" to d(successRate),
        "executionTime" to d(executionTime),
    )
}

fun recording(inner: Comparator<Test>) = Comparator<Test> { a, b ->
    event(q("compare"), t(a), t(b))
    inner.compare(a, b)
}

// RandomOrderSortingStrategy draws Random.nextInt() in cache.computeIfAbsent; the probe swaps the cache for a map
// that draws from a script instead, keeping the call site and its order.
class ScriptedCache(val draws: ArrayDeque<Int>) : LinkedHashMap<Test, Int>() {
    override fun computeIfAbsent(key: Test, mappingFunction: java.util.function.Function<in Test, out Int>): Int {
        get(key)?.let { return it }
        val value = draws.removeFirst()
        event(q("draw"), t(key), value.toString())
        put(key, value)
        return value
    }
}

fun scriptDraws(strategy: SortingStrategy, draws: List<Int>) {
    val field = strategy.javaClass.getDeclaredField("cache")
    field.isAccessible = true
    field.set(strategy, ScriptedCache(ArrayDeque(draws)))
}

// ---- (a) sorting -------------------------------------------------------------------------------------------
sealed class Op {
    data class Offer(val test: Test) : Op()
    data class AddAll(val tests: List<Test>) : Op()
    object Poll : Op()
    data class Remove(val test: Test) : Op()
    data class RemoveAll(val tests: List<Test>) : Op()
    data class SetExecutionTime(val test: Test, val value: Double) : Op()
    data class SetSuccessRate(val test: Test, val value: Double) : Op()
    object Reprocess : Op()
}

fun opJson(op: Op) = when (op) {
    is Op.Offer -> obj("op" to q("offer"), "test" to t(op.test))
    is Op.AddAll -> obj("op" to q("addAll"), "tests" to ts(op.tests))
    Op.Poll -> obj("op" to q("poll"))
    is Op.Remove -> obj("op" to q("remove"), "test" to t(op.test))
    is Op.RemoveAll -> obj("op" to q("removeAll"), "tests" to ts(op.tests))
    is Op.SetExecutionTime -> obj("op" to q("setExecutionTime"), "test" to t(op.test), "value" to d(op.value))
    is Op.SetSuccessRate -> obj("op" to q("setSuccessRate"), "test" to t(op.test), "value" to d(op.value))
    Op.Reprocess -> obj("op" to q("reprocess"))
}

fun sorting(name: String, strategyJson: String, cnf: SortingStrategyConfiguration, metrics: RecordingMetrics, draws: List<Int>, ops: List<Op>) {
    val strategy = cnf.toSortingStrategy()
    if (draws.isNotEmpty()) scriptDraws(strategy, draws)
    val input = metrics.json()
    var queue = PriorityQueue(recording(strategy.process(metrics)))
    val steps = ops.map { op ->
        val result = when (op) {
            is Op.Offer -> queue.offer(op.test).toString()
            is Op.AddAll -> queue.addAll(op.tests).toString()
            Op.Poll -> queue.poll()?.let(::t) ?: "null"
            is Op.Remove -> queue.remove(op.test).toString()
            is Op.RemoveAll -> queue.removeAll(op.tests.toSet()).toString()
            is Op.SetExecutionTime -> { metrics.executionTimes[op.test] = op.value; "null" }
            is Op.SetSuccessRate -> { metrics.successRates[op.test] = op.value; "null" }
            Op.Reprocess -> { queue = PriorityQueue(recording(strategy.process(metrics))); "null" }
        }
        @Suppress("UNCHECKED_CAST")
        obj("result" to result, "array" to ts(queue.toArray().map { it as Test }), "trace" to drain())
    }
    rows += obj(
        "family" to q("sorting"), "case" to q(name), "strategy" to strategyJson, "metrics" to input,
        "draws" to arr(draws.map { it.toString() }), "ops" to arr(ops.map(::opJson)), "steps" to arr(steps),
    )
}

fun sortingProbes() {
    val et = SortingStrategyConfiguration.ExecutionTimeSortingStrategyConfiguration(90.0, LIMIT)
    val etJson = obj("type" to q("ExecutionTime"), "percentile" to d(90.0))
    val ts6 = tests(6)
    fun times(vararg v: Double) = ts6.zip(v.toList()).toMap().toMutableMap()
    sorting("execution time: offers, polls and a re-queue keep first keys", etJson, et,
        RecordingMetrics(executionTimes = times(1000.0, 3000.0, 2000.0, 3000.0, 500.0, 2500.0)), emptyList(),
        listOf(Op.Offer(ts6[0]), Op.Offer(ts6[1]), Op.Offer(ts6[2]), Op.SetExecutionTime(ts6[0], 9000.0),
            Op.AddAll(listOf(ts6[3], ts6[4], ts6[5])), Op.Poll, Op.Offer(ts6[0]), Op.Poll, Op.Poll,
            Op.RemoveAll(listOf(ts6[4], ts6[5])), Op.Poll, Op.Poll, Op.Poll))
    sorting("execution time: duplicates, ties, signed zero and NaN", etJson, et,
        RecordingMetrics(executionTimes = times(0.0, -0.0, Double.NaN, 1500.0, Double.POSITIVE_INFINITY, 1500.0)), emptyList(),
        listOf(Op.AddAll(ts6), Op.Offer(ts6[2]), Op.Offer(ts6[1]), Op.Remove(ts6[3]), Op.Poll, Op.Poll, Op.Poll,
            Op.Poll, Op.Poll, Op.Poll, Op.Poll))
    sorting("execution time: reprocess starts a fresh memo", etJson, et,
        RecordingMetrics(executionTimes = times(1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0)), emptyList(),
        listOf(Op.AddAll(ts6.take(3)), Op.SetExecutionTime(ts6[0], 7000.0), Op.Reprocess, Op.AddAll(ts6.take(3)),
            Op.Poll, Op.Poll, Op.Poll))
    for (ascending in listOf(false, true)) {
        val sr = SortingStrategyConfiguration.SuccessRateSortingStrategyConfiguration(LIMIT, ascending)
        val srJson = obj("type" to q("SuccessRate"), "ascending" to ascending.toString())
        sorting("success rate ascending=$ascending: not memoised", srJson, sr,
            RecordingMetrics(successRates = ts6.zip(listOf(0.1, 0.9, 0.5, 0.5, 0.0, 1.0)).toMap().toMutableMap()), emptyList(),
            listOf(Op.AddAll(ts6.take(4)), Op.SetSuccessRate(ts6[0], 0.95), Op.Offer(ts6[4]), Op.Offer(ts6[5]),
                Op.Poll, Op.Poll, Op.RemoveAll(listOf(ts6[2])), Op.Poll, Op.Poll, Op.Poll))
    }
    val ro = SortingStrategyConfiguration.RandomOrderStrategyConfiguration
    val roJson = obj("type" to q("RandomOrder"))
    sorting("random order: one draw per distinct test, cache shared across comparators", roJson, ro, RecordingMetrics(),
        listOf(5, -3, Int.MAX_VALUE, Int.MIN_VALUE, 5, 0, 17, -17),
        listOf(Op.Offer(ts6[0]), Op.Offer(ts6[1]), Op.AddAll(listOf(ts6[2], ts6[3], ts6[0])), Op.Poll, Op.Offer(ts6[4]),
            Op.Reprocess, Op.AddAll(listOf(ts6[5], ts6[1], ts6[4], ts6[0])), Op.Poll, Op.Poll, Op.Poll, Op.Poll))
    val ns = SortingStrategyConfiguration.NoSortingStrategyConfiguration
    sorting("no sorting: heap order is not FIFO", obj("type" to q("NoSorting")), ns, RecordingMetrics(), emptyList(),
        listOf(Op.AddAll(ts6), Op.Poll, Op.Offer(ts6[0]), Op.Poll, Op.RemoveAll(listOf(ts6[3])), Op.Poll, Op.Poll,
            Op.Poll, Op.Poll, Op.Poll))
}

// ---- (b) batching ------------------------------------------------------------------------------------------
class Bundle(override val id: String) : TestBundle()

fun batching(
    name: String, strategyJson: String, cnf: BatchingStrategyConfiguration, sortingName: String, metrics: RecordingMetrics,
    queued: List<Test>, bundles: Map<Test, String?>?, calls: Int,
) {
    val sortingCnf = when (sortingName) {
        "NoSorting" -> SortingStrategyConfiguration.NoSortingStrategyConfiguration
        else -> SortingStrategyConfiguration.ExecutionTimeSortingStrategyConfiguration(90.0, LIMIT)
    }
    val input = metrics.json()
    val analytics = Analytics(metrics)
    val queue = PriorityQueue(recording(sortingCnf.toSortingStrategy().process(analytics)))
    val identityBundles = bundles?.values?.filterNotNull()?.distinct()?.associateWith { Bundle(it) }
    val identifier = bundles?.let { map ->
        object : TestBundleIdentifier {
            override fun identify(test: Test): TestBundle? {
                event(q("identify"), t(test))
                return map[test]?.let { identityBundles!![it] }
            }
        }
    }
    queue.addAll(queued)
    val setup = drain()
    val strategy = cnf.toBatchingStrategy()
    // QueueActor never batches an empty queue (lifecycle.md), so neither does the probe.
    val results = (0 until calls).asSequence().takeWhile { queue.isNotEmpty() }.map {
        val batch = strategy.process(queue, analytics, identifier)
        @Suppress("UNCHECKED_CAST")
        obj("tests" to ts(batch.tests), "array" to ts(queue.toArray().map { it as Test }), "trace" to drain())
    }.toList()
    rows += obj(
        "family" to q("batching"), "case" to q(name), "strategy" to strategyJson, "sorting" to q(sortingName),
        "metrics" to input, "queue" to ts(queued),
        "bundles" to (bundles?.let { m -> arr(m.map { "[${t(it.key)},${it.value?.let(::q) ?: "null"}]" }) } ?: "null"),
        "setup" to setup, "batches" to arr(results),
    )
}

fun fixed(size: Int, duration: Long? = null, percentile: Double? = null, limit: Boolean = false, lastMile: Int = 0) =
    BatchingStrategyConfiguration.FixedSizeBatchingStrategyConfiguration(size, duration, percentile, if (limit) LIMIT else null, lastMile) to
        obj("type" to q("FixedSize"), "size" to size.toString(), "durationMillis" to (duration?.toString() ?: "null"),
            "percentile" to (percentile?.let(::d) ?: "null"), "timeLimit" to limit.toString(), "lastMileLength" to lastMile.toString())

fun batchingProbes() {
    val t8 = tests(8)
    for (sortingName in listOf("NoSorting", "ExecutionTime")) {
        val times = t8.zip(listOf(1000.0, 4000.0, 2000.0, 8000.0, 500.0, 3000.0, 6000.0, 7000.0)).toMap().toMutableMap()
        val (f3, f3j) = fixed(3)
        batching("fixed size 3 over 8 [$sortingName]", f3j, f3, sortingName, RecordingMetrics(executionTimes = times), t8, null, 3)
        // Flaky copies: the duplicate is unbatchable but consumes a slot and is re-added at the end.
        val flaky = listOf(t8[0], t8[1], t8[0], t8[2], t8[1], t8[0], t8[3])
        val (f4, f4j) = fixed(4)
        batching("fixed size 4 with flaky copies [$sortingName]", f4j, f4, sortingName, RecordingMetrics(executionTimes = times), flaky, null, 4)
        // Mixed and null bundles: only a non-null bundle locks the batch.
        val bundles = mapOf(t8[0] to null, t8[1] to "a", t8[2] to null, t8[3] to "b", t8[4] to "a", t8[5] to null, t8[6] to "b", t8[7] to "a")
        val (f5, f5j) = fixed(5)
        batching("fixed size 5 with mixed and null bundles [$sortingName]", f5j, f5, sortingName, RecordingMetrics(executionTimes = times), t8, bundles, 4)
        val nullFirst = mapOf(t8[0] to null, t8[1] to null, t8[2] to "a", t8[3] to null, t8[4] to "b")
        batching("fixed size 5 with leading null bundles [$sortingName]", f5j, f5, sortingName, RecordingMetrics(executionTimes = times), t8.take(5), nullFirst, 2)
        // Last mile: below the threshold every batch is one polled test.
        val (fl, flj) = fixed(4, lastMile = 5)
        batching("fixed size 4 with last mile 5 [$sortingName]", flj, fl, sortingName, RecordingMetrics(executionTimes = times), t8, null, 5)
        // Duration break: every polled item adds its duration, unbatchable ones too.
        val (fd, fdj) = fixed(5, duration = 7000, percentile = 90.0, limit = true)
        batching("fixed size 5 with duration 7000 [$sortingName]", fdj, fd, sortingName, RecordingMetrics(executionTimes = times), t8, null, 4)
        batching("fixed size 5 with duration 7000 and flaky copies [$sortingName]", fdj, fd, sortingName, RecordingMetrics(executionTimes = times), flaky, null, 3)
        val (fe, fej) = fixed(5, duration = 7000, percentile = 90.0, limit = true)
        batching("fixed size 5 with duration and bundles [$sortingName]", fej, fe, sortingName, RecordingMetrics(executionTimes = times), t8, bundles, 3)
        // F2 nulls timeLimit: then no duration check and no executionTime lookups by the batcher.
        val (fn, fnj) = fixed(5, duration = 7000, percentile = 90.0, limit = false)
        batching("fixed size 5 with duration but no time limit [$sortingName]", fnj, fn, sortingName, RecordingMetrics(executionTimes = times), t8, null, 2)
        val (fz, fzj) = fixed(0)
        batching("fixed size 0 [$sortingName]", fzj, fz, sortingName, RecordingMetrics(executionTimes = times), t8.take(3), null, 1)
        val classes = listOf(classTest(1, 0), classTest(0, 0), classTest(1, 1), classTest(2, 0), classTest(0, 1), classTest(1, 2),
            classTest(0, 0), Test("", "clazz1", "method3", emptyList()), Test(" ", "clazz1", "method4", emptyList()))
        val cn = BatchingStrategyConfiguration.ClassNameBatchingStrategyConfiguration
        batching("class name [$sortingName]", obj("type" to q("ClassName")), cn, sortingName,
            RecordingMetrics(executionTimes = classes.withIndex().associate { it.value to 1000.0 * (it.index % 4) }.toMutableMap()), classes, null, 5)
        val iso = BatchingStrategyConfiguration.IsolateBatchingStrategyConfiguration
        batching("isolate [$sortingName]", obj("type" to q("Isolate")), iso, sortingName, RecordingMetrics(executionTimes = times), flaky, null, 3)
    }
}

// ---- (c) flakiness -----------------------------------------------------------------------------------------
fun flakinessProbes() {
    val rates = listOf(0.0, 0.001, 0.01, 0.1, 0.19, 0.2, 0.3, 0.5, 0.6, 0.7, 0.75, 0.79, 0.8, 0.81, 0.9, 0.99, 1.0)
    for (min in listOf(0.0, 0.5, 0.8, 0.9, 0.99, 0.999, 1.0)) for (max in listOf(0, 1, 3, 5, 100)) {
        val shard = TestShard(tests(rates.size))
        val metrics = RecordingMetrics(successRates = shard.tests.zip(rates).toMap().toMutableMap())
        val input = metrics.json()
        val strategy = FlakinessStrategyConfiguration.ProbabilityBasedFlakinessStrategyConfiguration(min, max, LIMIT).toFlakinessStrategy()
        val result = strategy.process(shard, metrics)
        rows += obj(
            "family" to q("flakiness"), "minSuccessRate" to d(min), "maxCount" to max.toString(), "metrics" to input,
            "tests" to ts(shard.tests), "result" to ts(result.tests), "flaky" to ts(result.flakyTests), "trace" to drain(),
        )
    }
}

// ---- (d) retry ---------------------------------------------------------------------------------------------
val config = Configuration.Builder(name = "", outputDir = File("")).apply {
    vendorConfiguration = VendorConfiguration.StubVendorConfiguration
    debug = false
    analyticsTracking = false
    executionStrategy = ExecutionStrategyConfiguration(ExecutionMode.ANY_SUCCESS, fast = false)
}.build()
val device = DeviceInfo(OperatingSystem("Fake OS"), "fake serial", "fake model", "fake manufacturer", NetworkState.CONNECTED, emptyList(), true)
fun result(test: Test, status: TestStatus = TestStatus.FAILURE) = TestResult(test, device, "stub-batch", status, 0, 10000)

// PoolProgressAccumulator.transition looks the test up in `tests` once per call; recording that lookup records
// every retryTest call with the state it starts from.
class RecordingTests : HashMap<String, Any>() {
    var recording = false
    override fun get(key: String): Any? {
        val value = super.get(key)
        if (recording) event(q("retryTest"), q(key), q(value?.let(::stateOf)?.javaClass?.simpleName ?: "none"))
        return value
    }
}
fun field(o: Any, name: String): Any {
    val f = o.javaClass.getDeclaredField(name)
    f.isAccessible = true
    return f.get(o)
}
fun stateOf(data: Any): Any = field(data, "state").let { it.javaClass.getMethod("getState").invoke(it) }
fun total(state: Any) = state.javaClass.getMethod("getTotal").invoke(state) as Int

fun retry(name: String, total: Int, perTest: Int, shardTests: List<Test>, flaky: List<Test>, pre: List<Pair<Test, TestStatus>>, rounds: List<List<Test>>) {
    val pool = DevicePoolId("pool")
    val shard = TestShard(shardTests, flaky)
    val accumulator = PoolProgressAccumulator(pool, shard, config, Track())
    val recordingTests = RecordingTests()
    @Suppress("UNCHECKED_CAST")
    val original = field(accumulator, "tests") as HashMap<String, Any>
    recordingTests.putAll(original)
    accumulator.javaClass.getDeclaredField("tests").apply { isAccessible = true }.set(accumulator, recordingTests)
    pre.forEach { (test, status) ->
        accumulator.testStarted(device, test)
        accumulator.testEnded(device, result(test, status))
    }
    val strategy = RetryStrategyConfiguration.FixedQuotaRetryStrategyConfiguration(total, perTest).toRetryStrategy() as FixedQuotaRetryStrategy
    val watchdog = field(strategy, "retryWatchdog")
    val failures = field(strategy, "poolTestCaseFailureAccumulator")
    val getCount = failures.javaClass.getMethod("getCount", DevicePoolId::class.java, Test::class.java)
    val distinct = (shardTests + flaky + rounds.flatten()).distinct()
    val steps = rounds.map { failed ->
        recordingTests.recording = true
        val returned = strategy.process(pool, failed.map { result(it) }, shard, accumulator)
        recordingTests.recording = false
        obj(
            "failed" to ts(failed), "returned" to ts(returned.map { it.test }),
            "left" to (field(watchdog, "totalAllowedRetryLeft") as java.util.concurrent.atomic.AtomicInteger).get().toString(),
            "counts" to arr(distinct.map { "[${t(it)},${getCount.invoke(failures, pool, it)}]" }),
            "totals" to arr(recordingTests.entries.sortedBy { it.key }.map { (k, v) -> val s = stateOf(v); "[${q(k)},${q(s.javaClass.simpleName)},${total(s)}]" }),
            "trace" to drain(),
        )
    }
    rows += obj(
        "family" to q("retry"), "case" to q(name), "total" to total.toString(), "perTest" to perTest.toString(),
        "shard" to ts(shardTests), "flaky" to ts(flaky),
        "pre" to arr(pre.map { "[${t(it.first)},${q(it.second.name)}]" }), "rounds" to arr(steps),
    )
}

fun retryProbes() {
    val t4 = tests(4)
    // C5: at left == 0 AddRetry still runs, then the decrement refuses.
    retry("total quota 1, two failures", 1, 3, t4, emptyList(), emptyList(), listOf(listOf(t4[0], t4[1])))
    retry("total quota 2 across rounds", 2, 3, t4, emptyList(), emptyList(), listOf(listOf(t4[0]), listOf(t4[1], t4[2]), listOf(t4[3], t4[0])))
    retry("per-test quota 2 with repeated failures", 100, 2, t4, emptyList(), emptyList(), listOf(listOf(t4[0], t4[0]), listOf(t4[0]), listOf(t4[0], t4[1])))
    retry("flaky copies count as failures", 100, 3, t4, listOf(t4[0], t4[0], t4[1]), emptyList(), listOf(listOf(t4[0], t4[1], t4[2]), listOf(t4[1])))
    retry("terminal tests return Complete", 100, 3, t4, emptyList(), listOf(t4[0] to TestStatus.PASSED, t4[1] to TestStatus.FAILURE), listOf(listOf(t4[0], t4[1], t4[2])))
    retry("unknown test has no FSM", 100, 3, t4.take(2), emptyList(), emptyList(), listOf(listOf(t4[3], t4[0])))
    retry("total quota 0", 0, 3, t4, emptyList(), emptyList(), listOf(listOf(t4[0], t4[1])))
    retry("negative quotas", -1, -1, t4, emptyList(), emptyList(), listOf(listOf(t4[0])))
    // QueueActor builds one strategy per pool, so each pool's queue gets the full total quota (C30).
    retry("pool A of two, total quota 1", 1, 3, t4, emptyList(), emptyList(), listOf(listOf(t4[0], t4[1])))
    retry("pool B of two, total quota 1", 1, 3, t4, emptyList(), emptyList(), listOf(listOf(t4[2], t4[3])))
}

fun main(args: Array<String>) {
    val out = File(args[0]).apply { mkdirs() }
    for ((family, probe) in listOf("sorting" to ::sortingProbes, "batching" to ::batchingProbes,
        "flakiness" to ::flakinessProbes, "retry" to ::retryProbes)) {
        rows.clear()
        probe()
        File(out, "$family.jsonl").writeText(rows.joinToString("\n", postfix = "\n"))
        System.err.println("${rows.size} rows -> $out/$family.jsonl")
    }
}
