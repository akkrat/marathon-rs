import ch.qos.logback.classic.Level
import ch.qos.logback.classic.LoggerContext
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import com.google.gson.GsonBuilder
import com.fasterxml.jackson.databind.node.ObjectNode
import com.malinskiy.marathon.apple.AppleDevice
import com.malinskiy.marathon.apple.RemoteFileManager
import com.malinskiy.marathon.apple.listener.*
import com.malinskiy.marathon.apple.logparser.*
import com.malinskiy.marathon.apple.logparser.parser.*
import com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.Xcresulttool
import com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.Xcresulttool.Companion.AppleJsonMapper
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.config.vendor.apple.TimeoutConfiguration
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.test.*
import com.malinskiy.marathon.time.Timer
import com.malinskiy.marathon.vendor.ios.xcrun.xcresulttool.*
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import org.slf4j.LoggerFactory
import java.io.File
import java.lang.reflect.Proxy
import java.nio.charset.Charset
import java.time.Duration

val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
val events = ListAppender<ILoggingEvent>()
val timer = object : Timer {
    override val startTimeMillis = 1537187696000L
    override val elapsedTimeMillis = 0L
    override fun currentTimeMillis() = startTimeMillis
    override fun measure(block: () -> Unit): Long {block(); return 0}
}
fun emit(name:String,kind:String,vararg pairs:Pair<String,Any?>) {
    println(gson.toJson(linkedMapOf("case" to name,"kind" to kind,*pairs)))
}
fun logs() = events.list.map { linkedMapOf("logger" to it.loggerName,"level" to it.level.toString(),"message" to it.formattedMessage) }
fun stream(name:String,lines:List<String>,target:String="",producer:Boolean=false) {
    val p = if (producer) XctestEventProducer(target,timer) else TestRunProgressParser(timer,target)
    events.list.clear()
    val result=lines.map {line ->
        try {linkedMapOf<String,Any?>("line" to line,"events" to p.process(line)?.map {it.toString()},"exception" to null)}
        catch (e:Exception) {linkedMapOf("line" to line,"events" to null,"exception" to listOf(e.javaClass.name,e.message))}
    }
    emit(name,"stream","target" to target,"producer" to producer,"rows" to result,"logs" to logs())
}
class Session(val result:CommandResult):CommandSession {
    override val stdout = Channel<String>(Channel.UNLIMITED).apply {result.stdout.forEach {trySend(it)};close()}
    override val stderr = Channel<String>(Channel.UNLIMITED).apply {result.stderr.forEach {trySend(it)};close()}
    override val exitCode = CompletableDeferred(result.exitCode)
    override val isAlive = false
    override suspend fun drain() {}
    override fun terminate() {}
    override fun kill() {}
    override suspend fun await() = result
    override fun close() {}
}
class Recording(val root:String=""):CommandExecutor {
    override val host:CommandHost = KotlinProcessHost("local",Charsets.UTF_8)
    override val logger=mu.KotlinLogging.logger("Recording")
    override val connected=true
    override suspend fun execute(command:List<String>,timeout:Duration,idleTimeout:Duration,env:Map<String,String>,workdir:String?,charset:Charset):CommandSession = Session(CommandResult(root.lines(),emptyList(),0))
    override fun close() {}
}
fun device():AppleDevice = Proxy.newProxyInstance(AppleDevice::class.java.classLoader,arrayOf(AppleDevice::class.java)) {_,method,_ ->
    when(method.name) {
        "getSerialNumber" -> "serial"
        "getOperatingSystem" -> OperatingSystem("27")
        "getModel" -> "Mac"
        "getManufacturer" -> "Apple"
        "getNetworkState" -> NetworkState.CONNECTED
        "getDeviceFeatures" -> emptyList<DeviceFeature>()
        "getHealthy" -> true
        "getStoragePath" -> "/tmp"
        "getLogger" -> mu.KotlinLogging.logger("device")
        "getAbi" -> "arm64"
        else -> null
    }
} as AppleDevice
fun project(r:TestResult) = linkedMapOf("test" to r.test.toString(),"status" to r.status.name,"start" to r.startTime,"end" to r.endTime,"trace" to r.stacktrace)
fun main(args:Array<String>) = runBlocking {
    val ctx=LoggerFactory.getILoggerFactory() as LoggerContext
    ctx.getLogger("ROOT").level=Level.TRACE
    events.context=ctx;events.start();ctx.getLogger("ROOT").addAppender(events)
    val fixtures=File(args[0]);val base=File(fixtures,"vendor-apple-base/fixtures/test_output")
    base.listFiles()!!.filter {it.name.endsWith(".log.input")}.sortedBy {it.name}.forEach {f ->
        stream("fixture/${f.name}",f.readLines())
    }
    stream("fixture/success_multiple_0/target",File(base,"success_multiple_0.log.input").readLines(),"testTarget")
    stream("names",listOf("Test Case '-[a.b.C m]' started.","Test Case '-[C m]' started.","Test Case '-[C m]' passed (0.0005 seconds)."),"target")
    stream("real/xcode27-timeout",File(fixtures,"apple-logparser/xcode27-timeout.log.input").readLines())
    stream("no-target",listOf("Test Case '-[C m]' started."))
    stream("traces",listOf("Test Case '-[p.C m]' started.","/a:1: error: -[p.C m] : first","/b:2: error: -[p.C m] : last","Test Case '-[p.C m]' failed (1.000 seconds).","Test Case '-[p.C n]' started.","Test Case '-[p.C n]' failed (1.000 seconds)."))
    stream("terminators",listOf("Test Case '-[p.C m]' started.\r","Test Case '-[p.C m]' started.\u0085","Test Case '-[p.C m]' passed (١.٠ seconds).","Test Case '-[p.C m]' passed (1.2.3 seconds)."))
    for (allowance in listOf("1 minute","2 minutes","1 hour","1.5 seconds","1 minute, 30 seconds"," 3 minutes")) stream("duration/$allowance",listOf("Test Case '-[p.C m]' exceeded execution time allowance of $allowance. The test may have hung"))
    val literals=listOf("Failed to install or launch the test runner","Software caused connection abort","Unable to find a destination matching the provided destination specifier","Terminating since there is no system app","Exiting because the workspace server has disconnected","Not authorized for performing UI testing PropertyActions","Timed out waiting for automation session","Failed to terminate","Failed to launch app with identifier","Test runner exited before starting test execution","Early unexpected exit, operation never finished bootstrapping","Connection peer refused channel request","CoreSimulatorService connection became invalid","CoreSimulatorService connection interrupted","Simulator services will no longer be available","Unable to locate device set","Test runner exited")
    stream("failure-literals",literals+ literals.flatMapIndexed {i,a -> literals.drop(i+1).map {b -> "$b / $a"}} + listOf("Test Case '-[p.C Test runner exited]' started."),producer=true)
    val dev=device()
    for (runEvents in listOf(false,true)) {
        val passed=Test("p","C","passed",emptyList());val failed=Test("p","C","failed",emptyList());val ignored=Test("p","C","ignored",emptyList());val crashed=Test("p","C","crashed",emptyList());val missing=Test("p","C","missing",emptyList());val extra=Test("p","C","extra",emptyList())
        val batch=TestBatch(listOf(passed.copy(metaProperties=listOf(MetaProperty("annotation"))),failed,ignored,crashed,missing),"batch")
        val deferred=CompletableDeferred<TestBatchResults>()
        val listener=TestResultsListener(batch,dev,DevicePoolId("pool"),deferred,timer,RemoteFileManager(dev),Xcresulttool(Recording(),TimeoutConfiguration()),null,emptyList())
        events.list.clear()
        if(runEvents) listener.testRunStarted()
        listener.testStarted(passed);listener.testPassed(passed,10,20)
        listener.testStarted(failed);listener.testFailed(failed,30,40,"stdout")
        listener.testStarted(ignored);listener.testIgnored(ignored,50,60)
        listener.testStarted(crashed);listener.testPassed(extra,70,80)
        if(runEvents) listener.testRunEnded()
        listener.afterTestRun()
        val result=deferred.await()
        emit("listener/$runEvents","projection","runEvents" to runEvents,"finished" to result.finished.map(::project),"failed" to result.failed.map(::project),"uncompleted" to result.uncompleted.map(::project),"logs" to logs())
    }
    for (n in 1..5) {
        events.list.clear();val finder=SessionResultsPathFinder()
        for (i in 1..n) {finder.onLine("    /tmp/path$i.xcresult");finder.onLine("    /tmp/path$i.xcresult")}
        finder.afterTestRun();emit("artifact/$n","artifact","paths" to (1..n).map {"/tmp/path$it.xcresult"},"logs" to logs())
    }
    events.list.clear();val debug=DebugTestRunListener(dev);val t=Test("p","C","m",emptyList())
    debug.beforeTestRun();debug.testRunStarted();debug.testStarted(t);debug.testStarted(t.copy(metaProperties=listOf(MetaProperty("tag",mapOf("value" to "v")))));debug.testFailed(t,10,20,null);debug.testFailed(t,10,20,"first\nsecond");debug.testPassed(t,10,20);debug.testIgnored(t,10,20);debug.testRunFailed("error",DeviceFailureReason.ServicesUnavailable);debug.testRunEnded();debug.afterTestRun()
    emit("debug","debug","logs" to logs())
    // A nullable candidate must first survive the actual mapper's constructor defaults.
    for (shape in listOf("missing","null","wrapped-null")) {
        val node=AppleJsonMapper.readTree(File(fixtures,"vendor-apple-base/fixtures/xctesttool/sample_1.json").readText()) as ObjectNode
        val summary=node.at("/issues/testFailureSummaries/_values/0") as ObjectNode
        when(shape) {"missing"->summary.remove("message");"null"->summary.putNull("message");else->(summary["message"] as ObjectNode).putNull("_value")}
        try {val record=AppleJsonMapper.readValue(node.toString(),ActionsInvocationRecord::class.java);emit("message/$shape","message","message" to record.issues.testFailureSummaries!!.first().message)}
        catch(e:Exception) {emit("message/$shape","message","exception" to e.javaClass.name)}
    }
}
