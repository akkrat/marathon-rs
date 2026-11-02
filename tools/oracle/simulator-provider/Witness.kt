package com.malinskiy.marathon.apple.ios.device

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.apple.AppleTestBundleIdentifier
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceList
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceListDeserializer
import com.malinskiy.marathon.apple.cmd.FileBridge
import com.malinskiy.marathon.apple.configuration.*
import com.malinskiy.marathon.apple.device.ConnectionFactory
import com.malinskiy.marathon.apple.ios.AppleSimulatorDevice
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.apple.ios.LifecycleConfiguration
import com.malinskiy.marathon.config.vendor.apple.DeviceProvider as Source
import com.malinskiy.marathon.device.DeviceProvider.DeviceEvent
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import org.mockito.kotlin.*
import org.slf4j.LoggerFactory
import java.io.*
import java.net.*
import java.time.Clock

private class Harness(val root: File, val vendor: VendorConfiguration.IOSConfiguration = VendorConfiguration.IOSConfiguration(lifecycleConfiguration=LifecycleConfiguration(onDispose=emptySet())), val factory: SimulatorFactory? = null) {
    val worker = FakeSimulatorWorker(hostId="local")
    val connections = mock<ConnectionFactory>()
    val trace = java.util.concurrent.CopyOnWriteArrayList<String>()
    val logger = LoggerFactory.getLogger("AppleSimulatorProvider") as Logger
    val appender = ListAppender<ILoggingEvent>().apply { list = java.util.concurrent.CopyOnWriteArrayList<ILoggingEvent>(); start() }
    val config = Configuration.Builder("test",root).apply { vendorConfiguration=vendor }.build()
    val provider: AppleSimulatorProvider
    init {
        logger.addAppender(appender)
        whenever(connections.create(any())).thenAnswer { trace.add("acquire"); worker to mock<FileBridge>() }
        whenever(connections.connectWorker(any())).thenAnswer { trace.add("acquire"); worker }
        whenever(connections.createOrThrow(any())).thenAnswer { trace.add("acquire"); worker to mock<FileBridge>() }
        whenever(connections.dispose(any())).thenAnswer { trace.add("release"); Unit }
        provider=AppleSimulatorProvider(config,vendor,AppleTestBundleIdentifier(),GsonBuilder().registerTypeAdapter(SimctlDeviceList::class.java,SimctlDeviceListDeserializer()).create(),ObjectMapper(YAMLFactory()).registerModule(KotlinModule.Builder().configure(com.fasterxml.jackson.module.kotlin.KotlinFeature.SingletonSupport,true).build()),Track(),SystemTimer(Clock.systemUTC()),connections,factory)
    }
    suspend fun close() { provider.terminate(); logger.detachAppender(appender) }
    suspend fun connected() = withTimeout(5000) { (provider.subscribe().receive() as DeviceEvent.DeviceConnected).device as AppleSimulatorDevice }
    fun lines()=appender.list.map { it.formattedMessage }
    fun monitor()=(AppleSimulatorProvider::class.java.getDeclaredField("monitoringJob").apply {isAccessible=true}.get(provider) as Job).isActive
}
private fun snapshot(vararg ids:String)=Marathondevices(listOf(Worker(Transport.Local,ids.map { AppleTarget.Simulator(it) })))
private fun payload(ids:List<String>)="workers:\n- transport:\n    type: local\n  devices: " + if(ids.isEmpty()) "[]\n" else "\n"+ids.joinToString("") { "  - type: simulator\n    udid: $it\n" }
private fun frame(bytes:ByteArray)=ByteArrayOutputStream().also { b->DataOutputStream(b).apply {writeInt(0x01020304);writeInt(bytes.size);write(bytes)} }.toByteArray()
private suspend fun until(predicate:()->Boolean)=withTimeout(5000){while(!predicate())delay(5)}
private fun chain(error:Throwable?, root:File):Any?=error?.let{mapOf("class" to it.javaClass.name,"message" to it.message?.replace(root.absolutePath,"<ROOT>"),"cause" to chain(it.cause,root))}
private fun field(provider:AppleSimulatorProvider,name:String):Any=AppleSimulatorProvider::class.java.getDeclaredField(name).apply{isAccessible=true}.get(provider)
private fun register(provider:AppleSimulatorProvider,device:AppleSimulatorDevice) {
    AppleSimulatorProvider::class.java.getDeclaredMethod("connect",String::class.java,AppleSimulatorDevice::class.java).apply{isAccessible=true}.invoke(provider,"a@local",device)
}
fun supplemental(root:File)=runBlocking {
    suspend fun fresh(name:String,body:suspend (Harness)->Unit) { val h=Harness(File(root,name).apply{mkdirs()});try{body(h)}finally{h.close()} }
    fresh("failure-description") { h ->
        val method=AppleSimulatorProvider::class.java.getDeclaredMethod("describe",Throwable::class.java).apply{isAccessible=true}
        emit("failure-description",mapOf("withMessages" to method.invoke(h.provider,IllegalStateException("plan failed",IllegalArgumentException("root"))),"withoutMessage" to method.invoke(h.provider,IllegalStateException())))
    }
    fresh("stale-borrow") { h->
        h.worker.add("a");h.provider.processUpdate(snapshot("a"));val device=h.connected();h.provider.processUpdate(snapshot());val event=h.provider.subscribe().receive();val borrowed=h.provider.borrow()
        emit("A80-stale-borrow",mapOf("same" to (borrowed===device),"disposed" to device.disposed,"disconnect" to (event is DeviceEvent.DeviceDisconnected),"releasesBeforeTerminate" to h.trace.count{it=="release"}))
        h.provider.processUpdate(snapshot("a"));val replacement=h.connected();check(replacement!==device);h.close();emit("replaced-leases",mapOf("acquired" to h.trace.count{it=="acquire"},"released" to h.trace.count{it=="release"}, "trace" to h.trace))
    }
    fresh("failed-tracking") {h->
        h.worker.add("a");h.worker.before={if(it.take(3)==listOf("xcrun","simctl","getenv")&&it.last()=="HOME") throw IllegalStateException("setup failed");null}
        h.provider.processUpdate(snapshot("a"));val first=h.worker.commands.size;h.provider.processUpdate(snapshot("a"))
        emit("A81-failed-setup-remains-tracked",mapOf("repeatCommands" to (h.worker.commands.size-first),"events" to h.provider.subscribe().tryReceive().isSuccess))
    }
    run {
        val h=Harness(File(root,"throwing-dispose").apply{mkdirs()})
        val device=mock<AppleSimulatorDevice>{on{udid} doReturn "a";on{serialNumber}doReturn "a@local";on{transport}doReturn Transport.Local;on{commandExecutor}doReturn h.worker}
        doThrow(IllegalStateException("dispose failed")).whenever(device).dispose()
        val connect=AppleSimulatorProvider::class.java.getDeclaredMethod("connect",String::class.java,AppleSimulatorDevice::class.java).apply{isAccessible=true};connect.invoke(h.provider,"a@local",device);h.connected();h.close()
        emit("A82-throwing-dispose-skips-release",mapOf("released" to h.trace.count{it=="release"}))
    }
    fresh("snapshot-deltas") { h ->
        h.worker.add("a", "b")
        h.provider.processUpdate(snapshot("a", "b")); val first = listOf(h.connected(), h.connected())
        val before = h.worker.commands.size; h.provider.processUpdate(snapshot("a", "b"))
        val repeated = h.worker.commands.size - before
        h.provider.processUpdate(Marathondevices(emptyList()))
        val disconnects = List(2) { (h.provider.subscribe().receive() as DeviceEvent.DeviceDisconnected).device.serialNumber }.sorted()
        emit("whole-worker-removal", mapOf("repeatCommands" to repeated, "disconnects" to disconnects, "disposed" to first.all { it.disposed }))
    }
    fresh("profile-shrink") { h ->
        val profile = AppleTarget.SimulatorProfile("iPhone 15 Pro", "iOS 18.6", "test")
        h.provider.processUpdate(Marathondevices(listOf(Worker(Transport.Local, List(3) { profile }))))
        val first = List(3) { h.connected() }
        h.provider.processUpdate(Marathondevices(listOf(Worker(Transport.Local, listOf(profile)))))
        h.provider.processUpdate(Marathondevices(emptyList()))
        emit("profile-high-water", mapOf("registered" to first.size, "disposed" to first.count { it.disposed }, "extraEvent" to h.provider.subscribe().tryReceive().isSuccess))
    }
    kotlinx.coroutines.test.runTest {
        val h=Harness(File(root,"borrow-cadence").apply{mkdirs()})
        try {
            val borrowed=async{h.provider.borrow()};testScheduler.runCurrent()
            testScheduler.advanceTimeBy(199)
            val device=mock<AppleSimulatorDevice>{on{udid}doReturn "a";on{serialNumber}doReturn "a@local";on{transport}doReturn Transport.Local;on{commandExecutor}doReturn h.worker}
            register(h.provider,device);testScheduler.runCurrent();check(!borrowed.isCompleted)
            testScheduler.advanceTimeBy(1);testScheduler.runCurrent();check(borrowed.await()===device)
            emit("borrow-cadence",mapOf("before200" to false,"at200" to true))
        } finally { h.close() }
    }
    fresh("registration-removal") { h ->
        val entered = java.util.concurrent.CountDownLatch(1); val release = java.util.concurrent.CountDownLatch(1)
        fun device() = mock<AppleSimulatorDevice> { on { udid } doReturn "a"; on { serialNumber } doReturn "a@local"; on { transport } doReturn Transport.Local; on { commandExecutor } doReturn h.worker }
        val old = device(); val replacement = device()
        register(h.provider,old); h.connected()
        doAnswer { entered.countDown(); check(release.await(5,java.util.concurrent.TimeUnit.SECONDS)); Unit }.whenever(old).dispose()
        (field(h.provider,"deviceTracker") as DeviceTracker).update(snapshot("a"))
        val replacing = async(Dispatchers.IO) { register(h.provider,replacement) }
        withContext(Dispatchers.IO) { check(entered.await(5,java.util.concurrent.TimeUnit.SECONDS)) }
        h.provider.processUpdate(Marathondevices(emptyList()))
        val disconnected = (h.provider.subscribe().receive() as DeviceEvent.DeviceDisconnected).device
        check(h.provider.subscribe().tryReceive().isFailure)
        release.countDown(); replacing.await(); val connected=h.connected()
        val rejected = runCatching { register(h.provider,device()) }.exceptionOrNull() as java.lang.reflect.InvocationTargetException
        emit("registration-removal-race",mapOf("disconnectNew" to (disconnected===replacement),"connectNew" to (connected===replacement),"failure" to chain(rejected.targetException,h.root),"skipped" to (field(h.provider,"skippedRemoved") as Set<*>).contains("a@local")))
    }
    fresh("live-eligibility") { h ->
        h.worker.add("a"); var retired=false
        val device=mock<AppleSimulatorDevice>{on{udid}doReturn "a";on{serialNumber}doReturn "a@local";on{transport}doReturn Transport.Local;on{commandExecutor}doReturn h.worker;on{disposed}doReturn true}
        whenever(device.retired).thenAnswer{retired};register(h.provider,device);h.connected()
        val bin=com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment(h.worker,h.config,h.vendor.timeoutConfiguration,GsonBuilder().registerTypeAdapter(SimctlDeviceList::class.java,SimctlDeviceListDeserializer()).create())
        suspend fun planned()=h.provider.plan(Transport.Local,bin,listOf(AppleTarget.Simulator("a"))).existingSimulators.sorted()
        val eligible=planned();val recovering=field(h.provider,"recovering") as MutableSet<AppleSimulatorDevice>;recovering.add(device);val during=planned();recovering.clear()
        val losses=field(h.provider,"losses") as MutableMap<String,Int>;losses["a@local"]=4;val exhausted=planned();losses.clear();retired=true;val retiredPlan=planned();retired=false
        val removed=field(h.provider,"removed") as MutableSet<String>;removed.add("a@local");val absent=planned();removed.clear();h.worker.connected=false;val disconnected=planned();h.worker.connected=true
        emit("live-plan-exclusions",mapOf("eligible" to eligible,"recovering" to during,"exhausted" to exhausted,"retired" to retiredPlan,"removed" to absent,"disconnected" to disconnected))
    }
    for((name,text)in listOf("empty" to "workers: []\n","mapping" to "workers: wrong\n","syntax" to "workers: [\n","null" to "null\n", "cycle" to "workers: \${PROVIDER_CYCLE}\n")) {
        val dir=File(root,"load-$name").apply{mkdirs()};val file=File(dir,"Marathondevices").apply{writeText(text)}
        val h=Harness(dir,VendorConfiguration.IOSConfiguration(devicesFile=file))
        try{val error=runCatching{h.provider.initialize()}.exceptionOrNull();emit("load-$name",chain(error,dir)?:mapOf("success" to true))}finally{h.close()}
    }
    run {
        val dir=File(root,"precedence").apply{mkdirs()};val file=File(dir,"Marathondevices").apply{writeText(payload(emptyList()))}
        ServerSocket(0,1,InetAddress.getLoopbackAddress()).use{server->server.soTimeout=150
            val h=Harness(dir,VendorConfiguration.IOSConfiguration(devicesFile=file,deviceProvider=Source.Dynamic("127.0.0.1",server.localPort)))
            try{h.provider.initialize();delay(50);val accepted=withContext(Dispatchers.IO){runCatching{server.accept().close()}.isSuccess};emit("static-over-dynamic",mapOf("accepted" to accepted,"monitor" to h.monitor()))}finally{h.close()}
        }
    }
    for(name in listOf("split-coalesced","negative","oversized","zero","partial-eof","partial-length-eof","partial-payload-eof","boundary-eof","malformed","backpressure")) {
        ServerSocket(0,1,InetAddress.getLoopbackAddress()).use{server->server.soTimeout=5000
            val dir=File(root,"tcp-$name").apply{mkdirs()};val h=Harness(dir,VendorConfiguration.IOSConfiguration(devicesFile=File(dir,"absent"),deviceProvider=Source.Dynamic("127.0.0.1",server.localPort),lifecycleConfiguration=LifecycleConfiguration(onDispose=emptySet())))
            try{h.worker.add("a","b");h.provider.initialize();val socket=withContext(Dispatchers.IO){server.accept()};val output=socket.getOutputStream();val devices=mutableListOf<String>()
                when(name){
                    "split-coalesced"->{val first=frame(payload(listOf("a")).toByteArray());output.write(first,0,3);output.flush();delay(25);output.write(first,3,first.size-3);output.write(frame(payload(listOf("a","b")).toByteArray()));output.flush();repeat(2){devices.add(h.connected().udid)};socket.close()}
                    "negative","oversized"->{DataOutputStream(output).apply{writeInt(0);writeInt(if(name=="negative")-1 else (1 shl 20)+1);flush()};socket.close()}
                    "zero"->{output.write(frame(byteArrayOf()));output.flush();socket.close()}
                    "partial-eof"->{output.write(byteArrayOf(0,0));output.flush();delay(100);socket.close()}
                    "partial-length-eof"->{output.write(frame(payload(emptyList()).toByteArray()).copyOf(5));output.flush();delay(100);socket.close()}
                    "partial-payload-eof"->{output.write(frame(payload(emptyList()).toByteArray()).copyOf(9));output.flush();delay(100);socket.close()}
                    "boundary-eof"->{socket.close()}
                    "malformed"->{output.write(frame("workers: wrong\n".toByteArray()));output.flush();socket.close()}
                    "backpressure"->{
                        val entered=CompletableDeferred<Unit>();val release=CompletableDeferred<Unit>()
                        h.worker.before={if(it==FakeSimulatorWorker.LIST){entered.complete(Unit);release.await()};null}
                        output.write(frame(payload(listOf("a")).toByteArray())+frame(payload(listOf("a","b")).toByteArray())+frame("workers: wrong\n".toByteArray()));output.flush();entered.await()
                        try{delay(50);check(h.lines().none{it.startsWith("Error occurred:")})}finally{release.complete(Unit)}
                        repeat(2){devices.add(h.connected().udid)};socket.close()
                    }
                }
                until{h.lines().any{it.startsWith("Error occurred:")}}
                delay(40)
                emit("tcp-$name",mapOf("devices" to devices.sorted(),"errors" to h.lines().filter{it.startsWith("Error occurred:")},"monitor" to h.monitor()))
            }finally{h.close()}
        }
    }
    run {
        val dir=File(root,"invalid-port").apply{mkdirs()}
        val h=Harness(dir,VendorConfiguration.IOSConfiguration(devicesFile=File(dir,"absent"),deviceProvider=Source.Dynamic("127.0.0.1",65536)))
        try {h.provider.initialize();until{h.lines().any{it.startsWith("Error occurred:")}};emit("invalid-port",mapOf("errors" to h.lines().filter{it.startsWith("Error occurred:")},"monitor" to h.monitor()))}finally{h.close()}
    }
    run {
        val dir=File(root,"refused").apply{mkdirs()};val port=ServerSocket(0).use{it.localPort}
        val h=Harness(dir,VendorConfiguration.IOSConfiguration(devicesFile=File(dir,"absent"),deviceProvider=Source.Dynamic("127.0.0.1",port)))
        try{h.provider.initialize();until{h.lines().contains("Connection refused, retrying in 5 seconds...")};ServerSocket(port,1,InetAddress.getLoopbackAddress()).use{server->server.soTimeout=7000;withContext(Dispatchers.IO){server.accept()}.use{it.getOutputStream().write(frame(payload(emptyList()).toByteArray()))}};until{h.lines().any{it.startsWith("Error occurred:")}};emit("refused-then-success",mapOf("warnings" to h.lines().count{it=="Connection refused, retrying in 5 seconds..."},"monitor" to h.monitor()))}finally{h.close()}
    }
}
