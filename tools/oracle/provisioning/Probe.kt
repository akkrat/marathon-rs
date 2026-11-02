@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class, kotlin.time.ExperimentalTime::class)
package com.malinskiy.marathon.apple.ios.device

import com.google.gson.Gson
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.*
import com.malinskiy.marathon.apple.cmd.CommandExecutor
import com.malinskiy.marathon.apple.configuration.*
import com.malinskiy.marathon.apple.model.XcodeVersion
import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.mockito.Answers.RETURNS_DEEP_STUBS
import org.mockito.kotlin.*
import java.io.File
import java.time.Duration

private fun emit(case: String, value: Any?) = println(Gson().toJson(linkedMapOf("case" to case, "value" to value)))
private fun inventory(id: String) = SimctlListDevicesOutput(emptyList(), emptyList(), SimctlDeviceList(listOf(SimctlDevice(null, SimctlDevice.State.Shutdown, id, id, null, true, null, null, null))))
private fun fields(instance: Any, name: String): Any? = instance.javaClass.getDeclaredField(name).apply { isAccessible = true }.get(instance)
private fun connections(bin: AppleBinaryEnvironment, executor: CommandExecutor) { whenever(bin.commandExecutor).thenReturn(executor) }
private suspend fun list(bin: AppleBinaryEnvironment, id: String) { org.mockito.Mockito.reset(bin.xcrun.simctl.device); whenever(bin.xcrun.simctl.device.list()).thenReturn(inventory(id)) }

