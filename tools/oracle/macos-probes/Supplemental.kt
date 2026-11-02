import com.google.gson.Gson
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.malinskiy.marathon.apple.macos.*
import com.malinskiy.marathon.apple.*
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.apple.configuration.Transport
import com.malinskiy.marathon.apple.device.ConnectionFactory
import com.malinskiy.marathon.apple.model.*
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.apple.macos.Permission
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.io.FileManager
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.time.SystemTimer
import com.malinskiy.marathon.apple.logparser.parser.*
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import org.mockito.kotlin.*
import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import org.slf4j.LoggerFactory
import java.io.File
import java.nio.charset.Charset
import java.time.Duration
import java.time.Clock

private class Done(private val result: CommandResult): CommandSession {
    override val stdout = Channel<String>(Channel.UNLIMITED).apply { result.stdout.forEach { trySend(it) }; close() }
    override val stderr = Channel<String>(Channel.UNLIMITED).apply { result.stderr.forEach { trySend(it) }; close() }
    override val exitCode = CompletableDeferred(result.exitCode)
    override val isAlive = false
    override suspend fun await() = result
    override suspend fun drain() = Unit
    override fun terminate() = Unit
    override fun kill() = Unit
    override fun close() = Unit
}
private class Worker(val mode:String): CommandExecutor {
    override val host:CommandHost=KotlinProcessHost("local")
    override val logger=MarathonLogging.logger {}
    override val connected=true
    val requests=mutableListOf<Any>()
    var sql=0
    var fail:Throwable?=null
    val started=CompletableDeferred<Unit>()
    val events=mutableListOf<String>()
    override suspend fun execute(command:List<String>,timeout:Duration,idleTimeout:Duration,env:Map<String,String>,workdir:String?,charset:Charset):CommandSession {
        requests += linkedMapOf("argv" to command,"timeout" to timeout.toString(),"idle" to idleTimeout.toString(),"env" to env,"workdir" to workdir)
        if(command.take(3)==listOf("xcrun","xcodebuild","test-without-building")) fail?.let { throw it }
        if(mode=="dispose-execute" && command.take(3)==listOf("xcrun","xcodebuild","test-without-building")) {
            started.complete(Unit)
            return object: CommandSession {
                override val stdout=Channel<String>();override val stderr=Channel<String>()
                override val exitCode=CompletableDeferred<Int?>();override val isAlive=true
                override suspend fun await()=throw UnsupportedOperationException()
                override suspend fun drain()=Unit;override fun terminate()=Unit;override fun kill()=Unit
                override fun close() { events += "session-closed" }
            }
        }
        var exit=0
        val out=when {
            command==listOf("env") -> if(mode=="setup-failure") {exit=1;emptyList()} else listOf(" HOME = /Different/home ","USER=tester"," A = first "," A = last ","bare").filterNot { mode=="missing-user" && it.startsWith("USER=") }
            command==listOf("xcrun","xcodebuild","-version") -> listOf("Xcode 16.4","Build version 16F6")
            command.any { it.contains("system_profiler") } -> if(mode=="fallback") listOf(" ") else listOf("Provisioning UDID: udid")
            command.any { it.contains("IOPlatformUUID") } -> listOf("fallback")
            command.any { it.contains("manufacturer") } -> listOf("Apple")
            command.firstOrNull()=="sh" -> listOf("Mac14,6")
            command.firstOrNull()=="sw_vers" -> listOf(when(mode) { "os10"->"10.15"; "unknown"->"Unknown";else->"11.0" })
            command==listOf("uname","-m") -> listOf(" arm64 ")
            command==listOf("csrutil","status") -> when(mode) { "sip-full"->listOf("System Integrity Protection status: disabled.");"sip-fail"->{exit=1;emptyList()};else->listOf("disabled") }
            command.firstOrNull()=="sqlite3" -> {sql++;if((mode=="sql-first" && sql==1)||(mode=="sql-second" && sql==2))exit=1;emptyList()}
            command.take(2)==listOf("xcrun","xcresulttool") -> {exit=1;emptyList()}
            else->emptyList()
        }
        return Done(CommandResult(out,emptyList(),exit))
    }
    override fun close()=Unit
}
private fun error(e:Throwable?):List<Map<String,String?>> = generateSequence(e) { it.cause }.map { linkedMapOf("class" to it.javaClass.name,"message" to it.message) }.toList()
private fun row(root:File, data:Any) {
    var text=Gson().toJson(data).replace(root.absolutePath,"$"+"ROOT")
    text=text.replace(Regex("0,[0-9]{10}\\);"),"0,<epoch>);")
    // Object identity suffixes have no cross-runtime equality contract (D12).
    text=text.replace(Regex("@[0-9a-f]{4,16}"),"@<identity>")
    println("ROW="+text)
}
private fun device(root:File,worker:Worker,bridge:FileBridge=mock()):MacosDevice {
    val vendor=VendorConfiguration.MacosConfiguration()
    val config=Configuration.Builder("test",root).apply { vendorConfiguration=vendor }.build()
    return MacosDevice("udid",Transport.Local,Sdk.MACOS,AppleBinaryEnvironment(worker,config,vendor.timeoutConfiguration,Gson()),AppleTestBundleIdentifier(),FileManager(0,0,root),config,vendor,worker,bridge,SharedTransfers(),Track(),SystemTimer(Clock.systemUTC())).also {
        MacosDevice::class.java.getDeclaredField("testBundle").apply { isAccessible=true }.set(it,mock<AppleTestBundle>{on {testBundleId} doReturn "Tests"})
    }
}
fun supplemental(root:File)=runBlocking {
    for(mode in listOf("permissions","missing-user","os10","unknown","sip-full","sip-fail","sql-first","sql-second","bad-client")) {
        val w=Worker(mode);val d=device(root,w)
        val outcome=runCatching {
            d.setup()
            if(d.binaryEnvironment.csrutil.status()) throw com.malinskiy.marathon.exceptions.IncompatibleDeviceException("Unable to add permissions: requires System Integrity Protection to be disabled")
            d.grant(if(mode=="bad-client") "com.example.\"broken" else "com.example.App",linkedSetOf(Permission.Accessibility,Permission.DeveloperTool))
        }.exceptionOrNull()
        val env=MacosDevice::class.java.getDeclaredField("env").apply {isAccessible=true}.get(d)
        row(root,linkedMapOf("case" to mode,"requests" to w.requests,"error" to error(outcome),"facts" to linkedMapOf("os" to d.operatingSystem.version,"model" to d.model,"manufacturer" to d.manufacturer,"abi" to d.abi,"serial" to d.serialNumber,"env" to env)))
        d.dispose()
    }
    val host="workers:\n- transport: {type: local}\n  devices: [{type: host}]\n"
    for((mode,input) in listOf("empty" to "workers: []\n","mapping" to "workers: null","syntax" to "workers: [","root-syntax" to "@","null-document" to "null","missing-file" to null,"directory-read" to null,"permission-denied-read" to "workers: []","non-host" to host.replace("type: host","type: simulator, udid: ignored"),"overwrite" to (host+"- transport: {type: local}\n  devices: [{type: simulator, udid: ignored}]\n"),"connect-null" to host,"setup-failure" to host,"fallback" to host,"replace" to host,"substitution" to "workers: \${LOOP}")) {
        val dir=File(root,mode).apply {mkdirs()};val file=File(dir,"Marathondevices");input?.let {file.writeText(it)}
        if(mode=="directory-read") check(file.mkdir())
        if(mode=="permission-denied-read") {
            java.nio.file.Files.setPosixFilePermissions(file.toPath(), emptySet())
            check(!file.canRead()) { "Permission oracle requires an unprivileged user" }
        }
        val vendor=VendorConfiguration.MacosConfiguration(devicesFile=file)
        val config=Configuration.Builder("test",dir).apply {vendorConfiguration=vendor}.build()
        val p=AppleMacosProvider(config,vendor,AppleTestBundleIdentifier(),Gson(),ObjectMapper(YAMLFactory()).registerModule(KotlinModule.Builder().configure(com.fasterxml.jackson.module.kotlin.KotlinFeature.SingletonSupport,true).build()),Track(),SystemTimer(Clock.systemUTC()))
        val w=Worker(mode);val factory=mock<ConnectionFactory>();val bridge=mock<FileBridge>()
        whenever(factory.create(any())).thenReturn(Pair(if(mode=="connect-null") null else w,bridge))
        AppleMacosProvider::class.java.getDeclaredField("connectionFactory").apply {isAccessible=true}.set(p,factory)
        if(mode=="substitution") AppleMacosProvider::class.java.getDeclaredField("environmentVariableSubstitutor").apply {isAccessible=true}.set(p,org.apache.commons.text.StringSubstitutor(mapOf("LOOP" to "\${LOOP}")))
        val logger=LoggerFactory.getLogger("AppleMacosProvider") as Logger
        val logs=ListAppender<ILoggingEvent>().apply {start()};logger.addAppender(logs)
        val outcome=runCatching {p.initialize();if(mode=="replace")p.initialize()}.exceptionOrNull()
        val devices=AppleMacosProvider::class.java.getDeclaredField("devices").apply {isAccessible=true}.get(p) as Map<*,*>
        val serials=devices.keys.toList()
        delay(50)
        p.terminate()
        val events=mutableListOf<String>();while(true) { val e=p.subscribe().tryReceive().getOrNull() ?: break;events+=e.javaClass.simpleName }
        row(root,linkedMapOf("case" to mode,"error" to error(outcome),"requests" to w.requests,"serials" to serials,"events" to events,"creates" to mockingDetails(factory).invocations.count {it.method.name=="create"},"releases" to mockingDetails(factory).invocations.count {it.method.name=="dispose"},"logs" to logs.list.map {linkedMapOf("logger" to it.loggerName,"level" to it.level.toString(),"message" to it.formattedMessage)}))
        logger.detachAppender(logs)
    }
    run {
        val dir=File(root,"inline").apply {mkdirs()};val events=mutableListOf<String>()
        val result=CompletableDeferred<com.malinskiy.marathon.execution.TestBatchResults>()
        val bridge=mock<FileBridge>()
        whenever(bridge.receive(any(),any())).thenAnswer { invocation ->
            events += "pull:result-completed="+result.isCompleted
            val src=invocation.getArgument<String>(0);val dst=invocation.getArgument<File>(1)
            File(dst,File(src).name).mkdirs();true
        }
        val w=Worker("inline");val d=device(dir,w,bridge);d.setup()
        val config=Configuration.Builder("test",dir).apply {vendorConfiguration=VendorConfiguration.MacosConfiguration()}.build()
        val outcome=runCatching {withTimeout(5000) {d.execute(config,DevicePoolId("pool"),TestBatch(emptyList(),"batch"),result)}}.exceptionOrNull()
        events += "execute-returned"
        row(root,linkedMapOf("case" to "inline-xcresult","error" to error(outcome),"events" to events,"requests" to w.requests,"local" to dir.walkTopDown().filter {it.name=="batch.xcresult"}.map {it.relativeTo(dir).path}.toList()))
        d.dispose()
    }
    run {
        val w=Worker("dispose-execute");val d=device(root,w)
        val config=Configuration.Builder("test",root).apply {vendorConfiguration=VendorConfiguration.MacosConfiguration()}.build()
        val task=async {runCatching {d.execute(config,DevicePoolId("pool"),TestBatch(emptyList(),"dispose"),CompletableDeferred())}.exceptionOrNull()}
        withTimeout(5000) {w.started.await()};d.dispose();w.events += "disposed"
        val outcome=task.await()
        row(root,linkedMapOf("case" to "dispose-execute","error" to error(outcome),"events" to w.events))
    }
    val failures=listOf<Throwable>(IllegalStateException("state"),java.io.IOException("io"),net.schmizz.sshj.connection.ConnectionException("connection"),net.schmizz.sshj.transport.TransportException("transport"),net.schmizz.sshj.connection.channel.OpenFailException("session", 1, "open"),DeviceFailureException(DeviceFailureReason.IncompatibleDevice),DeviceFailureException(DeviceFailureReason.FailedRunner))
    for(f in failures) {
        val w=Worker("execute");w.fail=f;val d=device(root,w)
        val config=Configuration.Builder("test",root).apply {vendorConfiguration=VendorConfiguration.MacosConfiguration()}.build()
        val outcome=runCatching {withTimeout(5000) {d.execute(config,DevicePoolId("pool"),TestBatch(emptyList()),CompletableDeferred())}}.exceptionOrNull()
        row(root,linkedMapOf("case" to "execute-"+f.javaClass.simpleName+(if(f is DeviceFailureException) "-"+f.reason else ""),"error" to error(outcome)))
        d.dispose()
    }
}
