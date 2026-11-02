package oracle

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.LoggerContext
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.AppenderBase
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.Device
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.toDeviceInfo
import com.malinskiy.marathon.exceptions.DeviceLostException
import com.malinskiy.marathon.exceptions.TestBatchExecutionException
import com.malinskiy.marathon.execution.DevicePoolMessage
import com.malinskiy.marathon.execution.TestBatchResults
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.TestStatus
import com.malinskiy.marathon.execution.device.DeviceActor
import com.malinskiy.marathon.execution.device.DeviceEvent
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.test.StubDevice
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ChannelResult
import kotlinx.coroutines.channels.SendChannel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.SelectClause2
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestCoroutineScheduler
import org.slf4j.LoggerFactory
import java.io.File

/**
 * WP 3.6: the real DeviceActor of `:core` on a StandardTestDispatcher (single thread, FIFO, virtual time), driven
 * step by step. Each row is one scenario: the ordered trace of DeviceActor log events and pool messages, the vendor call
 * counts, the actor job's final state and scenario-specific extras. The scheduler drains with `runCurrent` and advances
 * time only where a scenario says so, the order the Rust replay driver reproduces.
 *
 * The batch id (a random UUID) is written as `<batch>`; stack frame lines (`\tat …`, `\t... n more`) are dropped from
 * strings, since JVM frames are an accepted difference (PLAN §3.10).
 */
@OptIn(ExperimentalCoroutinesApi::class)
object DeviceActorProbe {
    private val configuration = Configuration.Builder(name = "", outputDir = File("")).apply {
        vendorConfiguration = VendorConfiguration.StubVendorConfiguration
        analyticsTracking = false
    }.build()

    class NotBootedProbeException(message: String) : CancellationException(message)

    private class Trace {
        val items = mutableListOf<Map<String, Any?>>()
    }

    /** One scenario: scheduler, parent job, recording pool and the log appender share a trace */
    private class Run(val name: String) {
        val scheduler = TestCoroutineScheduler()
        val dispatcher = StandardTestDispatcher(scheduler)
        val parent = Job()
        val trace = Trace()
        val channel = Channel<DevicePoolMessage>(Channel.UNLIMITED)
        val test = Test("pkg", "clazz", "method", emptyList())
        val batch = TestBatch(listOf(test))
        var prepares = 0
        var executes = 0
        val extra = linkedMapOf<String, Any?>()
        lateinit var actor: DeviceActor

        val pool = object : SendChannel<DevicePoolMessage> by channel {
            override fun trySend(element: DevicePoolMessage): ChannelResult<Unit> {
                val result = channel.trySend(element)
                record(element, if (result.isSuccess) "trySend" else "trySend-failed")
                return result
            }

            override suspend fun send(element: DevicePoolMessage) {
                try {
                    channel.send(element)
                    record(element, "send")
                } catch (e: Throwable) {
                    record(element, "send-failed:" + e.javaClass.name)
                    throw e
                }
            }

            @Suppress("OVERRIDE_DEPRECATION")
            override val onSend: SelectClause2<DevicePoolMessage, SendChannel<DevicePoolMessage>> get() = channel.onSend
        }

        fun record(message: DevicePoolMessage, via: String) {
            val body = linkedMapOf<String, Any?>("type" to message.javaClass.simpleName, "via" to via)
            when (message) {
                is DevicePoolMessage.FromDevice.CompletedTestBatch -> {
                    body["finished"] = message.results.finished.map { it.test.toString() }
                    body["failed"] = message.results.failed.map { it.test.toString() }
                    body["uncompleted"] = message.results.uncompleted.map { it.test.toString() }
                    body["sameDevice"] = message.results.device === message.device
                }
                is DevicePoolMessage.FromDevice.ReturnTestBatch -> {
                    body["sameBatch"] = message.batch === batch
                    body["reason"] = clean(message.reason)
                }
                else -> {}
            }
            synchronized(trace) { trace.items.add(linkedMapOf("t" to scheduler.currentTime, "pool" to body)) }
        }

        fun clean(text: String?): String? = text?.replace(batch.id, "<batch>")?.split("\n")
            ?.filterNot { it.startsWith("\tat ") || it.startsWith("\t... ") }?.joinToString("\n")

