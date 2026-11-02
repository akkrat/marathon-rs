package oracle

import com.fasterxml.jackson.databind.JsonNode
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.device.Device
import com.malinskiy.marathon.device.DeviceFeature
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.NetworkState
import com.malinskiy.marathon.device.OperatingSystem
import com.malinskiy.marathon.device.toDeviceInfo
import com.malinskiy.marathon.exceptions.DeviceLostException
import com.malinskiy.marathon.exceptions.TestBatchExecutionException
import com.malinskiy.marathon.execution.TestBatchResults
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.TestStatus
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay

/**
 * The device of a replay scenario. Its properties are those of vendor-test's StubDevice, and so are its two behaviours:
 * - `stub`: StubDevice's prepare and execute, statement by statement (prepare and test delays in scheduler time, the crash
 *   flags, the per-test result sequences and the 0, 1, 2… time counter), so that the Kotlin scenario tests replay unchanged;
 * - `scripted`: prepare and execute suspend until the event script delivers `prepare-done`/`prepare-failed` and
 *   `batch-result`/`batch-failed` for this device (PLAN.md §6.5).
 *
 * It is a copy rather than StubDevice itself so that every call can be logged in the transcript: StubDevice is final, and a
 * wrapper would hand marathon a different Device instance than the one in the results.
 */
class ReplayDevice(spec: ScenarioSpec.DeviceSpec, private val tests: Map<String, Test>, private val replay: Replay) : Device {
    private val node = spec.node
    private val behaviour = spec.behaviour

    init {
        node.only("devices[]", "serial", "behaviour", "model", "manufacturer", "os", "abi", "healthy", "networkState",
            "prepareTimeMillis", "testTimeMillis", "crashWithTestBatchException", "crashWithDeviceLostException",
            "reportResultsBeforeCrash", "results")
        require(behaviour == "stub" || behaviour == "scripted") { "device ${spec.serial}: behaviour $behaviour is not stub or scripted" }
    }

    override val serialNumber: String = spec.serial
    override val operatingSystem = OperatingSystem(node.path("os").asText("25"))
    override val model: String = node.path("model").asText("test")
    override val manufacturer: String = node.path("manufacturer").asText("test")
    override val networkState = NetworkState.valueOf(node.path("networkState").asText("CONNECTED"))
    override val deviceFeatures: Collection<DeviceFeature> = listOf()
    override val abi: String = node.path("abi").asText("test")
    override val healthy: Boolean = node.path("healthy").asBoolean(true)
    override val logger = MarathonLogging.logger("StubDevice")

    private val prepareTimeMillis = node.path("prepareTimeMillis").asLong(5000L)
    private val testTimeMillis = node.path("testTimeMillis").asLong(5000L)
    private val crashWithTestBatchException = node.path("crashWithTestBatchException").asBoolean(false)
    private val crashWithDeviceLostException = node.path("crashWithDeviceLostException").asBoolean(false)
    private val reportResultsBeforeCrash = node.path("reportResultsBeforeCrash").asBoolean(false)

    /** StubDevice.executionResults, a lateinit there: a stub device without results fails when it executes */
    private val executionResults: Map<Test, Array<TestStatus>>? = node.get("results")?.let { results ->
        results.fields().asSequence().associate { (id, statuses) ->
            val test = tests[id] ?: throw IllegalArgumentException("device $serialNumber: unknown test $id")
            test to statuses.map { TestStatus.valueOf(it.asText()) }.toTypedArray()
        }
    }
    private val executionIndexMap: MutableMap<Test, Int> = mutableMapOf()
    private var timeCounter: Long = 0

    private sealed class Step {
        class Prepare(val done: CompletableDeferred<JsonNode>) : Step()
        class Execute(val batch: TestBatch, val done: CompletableDeferred<JsonNode>) : Step()
    }

    private var pending: Step? = null

    override suspend fun prepare(configuration: Configuration) {
        logger.debug { "Preparing" }
        call("prepare")
        try {
            if (behaviour == "stub") {
                delay(prepareTimeMillis)
            } else {
                val done = CompletableDeferred<JsonNode>()
                pending = Step.Prepare(done)
                val event = done.await()
                if (event.text("event") == "prepare-failed") {
                    call("prepare-failed")
                    throw RuntimeException(event.path("message").asText("prepare failed"))
                }
            }
            call("prepared")
        } catch (e: CancellationException) {
            call("prepare-cancelled")
            throw e
        } finally {
            pending = null
        }
    }

