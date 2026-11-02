@file:OptIn(kotlin.time.ExperimentalTime::class, kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package com.malinskiy.marathon.apple.ios

import com.dd.plist.PropertyListParser
import com.google.gson.Gson
import org.junit.jupiter.api.Test
import java.io.File

@org.junit.jupiter.api.extension.ExtendWith(SimulatorCapture::class)
class SimulatorEvidenceTest {
    private fun emit(row: Any) { synchronized(File::class.java) { File(System.getProperty("simulator.evidence")).appendText(Gson().toJson(row) + "\n") } }
    @Test fun quirks() {
        for (value in listOf("<integer>1</integer>", "<integer>2147483648</integer>", "<real>1.0</real>", "<string>1</string>", "<true/>", "")) {
            val xml = "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>" + (if(value.isEmpty()) "" else "<key>state</key>$value") + "</dict></plist>"
            val state = (PropertyListParser.parse(xml.toByteArray()).toJavaObject() as Map<*,*>)["state"]
            emit(mapOf("kind" to "plistNumber", "xml" to value, "javaClass" to state?.javaClass?.name, "intCast" to (state as? Int)))
        }
        for (home in listOf("/Users/worker/Devices/a/data", "/data/users/a/data")) emit(mapOf("kind" to "root", "home" to home, "root" to home.substringBefore("/data")))
        for (line in listOf("  NO_EQUAL  ", " KEY = value = rest ", "HOME=  /Users/worker  ")) {
            val trimmed=line.trim()
            emit(mapOf("kind" to "env", "line" to line, "key" to trimmed.substringBefore('=').trim(), "value" to trimmed.substringAfter('=').trim()))
        }
    }
}

@org.junit.jupiter.api.extension.ExtendWith(SimulatorCapture::class)
class SimulatorSetupEvidenceTest {
    @org.junit.jupiter.api.io.TempDir lateinit var root: File
    private fun emit(row: Any) { synchronized(File::class.java) { File(System.getProperty("simulator.evidence")).appendText(Gson().toJson(row) + "\n") } }
    private fun field(d: Any, name: String): Any? = d.javaClass.getDeclaredField(name).apply { isAccessible = true }.get(d)
    @Test fun setupAndCommands() = kotlinx.coroutines.runBlocking<Unit> {
        for (scenario in listOf("uninitialized", "happy", "host-home", "blank-home", "missing-home", "root-first-data", "plist-device-type", "missing-plist", "malformed-plist", "env-line", "failed-displays")) {
            System.setProperty("simulator.scenario", "setup:$scenario")
            val worker = com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker().apply { add("a") }
            var plistValue = "<integer>1</integer>"
            var readyAt: Long? = null
            worker.before = { cmd -> when {
                cmd == listOf("env") && scenario == "host-home" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("USER=worker")
                cmd == listOf("env") && scenario == "blank-home" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("HOME= ")
                cmd == listOf("env") && scenario == "env-line" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("HOME=/Users/worker", " NO_EQUAL ", " KEY = value = rest ")
                cmd.lastOrNull() == "SIMULATOR_HOST_HOME" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("/Users/fallback")
                cmd == listOf("xcrun", "simctl", "getenv", "a", "HOME") && scenario == "missing-home" -> com.malinskiy.marathon.apple.cmd.CommandResult(emptyList(), emptyList(), 1)
                cmd == listOf("xcrun", "simctl", "getenv", "a", "HOME") && scenario == "root-first-data" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("/data/workers/a/data")
                cmd.firstOrNull() == "cat" -> when(scenario) {
                    "missing-plist" -> com.malinskiy.marathon.apple.cmd.CommandResult(emptyList(), emptyList(), 1)
                    "malformed-plist" -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok("malformed")
                    else -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok(("<plist><dict>" + (if(plistValue.isEmpty()) "" else "<key>state</key>$plistValue") + "<key>deviceType</key><string> plist-type </string></dict></plist>"))
                }
                cmd.firstOrNull() == "/usr/sbin/system_profiler" && scenario == "failed-displays" -> com.malinskiy.marathon.apple.cmd.CommandResult(listOf("spdisplays_metalfamily"),emptyList(),1)
                cmd.take(3) == listOf("xcrun","simctl","spawn") -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok((if(readyAt == null || SimulatorProbeClock.nanoTime() >= readyAt!!) "A" else "D") + " com.apple.springboard.services")
                cmd.take(3) == listOf("xcrun","simctl","boot") -> { plistValue="<integer>3</integer>"; com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok() }
                cmd.take(3) == listOf("xcrun","simctl","shutdown") -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok()
                cmd.firstOrNull() == "sqlite3" || cmd.take(4) == listOf("xcrun","simctl","io","a") -> com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok()
                else -> null
            } }
            val vendor = com.malinskiy.marathon.config.vendor.VendorConfiguration.IOSConfiguration(lifecycleConfiguration = com.malinskiy.marathon.config.vendor.apple.ios.LifecycleConfiguration(onDispose=if(scenario=="uninitialized") setOf(com.malinskiy.marathon.config.vendor.apple.ios.LifecycleAction.SHUTDOWN) else emptySet()), timeoutConfiguration = com.malinskiy.marathon.config.vendor.apple.TimeoutConfiguration(bootWait=java.time.Duration.ofMillis(500)))
            val config = com.malinskiy.marathon.config.Configuration.Builder("test", root).apply { vendorConfiguration = vendor }.build()
            val bridge = org.mockito.kotlin.mock<com.malinskiy.marathon.apple.cmd.FileBridge>()
            org.mockito.kotlin.whenever(bridge.receive(org.mockito.kotlin.any(), org.mockito.kotlin.any())).thenReturn(true)
            val d = AppleSimulatorDevice("a", com.malinskiy.marathon.apple.configuration.Transport.Local, com.malinskiy.marathon.apple.model.Sdk.IPHONESIMULATOR, com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment(worker, config, vendor.timeoutConfiguration, com.google.gson.GsonBuilder().registerTypeAdapter(com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceList::class.java, com.malinskiy.marathon.apple.bin.xcrun.simctl.model.SimctlDeviceListDeserializer()).create()), org.mockito.kotlin.mock(), com.malinskiy.marathon.io.FileManager(0,0,root), config, vendor, worker, bridge, com.malinskiy.marathon.apple.cmd.SharedTransfers(), com.malinskiy.marathon.apple.ArtifactProcessor(), com.malinskiy.marathon.analytics.internal.pub.Track(), org.mockito.kotlin.mock(), bootPollInterval=java.time.Duration.ofMillis(20))
            try {
                if(scenario == "uninitialized") {
                    try { d.boot() } catch(e:Exception) { emit(mapOf("kind" to "partialField", "field" to "devicePlistPath", "class" to e.javaClass.name, "message" to e.message)) }
                    try { d.grant(com.malinskiy.marathon.config.vendor.apple.ios.Permission.UserTracking,"app") } catch(e:Exception) { emit(mapOf("kind" to "partialField", "field" to "env", "class" to e.javaClass.name, "message" to e.message)) }
                    d.disposeAndJoin()
                    emit(mapOf("kind" to "partialDisposal", "commands" to worker.commands.size, "disposed" to d.disposed))
                    continue
                }
                if(scenario == "plist-device-type") {
                    val cycle=com.malinskiy.marathon.apple.ios.device.WorkerProvisioningContext(com.malinskiy.marathon.apple.configuration.Transport.Local)
                    val record=cycle.simulator("a",d.binaryEnvironment).copy(deviceTypeIdentifier=null)
                    d.setup(record,cycle)
                } else d.setup()
                emit(mapOf("kind" to "setup", "scenario" to scenario, "root" to field(d,"rootPath"), "home" to field(d,"home"), "env" to field(d,"env"), "deviceType" to field(d,"deviceType"), "healthy" to d.healthy, "features" to d.deviceFeatures.map { it.name }, "os" to d.operatingSystem.version))
                if(scenario == "host-home") emit(mapOf("kind" to "grant", "result" to d.grant(com.malinskiy.marathon.config.vendor.apple.ios.Permission.UserTracking,"app")))
                if(scenario == "happy") {
                    d.getScreenshot(java.time.Duration.ofMillis(-1),File(root,"one.png"))
                    d.getScreenshot(java.time.Duration.ofMillis(999),File(root,"two.png"))
                    worker.connected=false
                    emit(mapOf("kind" to "disconnectedHealth", "healthy" to d.healthy, "network" to d.networkState.name))
                    worker.connected=true
                    for(value in listOf("<integer>1</integer>","<integer>2147483648</integer>","<real>1.0</real>","<string>1</string>","<true/>","")) {
                        plistValue=value
                        try { emit(mapOf("kind" to "actualState", "xml" to value,"shutdown" to d.shutdown())) }
                        catch(e:Exception) { emit(mapOf("kind" to "actualState", "xml" to value,"errorClass" to e.javaClass.name)) }
                    }
                    for (ready in listOf(500L,501L)) kotlinx.coroutines.test.runTest {
                        plistValue="<integer>1</integer>"
                        readyAt=ready*1_000_000L
                        val beforeReads=worker.commands.count { it.firstOrNull()=="cat" }
                        SimulatorProbeClock.now={testScheduler.currentTime*1_000_000L}
                        try { val booted=d.boot(); emit(mapOf("kind" to "bootDeadline","readyMillis" to ready,"booted" to booted,"elapsedMillis" to testScheduler.currentTime,"reads" to worker.commands.count { it.firstOrNull()=="cat" }-beforeReads)) }
                        finally { SimulatorProbeClock.now=null; readyAt=null }
                    }
                    plistValue="<integer>3</integer>"
                    val before=worker.commands.count { it.firstOrNull()=="cat" }
                    kotlinx.coroutines.test.runTest {
                        val start=testScheduler.currentTime
                        val shutdown=d.shutdown()
                        emit(mapOf("kind" to "fixedWait", "shutdown" to shutdown,"elapsedMillis" to testScheduler.currentTime-start,"reads" to worker.commands.count { it.firstOrNull()=="cat" }-before))
                    }
                    System.setProperty("simulator.scenario","prepare:installation-fails")
                    val beforePrepare=worker.commands.size
                    val preparation=runCatching { d.prepare(config) }.exceptionOrNull()
                    emit(mapOf("kind" to "parallelPrepare", "failure" to preparation?.javaClass?.name,"message" to preparation?.message,"argv" to worker.commands.drop(beforePrepare)))
                    System.setProperty("simulator.scenario","setup:happy")
                }
            } catch(e:Exception) { emit(mapOf("kind" to "setupFailure", "scenario" to scenario,"class" to e.javaClass.name,"message" to e.message)) }
            finally { d.disposeAndJoin() }
        }
    }
}

@org.junit.jupiter.api.extension.ExtendWith(SimulatorCapture::class)
class SimulatorClassifierEvidenceTest {
    @org.junit.jupiter.api.io.TempDir lateinit var root: File
    private fun emit(row: Any) { synchronized(File::class.java) { File(System.getProperty("simulator.evidence")).appendText(Gson().toJson(row) + "\n") } }
    private fun chain(e:Throwable?):Any? = e?.let { mapOf("class" to it.javaClass.name,"message" to it.message,"cause" to chain(it.cause),"suppressed" to it.suppressed.map { s->chain(s) }) }
    @Test fun failuresBeforeRunner() = kotlinx.coroutines.runBlocking<Unit> {
        for(name in listOf("net.schmizz.sshj.connection.ConnectionException","net.schmizz.sshj.transport.TransportException","net.schmizz.sshj.connection.channel.OpenFailException","java.lang.IllegalStateException","java.io.IOException","java.util.concurrent.CancellationException")) {
            System.setProperty("simulator.scenario","classification:$name")
            val worker=com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker()
            val type=Class.forName(name)
            val ctor=type.constructors.first { it.parameterTypes.all { p->p==String::class.java || p.isEnum } }
            val problem=ctor.newInstance(*ctor.parameterTypes.map { p->if(p==String::class.java) "probe" else p.enumConstants[0] }.toTypedArray()) as Exception
            worker.before={ if(it.take(3)==listOf("xcrun","simctl","privacy")) throw problem else com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker.ok() }
            val vendor=com.malinskiy.marathon.config.vendor.VendorConfiguration.IOSConfiguration(permissions=com.malinskiy.marathon.config.vendor.apple.ios.PermissionsConfiguration(setOf(com.malinskiy.marathon.config.vendor.apple.ios.Permission.Calendar),com.malinskiy.marathon.config.vendor.apple.ios.GrantLifecycle.BEFORE_EACH_BATCH))
            val config=com.malinskiy.marathon.config.Configuration.Builder("test",root).apply {vendorConfiguration=vendor}.build()
            val processor=com.malinskiy.marathon.apple.ArtifactProcessor()
            val d=AppleSimulatorDevice("a",com.malinskiy.marathon.apple.configuration.Transport.Local,com.malinskiy.marathon.apple.model.Sdk.IPHONESIMULATOR,com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment(worker,config,vendor.timeoutConfiguration,Gson()),org.mockito.kotlin.mock(),com.malinskiy.marathon.io.FileManager(0,0,root),config,vendor,worker,org.mockito.kotlin.mock(),com.malinskiy.marathon.apple.cmd.SharedTransfers(),processor,com.malinskiy.marathon.analytics.internal.pub.Track(),org.mockito.kotlin.mock())
            val bundle=org.mockito.kotlin.mock<com.malinskiy.marathon.apple.model.AppleTestBundle>()
            org.mockito.kotlin.whenever(bundle.appId).thenReturn("app")
            d.javaClass.getDeclaredField("testBundle").apply {isAccessible=true}.set(d,bundle)
            try {d.execute(config,com.malinskiy.marathon.device.DevicePoolId("pool"),com.malinskiy.marathon.test.TestBatch(emptyList()),kotlinx.coroutines.CompletableDeferred())}
            catch(e:Exception) {emit(mapOf("kind" to "classification","inputClass" to name,"failure" to chain(e),"lost" to d.lost,"retired" to d.retired,"waiting" to (d.waitingSince!=null)))}
            finally {d.disposeAndJoin();processor.closeAndJoin()}
        }
    }
}