        fun start(device: Device, poolChannel: SendChannel<DevicePoolMessage> = pool): DeviceActor {
            actor = DeviceActor(DevicePoolId("test"), poolChannel, configuration, device, parent, dispatcher)
            return actor
        }

        fun send(event: DeviceEvent) {
            check(actor.trySend(event).isSuccess) { "$name: actor rejected $event" }
        }

        fun runCurrent() = scheduler.runCurrent()

        fun passed(device: Device) = TestBatchResults(
            device,
            listOf(TestResult(test, device.toDeviceInfo(), batch.id, TestStatus.PASSED, 0, 1, null)),
            emptyList(),
            emptyList(),
        )

        /** StubDevice(0, 0) whose execute runs the script, like DeviceActorBatchAccountingTest.scriptedDevice */
        fun scripted(script: suspend Device.(CompletableDeferred<TestBatchResults>) -> Unit): Device {
            val underlying = StubDevice(prepareTimeMillis = 0, testTimeMillis = 0)
            return object : Device by underlying {
                override suspend fun prepare(configuration: Configuration) {
                    prepares++
                    underlying.prepare(configuration)
                }

                override suspend fun execute(
                    configuration: Configuration,
                    devicePoolId: DevicePoolId,
                    testBatch: TestBatch,
                    deferred: CompletableDeferred<TestBatchResults>,
                ) {
                    executes++
                    script(this, deferred)
                }
            }
        }

        /** StubDevice with counted calls; execute and prepare delegate unchanged */
        fun stub(device: StubDevice, prepare: (suspend () -> Unit)? = null): Device = object : Device by device {
            override suspend fun prepare(configuration: Configuration) {
                prepares++
                if (prepare != null) prepare() else device.prepare(configuration)
            }

            override suspend fun execute(
                configuration: Configuration,
                devicePoolId: DevicePoolId,
                testBatch: TestBatch,
                deferred: CompletableDeferred<TestBatchResults>,
            ) {
                executes++
                device.execute(configuration, devicePoolId, testBatch, deferred)
            }
        }

        /** Initialize, drain to IsReady, then Execute and drain (DeviceActorBatchAccountingTest.startBatch) */
        fun startBatch(device: Device) {
            start(device)
            send(DeviceEvent.Initialize)
            runCurrent()
            send(DeviceEvent.Execute(batch))
            runCurrent()
        }

        fun row(): Map<String, Any?> {
            val job = actor.coroutineContext.job
            return linkedMapOf(
                "scenario" to name,
                "trace" to synchronized(trace) { trace.items.toList() },
                "calls" to linkedMapOf("prepare" to prepares, "execute" to executes),
                "job" to linkedMapOf("active" to job.isActive, "completed" to job.isCompleted, "cancelled" to job.isCancelled),
                "parent" to linkedMapOf("active" to parent.isActive, "cancelled" to parent.isCancelled),
                "t" to scheduler.currentTime,
                "extra" to extra,
            )
        }
    }

    private class Recorder : AppenderBase<ILoggingEvent>() {
        var run: Run? = null
        override fun append(event: ILoggingEvent) {
            val current = run ?: return
            if (event.loggerName == "StubDevice") return
            val throwable = mutableListOf<Map<String, Any?>>()
            var proxy = event.throwableProxy
            while (proxy != null) {
                throwable.add(linkedMapOf("class" to proxy.className, "message" to current.clean(proxy.message)))
                proxy = proxy.cause
            }
            val body = linkedMapOf<String, Any?>(
                "logger" to event.loggerName,
                "level" to event.level.toString(),
                "message" to current.clean(event.formattedMessage),
                "throwable" to throwable,
            )
            synchronized(current.trace) { current.trace.items.add(linkedMapOf("t" to current.scheduler.currentTime, "log" to body)) }
        }
    }

    private fun Run.initializeJob(): Job {
        val field = DeviceActor::class.java.getDeclaredField("initializeJob").apply { isAccessible = true }
        return field.get(actor) as Job
    }