    override suspend fun execute(
        configuration: Configuration,
        devicePoolId: DevicePoolId,
        testBatch: TestBatch,
        deferred: CompletableDeferred<TestBatchResults>
    ) {
        call("execute", "pool" to devicePoolId.name, "batch" to testBatch.id, "tests" to testBatch.tests.map { it.id() })
        try {
            if (behaviour == "stub") executeStub(testBatch, deferred) else executeScripted(testBatch, deferred)
            call("executed", "batch" to testBatch.id)
        } catch (e: CancellationException) {
            call("execute-cancelled", "batch" to testBatch.id)
            throw e
        } catch (e: Throwable) {
            call("execute-threw", "batch" to testBatch.id, "exception" to e.javaClass.simpleName)
            throw e
        } finally {
            pending = null
        }
    }

    /** StubDevice.execute */
    private suspend fun executeStub(testBatch: TestBatch, deferred: CompletableDeferred<TestBatchResults>) {
        delay(testTimeMillis)

        if (reportResultsBeforeCrash) {
            deferred.complete(TestBatchResults(this, emptyList(), emptyList(), emptyList()))
        }

        if (crashWithTestBatchException) {
            throw TestBatchExecutionException("user requested the device to crash via crashWithTestBatchException parameter")
        }

        if (crashWithDeviceLostException) {
            throw DeviceLostException(RuntimeException("user requested the device to crash via crashWithDeviceLostException parameter"))
        }

        val executionResults = executionResults
            ?: throw UninitializedPropertyAccessException("lateinit property executionResults has not been initialized")
        val results = testBatch.tests.map {
            val i = executionIndexMap.getOrDefault(it, 0)
            val result = executionResults[it]!![i]
            executionIndexMap[it] = i + 1
            val testResult = TestResult(it, toDeviceInfo(), testBatch.id, result, timeCounter, timeCounter + 1, null)
            timeCounter += 1
            testResult
        }
        complete(deferred, results)
    }

    private suspend fun executeScripted(testBatch: TestBatch, deferred: CompletableDeferred<TestBatchResults>) {
        val done = CompletableDeferred<JsonNode>()
        pending = Step.Execute(testBatch, done)
        val event = done.await()
        if (event.text("event") == "batch-failed") {
            if (event.path("reportEmptyResultsFirst").asBoolean(false)) {
                deferred.complete(TestBatchResults(this, emptyList(), emptyList(), emptyList()))
            }
            val message = event.path("message").asText("scripted failure")
            throw when (val kind = event.text("exception")) {
                "TestBatchExecutionException" -> TestBatchExecutionException(message)
                "DeviceLostException" -> DeviceLostException(RuntimeException(message))
                "RuntimeException" -> RuntimeException(message)
                else -> IllegalArgumentException("batch-failed: exception $kind is not supported")
            }
        }
        val results = event.path("results").map { r ->
            val test = tests[r.text("test")] ?: throw IllegalArgumentException("batch-result: unknown test ${r.text("test")}")
            val start = r.path("start").asLong(timeCounter)
            val end = r.path("end").asLong(start + 1)
            timeCounter += 1
            TestResult(test, toDeviceInfo(), testBatch.id, TestStatus.valueOf(r.text("status")), start, end,
                r.get("stacktrace")?.asText())
        }
        complete(deferred, results)
    }

    private fun complete(deferred: CompletableDeferred<TestBatchResults>, results: List<TestResult>) {
        deferred.complete(
            TestBatchResults(this,
                results.filter { it.status == TestStatus.PASSED },
                results.filter { it.status == TestStatus.FAILURE },
                results.filter { it.status == TestStatus.INCOMPLETE }
            )
        )
    }

    override fun dispose() {
        logger.debug { "Disposing" }
        call("dispose")
    }

    /** Delivers a device event of the script; returns an error when the device is not in a state that takes it */
    fun deliver(type: String, event: JsonNode): String? {
        val step = pending
        when (type) {
            "prepare-done", "prepare-failed" -> {
                if (step !is Step.Prepare) return "$type for $serialNumber, which is not preparing"
                step.done.complete(event)
            }
            "batch-result", "batch-failed" -> {
                if (step !is Step.Execute) return "$type for $serialNumber, which is not executing a batch"
                if (type == "batch-result") {
                    val reported = event.path("results").map { it.text("test") }.sorted()
                    val running = step.batch.tests.map { it.id() }.sorted()
                    if (reported != running) return "batch-result for $serialNumber reports $reported, but the batch is $running"
                }
                step.done.complete(event)
            }
            else -> return "$type is not a device event"
        }
        return null
    }

    private fun call(name: String, vararg fields: Pair<String, Any?>) {
        replay.record(linkedMapOf<String, Any?>("device" to serialNumber, "call" to name) + fields)
    }

    private fun Test.id() = "$pkg.$clazz#$method"
}