fun main(args: Array<String>) {
    for (suite in listOf(DeviceTrackerTest::class.java, SimulatorFactoryTest::class.java, SimulatorProvisionerTest::class.java)) {
        val methods = suite.declaredMethods.filter { it.getAnnotation(org.junit.jupiter.api.Test::class.java) != null || it.getAnnotation(org.junit.jupiter.params.ParameterizedTest::class.java) != null }.sortedBy { it.name }
        for (method in methods) for (n in if (method.parameterCount == 0) listOf<Int?>(null) else listOf(1,2,4)) {
            val instance = suite.getDeclaredConstructor().newInstance()
            if (instance is SimulatorFactoryTest) instance.root = File(args[0], "factory").apply { mkdirs() }
            if (instance is DeviceTrackerTest) instance.`setup yaml mapper`()
            try { if (n == null) method.invoke(instance) else method.invoke(instance,n) }
            catch (e: java.lang.reflect.InvocationTargetException) { throw e.cause ?: e }
            val value = linkedMapOf<String,Any?>("passed" to true, "invocation" to n)
            if (instance is SimulatorProvisionerTest) {
                value["peak"] = fields(instance,"peak")
                value["created"] = fields(instance,"created")
                value["registered"] = fields(instance,"registered")
                val connections = fields(instance,"connections")!!
                value["released"] = org.mockito.Mockito.mockingDetails(connections).invocations.count { it.method.name == "dispose" }
            }
            emit("test:${suite.simpleName}:${method.name}${n?.let { ":$it" } ?: ""}",value)
        }
    }
    runTest {
        for (age in listOf(9_999_999_999L,10_000_000_000L,10_000_000_001L)) {
            var now = 0L
            val bin = mock<AppleBinaryEnvironment>(defaultAnswer=RETURNS_DEEP_STUBS); val ex = mock<CommandExecutor>(); connections(bin,ex)
            list(bin,"old"); val c = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
            c.inventory(bin); now = age; list(bin,"new")
            emit("ttl:$age",c.inventory(bin).devices.devices.single().udid)
        }
        run {
            var wrapNow = Long.MAX_VALUE - 2
            val b = mock<AppleBinaryEnvironment>(defaultAnswer=RETURNS_DEEP_STUBS); connections(b,mock())
            list(b,"wrapped"); val c = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { wrapNow }
            c.inventory(b); wrapNow = Long.MIN_VALUE + 2; list(b,"fresh")
            emit("wrapping-age",c.inventory(b).devices.devices.single().udid)
        }
        var now = 0L
        val bin = mock<AppleBinaryEnvironment>(defaultAnswer=RETURNS_DEEP_STUBS); val ex = mock<CommandExecutor>(); connections(bin,ex)
        whenever(ex.connected).thenReturn(true); list(bin,"old")
        val c = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        c.inventory(bin); whenever(ex.connected).thenReturn(false); list(bin,"new")
        emit("same-disconnected",c.inventory(bin).devices.devices.single().udid)
        val replacement = mock<CommandExecutor>(); connections(bin,replacement)
        emit("replacement-disconnected",c.inventory(bin).devices.devices.single().udid)
        val throwing = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        connections(bin,ex); list(bin,"loaded"); throwing.inventory(bin)
        whenever(ex.connected).thenThrow(IllegalStateException("closing")); connections(bin,replacement); list(bin,"after-throw")
        emit("throwing-old-getter",throwing.inventory(bin).devices.devices.single().udid)
        var loadedFacts = 0
        fun facts(shareable: Boolean) = WorkerFacts(emptyMap(),XcodeVersion(16,4,0),"arm64",emptyList(),false,shareable)
        val factsContext = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        factsContext.workerFacts(ex) { loadedFacts++; facts(false) }
        runCatching { factsContext.workerFacts(ex) { loadedFacts++; error("partial") } }
        factsContext.workerFacts(ex) { loadedFacts++; facts(true) }
        factsContext.workerFacts(ex) { loadedFacts++; facts(true) }
        emit("facts-unshareable-partial-complete",loadedFacts)
        val cycles = listOf(Transport.Ssh("worker"),Transport.Ssh("worker",checkReachability=false))
        var loads = 0
        for(t in cycles) { WorkerProvisioningContext(t,Duration.ofSeconds(10)) { now }.workerFacts(ex) { loads++; facts(true) } }
        emit("fresh-cycles-facts",loads)
        val earlier = CompletableDeferred<SimctlListDevicesOutput>()
        val race = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        val answers = ArrayDeque<suspend () -> SimctlListDevicesOutput>(listOf({earlier.await()},{inventory("new")}))
        org.mockito.Mockito.reset(bin.xcrun.simctl.device)
        whenever(bin.xcrun.simctl.device.list()).doSuspendableAnswer { answers.removeFirst().invoke() }
        val first = async { race.refresh(bin) }; runCurrent(); race.refresh(bin); earlier.complete(inventory("old"))
        emit("refresh-earlier-own-output",first.await().devices.devices.single().udid)
        emit("refresh-publication",race.inventory(bin).devices.devices.single().udid)
        val before = CompletableDeferred<SimctlListDevicesOutput>()
        val failed = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        val failedAnswers = ArrayDeque<suspend () -> SimctlListDevicesOutput>(listOf({before.await()},{error("later failure")}))
        org.mockito.Mockito.reset(bin.xcrun.simctl.device)
        whenever(bin.xcrun.simctl.device.list()).doSuspendableAnswer { failedAnswers.removeFirst().invoke() }
        val older = async { failed.refresh(bin) }; runCurrent(); runCatching { failed.refresh(bin) }; before.complete(inventory("older-success")); older.await()
        emit("refresh-later-failure",failed.inventory(bin).devices.devices.single().udid)
        val slow = WorkerProvisioningContext(Transport.Local,Duration.ofSeconds(10)) { now }
        org.mockito.Mockito.reset(bin.xcrun.simctl.device)
        whenever(bin.xcrun.simctl.device.list()).doSuspendableAnswer { now += 10_000_000_001; inventory("slow") }
        slow.inventory(bin); list(bin,"fresh"); emit("request-start-age",slow.inventory(bin).devices.devices.single().udid)
    }
    // Frozen TimingLog reads the message clock before its own elapsed clock.
    val events = ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply { start() }
    val timings = org.slf4j.LoggerFactory.getLogger("Timings") as ch.qos.logback.classic.Logger
    timings.addAppender(events)
    val readings = java.util.ArrayDeque<Long>(listOf(1000L,1010L,1025L))
    val clock = object : java.time.Clock() {
        override fun getZone() = java.time.ZoneOffset.UTC
        override fun withZone(zone: java.time.ZoneId) = this
        override fun millis() = readings.removeFirst()
        override fun instant() = java.time.Instant.ofEpochMilli(millis())
    }
    val timer = com.malinskiy.marathon.time.SystemTimer(clock)
    com.malinskiy.marathon.log.MarathonLogging.debug = false
    com.malinskiy.marathon.log.TimingLog.info(timer) { "Worker local: connected in 2 ms, planned 4 simulators in 3 ms, provisioned them in ${timer.elapsedTimeMillis - 5} ms" }
    emit("timing-read-order",events.list.last().formattedMessage)
    timings.detachAppender(events)
    runTest {
        val trace = mutableListOf<String>()
        val connections = mock<com.malinskiy.marathon.apple.device.ConnectionFactory>()
        val factory = mock<SimulatorFactory>()
        val executor = mock<CommandExecutor>(); whenever(executor.connected).thenReturn(true)
        val bridge = mock<com.malinskiy.marathon.apple.cmd.FileBridge>()
        val bin = mock<AppleBinaryEnvironment>(defaultAnswer=RETURNS_DEEP_STUBS)
        val profile = AppleTarget.SimulatorProfile("iPhone X",newNamePrefix="trace")
        whenever(bin.xcrun.simctl.device.list()).thenReturn(SimctlListDevicesOutput(listOf(SimctlDeviceType("iPhone",profile.fullyQualifiedDeviceTypeId)),emptyList(),SimctlDeviceList(emptyList())))
        var count = 0
        whenever(bin.xcrun.simctl.simulator.create(any(),any(),anyOrNull())).thenAnswer { "new-${++count}".also { trace.add("create:$it") } }
        whenever(bin.xcrun.simctl.simulator.delete(any())).doSuspendableAnswer { invocation -> trace.add("delete:${invocation.getArgument<String>(0)}"); true }
        whenever(connections.create(any())).thenAnswer { trace.add("acquire"); executor to bridge }
        whenever(connections.dispose(any())).thenAnswer { trace.add("release"); Unit }
        whenever(factory.create(any(),any(),any(),any(),any())).doSuspendableAnswer { invocation ->
            val id = invocation.getArgument<String>(3); trace.add("setup:$id")
            if(id=="new-1") error("setup failed")
            delay(100); mock<com.malinskiy.marathon.apple.ios.AppleSimulatorDevice>()
        }
        val provisioner = SimulatorProvisioner(connections,factory,{id,_ -> trace.add("register:$id")}, concurrency=1)
        val failures = provisioner.provision(ProvisioningPlan(emptySet(),List(2){profile},emptySet()),Transport.Ssh("worker"),bin)
        emit("cleanup-order",trace)
        emit("cleanup-failures",failures.map { mapOf("created" to (it is FailedSetup.Created), "unreachable" to it.unreachable) })
    }
    runTest {
        val test = SimulatorFactoryTest().apply { root = File(args[0],"suppressed").apply { mkdirs() } }
        val executor = mock<CommandExecutor>()
        val record = SimctlDevice(null,SimctlDevice.State.Shutdown,"test","udid",null,true,null,null,null)
        org.mockito.Mockito.mockConstruction(AppleBinaryEnvironment::class.java, org.mockito.Mockito.withSettings().defaultAnswer(RETURNS_DEEP_STUBS)) { bin, _ ->
            runBlocking { whenever(bin.xcrun.simctl.device.list()).thenReturn(SimctlListDevicesOutput(emptyList(),emptyList(),SimctlDeviceList(listOf(record)))) }
        }.use {
            org.mockito.Mockito.mockConstruction(com.malinskiy.marathon.apple.ios.AppleSimulatorDevice::class.java) { device, _ ->
                runBlocking {
                    doThrow(IllegalStateException("setup")).whenever(device).setup(any(),any())
                    doThrow(IllegalStateException("dispose")).whenever(device).dispose()
                }
            }.use {
                val factory = test.javaClass.getDeclaredMethod("factory").apply { isAccessible=true }.invoke(test) as SimulatorFactory
                val failed = runCatching { factory.create(Transport.Local,executor,mock(),"udid",WorkerProvisioningContext(Transport.Local)) }.exceptionOrNull()!!
                emit("factory-suppressed",mapOf("original" to failed.message,"suppressed" to failed.suppressed.map { it.message }))
            }
        }
    }
    runTest {
        val vendor = com.malinskiy.marathon.config.vendor.VendorConfiguration.IOSConfiguration()
        val config = com.malinskiy.marathon.config.Configuration.Builder("probe",File(args[0],"timed")).apply { vendorConfiguration=vendor }.build()
        val readings = java.util.ArrayDeque<Long>(listOf(1000L,1010L,1020L,1030L))
        val clock = object : java.time.Clock() {
            override fun getZone() = java.time.ZoneOffset.UTC
            override fun withZone(zone: java.time.ZoneId) = this
            override fun millis() = readings.removeFirst()
            override fun instant() = java.time.Instant.ofEpochMilli(millis())
        }
        val timer = com.malinskiy.marathon.time.SystemTimer(clock)
        val captured = ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply { start() }
        timings.addAppender(captured)
        org.mockito.Mockito.mockConstruction(AppleBinaryEnvironment::class.java,org.mockito.Mockito.withSettings().defaultAnswer(RETURNS_DEEP_STUBS)) { bin,_ -> runBlocking { list(bin,"udid") } }.use {
            org.mockito.Mockito.mockConstruction(com.malinskiy.marathon.apple.ios.AppleSimulatorDevice::class.java) { device,_ -> whenever(device.serialNumber).thenReturn("udid@local") }.use {
                val factory = SimulatorFactory(config,vendor,com.malinskiy.marathon.apple.AppleTestBundleIdentifier(),Gson(),com.malinskiy.marathon.analytics.internal.pub.Track(),timer)
                factory.create(Transport.Local,mock(),mock(),"udid",WorkerProvisioningContext(Transport.Local))
            }
        }
        emit("factory-timing-read-order",captured.list.single().formattedMessage)
        timings.detachAppender(captured)
    }
    runTest {
        val vendor = com.malinskiy.marathon.config.vendor.VendorConfiguration.IOSConfiguration()
        val timer = com.malinskiy.marathon.time.SystemTimer(java.time.Clock.systemUTC())
        val planner = FrozenPlanner(vendor,timer)
        val transport = Transport.Ssh("worker")
        val profile = AppleTarget.SimulatorProfile("iPhone X")
        val bin = mock<AppleBinaryEnvironment>(defaultAnswer=RETURNS_DEEP_STUBS)
        val records = listOf("reuse-first","reuse-second").map { id -> inventory(id).devices.devices.single().copy(deviceTypeIdentifier=profile.fullyQualifiedDeviceTypeId) }
        whenever(bin.xcrun.simctl.device.list()).thenReturn(SimctlListDevicesOutput(emptyList(),emptyList(),SimctlDeviceList(records)))
        val reused = planner.plan(transport,bin,List(3){profile})
        emit("planner-reuse-order",mapOf("reused" to reused.reused.keys.toList(), "created" to reused.needsProvisioning.size))
        val captured = ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply { start() }
        val logger = org.slf4j.LoggerFactory.getLogger("AppleSimulatorProvider") as ch.qos.logback.classic.Logger
        logger.level = ch.qos.logback.classic.Level.WARN
        logger.addAppender(captured)
        planner.plan(transport,bin,listOf(AppleTarget.Simulator("missing"),AppleTarget.Host))
        emit("planner-log-order",captured.list.map { it.level.toString() })
        logger.detachAppender(captured)
        val shutdown = mutableListOf<String>()
        whenever(bin.xcrun.simctl.device.list()).thenReturn(SimctlListDevicesOutput(emptyList(),emptyList(),SimctlDeviceList(records.mapIndexed { index, it -> it.copy(udid=listOf("a","b")[index],state=SimctlDevice.State.Booted) })))
        whenever(bin.xcrun.simctl.simulator.shutdown(any())).doSuspendableAnswer { call ->
            val id = call.getArgument<String>(0); shutdown.add("start:$id"); delay(10); shutdown.add("end:$id"); true
        }
        planner.plan(transport,bin,emptyList())
        emit("planner-shutdown",shutdown)
    }
    val original = Transport.Ssh("worker")
    val changed = original.copy(checkReachability=false)
    val profile = AppleTarget.SimulatorProfile("iPhone X",newNamePrefix="original")
    val tracker = DeviceTracker()
    tracker.update(Marathondevices(listOf(Worker(original,List(3){profile}))))
    tracker.update(Marathondevices(emptyList()))
    val reappeared = tracker.update(Marathondevices(listOf(Worker(changed,listOf(profile.copy(newNamePrefix="changed"))))))
    emit("tracker-first-transport",reappeared.keys.single()==original)
    emit("tracker-retained-high-water",reappeared.values.single().size)
    for ((name, targets) in listOf("host-physical" to listOf(AppleTarget.Host,AppleTarget.Physical("host")), "profile-physical" to listOf(AppleTarget.Physical("${profile.fullyQualifiedDeviceTypeId}-"),profile))) {
        val result = runCatching { WorkerTracker(original).update(targets) }
        emit("S3-approved-divergence:$name",result.fold({mapOf("updates" to it.size)},{mapOf("error" to it.javaClass.name)}))
    }
}