    private val scenarios: List<Pair<String, Run.() -> Unit>> = listOf(
        // DeviceActorBatchAccountingTest
        "lost device preserves already reported passes" to {
            startBatch(scripted { deferred ->
                deferred.complete(passed(this))
                throw DeviceLostException(IllegalStateException("transport disconnected after test passed"))
            })
        },
        "lost device returns the batch when the reported results failed" to {
            startBatch(scripted { deferred ->
                deferred.completeExceptionally(IllegalStateException("result bundle is corrupted"))
                throw DeviceLostException(IllegalStateException("transport disconnected"))
            })
        },
        "lost device returns the batch when the reported results were cancelled" to {
            startBatch(scripted { deferred ->
                deferred.cancel()
                throw DeviceLostException(IllegalStateException("transport disconnected"))
            })
        },
        "cancelled execution sends already reported results" to {
            startBatch(scripted { deferred ->
                deferred.complete(passed(this))
                throw CancellationException("execution timed out while collecting artifacts")
            })
        },
        "cancelled execution without results returns the batch" to {
            startBatch(scripted { throw CancellationException("execution timed out") })
        },
        "termination after reported results sends them and cancels the execution" to {
            val cancelled = CompletableDeferred<Unit>()
            startBatch(scripted { deferred ->
                deferred.complete(passed(this))
                try {
                    awaitCancellation()
                } finally {
                    cancelled.complete(Unit)
                }
            })
            send(DeviceEvent.Terminate)
            runCurrent()
            extra["executionCancelled"] = cancelled.isCompleted
        },
        "termination before results returns the batch once and ignores a late report" to {
            val late = CompletableDeferred<Boolean>()
            startBatch(scripted { deferred ->
                try {
                    awaitCancellation()
                } finally {
                    late.complete(deferred.complete(passed(this)))
                }
            })
            send(DeviceEvent.Terminate)
            runCurrent()
            extra["lateReportAccepted"] = if (late.isCompleted) late.getCompleted() else null
        },
        "batch failure during termination does not return the batch twice" to {
            startBatch(scripted {
                try {
                    awaitCancellation()
                } catch (e: CancellationException) {
                    throw TestBatchExecutionException("test run failed while the device was terminated")
                }
            })
            send(DeviceEvent.Terminate)
            runCurrent()
        },
        "results of a completed batch reach the pool even if the actor stops right after" to {
            startBatch(scripted { deferred ->
                deferred.complete(passed(this))
                CoroutineScope(dispatcher).launch { parent.cancel() }
            })
            runCurrent()
        },
        "actor failing before a dispatched batch starts executing returns the batch" to {
            val fail = CompletableDeferred<Unit>()
            val underlying = StubDevice(prepareTimeMillis = 0, testTimeMillis = 0)
            start(object : Device by underlying {
                override suspend fun prepare(configuration: Configuration) {
                    prepares++
                    CoroutineScope(currentCoroutineContext()).launch {
                        fail.await()
                        throw IllegalStateException("log streaming crashed")
                    }
                }

                override suspend fun execute(
                    configuration: Configuration,
                    devicePoolId: DevicePoolId,
                    testBatch: TestBatch,
                    deferred: CompletableDeferred<TestBatchResults>,
                ) {
                    executes++
                }
            })
            send(DeviceEvent.Initialize)
            runCurrent()
            send(DeviceEvent.Execute(batch))
            fail.complete(Unit)
            runCurrent()
        },
        "completed execution with cancelled results returns the batch and stays ready" to {
            startBatch(scripted { deferred -> deferred.cancel() })
        },
        // DeviceActorTest
        "terminate while in progress" to {
            val device = StubDevice(prepareTimeMillis = 1000L, testTimeMillis = 10000L)
            device.executionResults = mapOf(test to arrayOf(TestStatus.FAILURE))
            start(stub(device))
            send(DeviceEvent.Initialize)
            runCurrent()
            scheduler.advanceTimeBy(1000)
            runCurrent()
            send(DeviceEvent.Execute(batch))
            send(DeviceEvent.Terminate)
            runCurrent()
            scheduler.advanceUntilIdle()
        },
        "device lost while in progress returns the batch and notifies the pool" to {
            startBatch(stub(StubDevice(prepareTimeMillis = 0L, testTimeMillis = 0L, crashWithDeviceLostException = true)))
        },
        "batch failure after reported results sends the results only" to {
            startBatch(stub(StubDevice(0L, 0L, crashWithTestBatchException = true, reportResultsBeforeCrash = true)))
        },
        "batch failure without results returns the batch only" to {
            startBatch(stub(StubDevice(prepareTimeMillis = 0L, testTimeMillis = 0L, crashWithTestBatchException = true)))
        },
        "a preparation that fails with a cancellation of its own terminates the device at once, without retries" to {
            start(stub(StubDevice(prepareTimeMillis = 0L)) { throw NotBootedProbeException("not booted") })
            send(DeviceEvent.Initialize)
            runCurrent()
        },
        // Additional scenarios (docs/wp/3.6.md, "Fixtures and oracle evidence")
        "terminate during preparation" to {
            start(stub(StubDevice(prepareTimeMillis = 5000L)))
            send(DeviceEvent.Initialize)
            runCurrent()
            send(DeviceEvent.Terminate)
            runCurrent()
            scheduler.advanceUntilIdle()
        },
        "thirty failing preparations" to {
            start(stub(StubDevice()) { throw IllegalStateException("prepare failed") })
            send(DeviceEvent.Initialize)
            runCurrent()
            scheduler.advanceUntilIdle()
        },
        "retry delay cancelled at its deadline" to {
            start(stub(StubDevice()) { throw IllegalStateException("prepare failed") })
            // Registered before the retry delay, so at 10 s it runs first: the delay throws, the isActive skip is not reached
            CoroutineScope(dispatcher).launch {
                delay(10_000)
                initializeJob().cancel()
            }
            send(DeviceEvent.Initialize)
            runCurrent()
            scheduler.advanceUntilIdle()
        },
        "execute at an unclosed terminated actor" to {
            start(scripted { })
            send(DeviceEvent.Initialize)
            runCurrent()
            send(DeviceEvent.Terminate)
            send(DeviceEvent.Execute(batch))
            runCurrent()
        },
        "completed execution with exceptional results without a message" to {
            startBatch(scripted { deferred -> deferred.completeExceptionally(IllegalStateException()) })
        },
        "lost device with exceptional results without a message" to {
            startBatch(scripted { deferred ->
                deferred.completeExceptionally(IllegalStateException())
                throw DeviceLostException(IllegalStateException("transport disconnected"))
            })
        },
        "parent cancellation while running" to {
            startBatch(scripted { awaitCancellation() })
            parent.cancel()
            runCurrent()
        },
        "readiness to a cancelled pool" to {
            channel.cancel()
            start(scripted { })
            send(DeviceEvent.Initialize)
            runCurrent()
        },
        "readiness to a closed pool" to {
            channel.close()
            start(scripted { })
            send(DeviceEvent.Initialize)
            runCurrent()
        },
        "actor job failure from a child launched in prepare" to {
            val fail = CompletableDeferred<Unit>()
            val underlying = StubDevice(prepareTimeMillis = 0, testTimeMillis = 0)
            start(object : Device by underlying {
                override suspend fun prepare(configuration: Configuration) {
                    prepares++
                    CoroutineScope(currentCoroutineContext()).launch {
                        fail.await()
                        throw IllegalStateException("log streaming crashed")
                    }
                }
            })
            send(DeviceEvent.Initialize)
            runCurrent()
            fail.complete(Unit)
            runCurrent()
        },
        "actor job failure from a child launched in the actor scope" to {
            start(scripted { })
            send(DeviceEvent.Initialize)
            runCurrent()
            actor.launch { throw IllegalStateException("actor child crashed") }
            runCurrent()
        },
        // Review round 1: the execute coroutine is dispatched before the terminate coroutine
        // that cancels it, so a vendor checking cancellation before its first suspension
        // still sees an active job and finishes its command.
        "entry cancellation" to {
            start(scripted {
                extra["entryActive"] = currentCoroutineContext().isActive
                currentCoroutineContext().ensureActive()
                extra["commandSent"] = true
            })
            send(DeviceEvent.Initialize)
            runCurrent()
            send(DeviceEvent.Execute(batch))
            send(DeviceEvent.Terminate)
            runCurrent()
        },
    )

    fun generate(out: File) {
        MarathonLogging.debug = true
        val context = LoggerFactory.getILoggerFactory() as LoggerContext
        val recorder = Recorder().apply { this.context = context; start() }
        val root = context.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME) as Logger
        root.addAppender(recorder)
        out.parentFile?.mkdirs()
        out.bufferedWriter().use { writer ->
            for ((name, scenario) in scenarios) {
                val run = Run(name)
                recorder.run = run
                run.scenario()
                recorder.run = null
                writer.write(Json.compact(run.row()))
                writer.write("\n")
                run.parent.cancel()
            }
        }
        root.detachAppender(recorder)
    }
}
