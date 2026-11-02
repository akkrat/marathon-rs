// WP 3.5: real e47352f2 accumulators; no source replacements or copied algorithms.
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.analytics.internal.pub.Tracker
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.CIConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionMode
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.execution.progress.PoolProgressAccumulator
import com.malinskiy.marathon.execution.result.TestRunResultsAccumulator
import com.malinskiy.marathon.execution.queue.TestState
import com.malinskiy.marathon.integrations.ci.CIIntegrationFactory
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.time.Timer
import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.AppenderBase
import org.slf4j.LoggerFactory
import java.io.*
import java.time.Instant
import kotlin.math.roundToInt

val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
val test = Test("com.example", "Class", "method", listOf(MetaProperty("original", emptyMap())))
val device = DeviceInfo(OperatingSystem("1"), "1", "model", "manufacturer", NetworkState.CONNECTED, emptyList(), true)
val resultDevice = device.copy(serialNumber = "result")
val logs = mutableListOf<Map<String,Any?>>()
val effects = mutableListOf<Map<String,Any?>>()
class Diagnostics : AppenderBase<ILoggingEvent>() {
    override fun append(event: ILoggingEvent) { val row = mapOf("logger" to event.loggerName, "level" to event.level.toString(), "message" to event.formattedMessage); logs += row; effects += mapOf("log" to row) }
}
fun setupLogs() {
    val root = LoggerFactory.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME) as Logger
    root.detachAndStopAllAppenders()
    root.addAppender(Diagnostics().apply {context = root.loggerContext;start()})
}
class Capture(val fail:Boolean=false) : Tracker {
    val records = mutableListOf<Map<String,Any?>>()
    override fun deviceConnected(p:DevicePoolId,d:DeviceInfo) {}
    override fun deviceDisconnected(p:DevicePoolId,d:DeviceInfo) {}
    override fun deviceProviderInit(s:String,a:Instant,b:Instant) {}
    override fun devicePreparing(s:String,a:Instant,b:Instant) {}
    override fun test(p:DevicePoolId,d:DeviceInfo,r:TestResult,final:Boolean) {
        val row = mapOf("pool" to p.name,"device" to d.serialNumber,"resultDevice" to r.device.serialNumber,"test" to r.test.method,"metadata" to r.test.metaProperties.map {it.name}, "status" to r.status.name,"batch" to r.testBatchId,"start" to r.startTime,"end" to r.endTime,"final" to final)
        records += row; effects += mapOf("track" to row)
        if (fail) throw IllegalStateException("tracker failed")
    }
}
fun config(mode:ExecutionMode,fast:Boolean,ci:CIConfiguration=CIConfiguration.None) = Configuration.Builder("",File("")).apply {
    vendorConfiguration=VendorConfiguration.StubVendorConfiguration;debug=false;analyticsTracking=false
    executionStrategy=ExecutionStrategyConfiguration(mode,fast);ciConfiguration=ci
}.build()
fun field(o:Any,name:String):Any = o.javaClass.getDeclaredField(name).apply {isAccessible=true}.get(o)
fun machine(p:PoolProgressAccumulator):Any = (field(p,"tests") as Map<*,*>).values.first()!!.let {field(it!!,"state")}
fun state(p:PoolProgressAccumulator):Map<String,Any?> {
    val s=machine(p).javaClass.getMethod("getState").invoke(machine(p))
    fun n(name:String):Int = s.javaClass.getMethod(name).invoke(s) as Int
    return mapOf("kind" to s.javaClass.simpleName,"total" to n("getTotal"),"done" to if(s is TestState.Added)0 else n("getDone"),"running" to n("getRunning"))
}
fun seed(p:PoolProgressAccumulator,s:TestState) {machine(p).javaClass.getDeclaredField("stateRef").apply {isAccessible=true}.set(machine(p),s)}
fun initial(kind:String):TestState = when(kind) {
    "Added" -> TestState.Added(3,0)
    "Passing" -> TestState.Passing(3,-1,1)
    "Failing" -> TestState.Failing(3,-1,1)
    "Passed" -> TestState.Passed(3,-1,1)
    else -> TestState.Failed(3,-1,1)
}
fun poolRow(mode:ExecutionMode,fast:Boolean,kind:String,event:String,fail:Boolean=false,unknown:Boolean=false,seedState:TestState?=null):Map<String,Any?> {
    logs.clear();effects.clear();val capture=Capture(fail);val p=PoolProgressAccumulator(DevicePoolId("test"),TestShard(listOf(test,test,test)),config(mode,fast),Track().add(capture));seed(p,seedState?:initial(kind));val before=state(p)
    val out=ByteArrayOutputStream();val original=System.out;System.setOut(PrintStream(out,true,"UTF-8"))
    val callback=test.copy(method=if(unknown)"unknown" else "method",metaProperties=emptyList())
    var action:String?=null;var error:String?=null;var returned=false
    try {
        val result=TestResult(callback,resultDevice,"batch",when(event) {"failed"->TestStatus.FAILURE;"ignored"->TestStatus.IGNORED;"assumption"->TestStatus.ASSUMPTION_FAILURE;"incomplete","finalIncomplete"->TestStatus.INCOMPLETE;else->TestStatus.PASSED},10,20)
        action=when(event) {
            "started"->{p.testStarted(device,callback);null}
            "retry"->p.retryTest(callback)?.javaClass?.simpleName
            "remove"->p.removeTest(callback,1)?.javaClass?.simpleName
            "removeNegative"->p.removeTest(callback,-1)?.javaClass?.simpleName
            "removeMax"->p.removeTest(callback,Int.MAX_VALUE)?.javaClass?.simpleName
            else->p.testEnded(device,result,event=="finalIncomplete")?.javaClass?.simpleName
        };returned=true
    } catch(e:Exception) {error=e.javaClass.name+": "+e.message} finally {System.setOut(original)}
    val after=state(p);val aggregate=p.aggregateResult()
    return mapOf("mode" to mode.name,"fast" to fast,"state" to kind,"event" to event,"failTracker" to fail,"unknown" to unknown,"before" to before,"after" to after,"action" to action,"returned" to returned,"error" to error,"progressBits" to p.progress().toRawBits(),"aggregate" to aggregate,"stdout" to out.toString("UTF-8"),"logs" to logs.toList(),"tracks" to capture.records,"effects" to effects.toList())
}
class Steps(val values:ArrayDeque<Long>):Timer {
    val reads=mutableListOf<Long>()
    override val startTimeMillis=0L;override val elapsedTimeMillis get()=currentTimeMillis()
    override fun currentTimeMillis():Long {val n=values.removeFirst();reads+=n;return n}
    override fun measure(block:()->Unit):Long {val a=currentTimeMillis();block();return currentTimeMillis()-a}
}
fun runRows():List<Map<String,Any?>> {
    val timer=Steps(ArrayDeque((100L..140L).toList()));val a=TestRunResultsAccumulator(timer);val rows=mutableListOf<Map<String,Any?>>()
    fun snap(op:String) {rows+=mapOf("op" to op,"records" to a.temporalTestResults.map {(t,r)->mapOf("test" to t.method,"metadata" to t.metaProperties.map{it.name},"status" to r.status.name,"start" to r.startTime,"end" to r.endTime,"trace" to r.stackTrace,"metrics" to r.metrics,"successful" to r.isSuccessful())},"name" to a.name,"complete" to a.isRunComplete,"failure" to a.runFailureMessage,"elapsed" to a.elapsedTime,"metrics" to a.runMetrics.toMap(),"completed" to a.completedTests.map {it.method},"failed" to a.numAllFailedTests,"counts" to TestStatus.values().map {a.getNumTestsInState(it)},"reads" to timer.reads.toList())}
    snap("new");a.testRunStarted("first",9);snap("runStarted")
    a.testEnded(test,mapOf("metric" to "v"));snap("missingEnded")
    a.testEnded(test,emptyMap(),startTime=999);snap("existingEndedIgnoresStart")
    a.testFailed(test,"trace",11,12);a.testFailed(test,"\u2003",null,null);snap("blankTrace")
    a.testStarted(test.copy(metaProperties=emptyList()),20);snap("replace")
    a.testAssumptionFailure(test,"assumed");snap("assumption")
    a.testStarted(test.copy(method="b"),30);a.testRunFailed("crash");snap("runFailed")
    a.testRunEnded(Long.MAX_VALUE,mapOf("old" to "v"));snap("runEnded")
    a.testRunStarted("second",0);snap("restart")
    a.testRunStopped(1);snap("stopped")
    val batch=TestBatch(listOf(test),"batch")
    val missing=a.createUncompletedTestResults(listOf(test.copy(method="c"),test.copy(method="d")),batch,device)
    rows+=mapOf("op" to "synthesis","results" to missing.map {mapOf("test" to it.test.method,"start" to it.startTime,"end" to it.endTime,"trace" to it.stacktrace,"batch" to it.testBatchId)},"reads" to timer.reads.toList())
    return rows
}
fun main(args:Array<String>) {
    setupLogs()
    if(args.firstOrNull()=="ci") {val ci=CIIntegrationFactory.get(config(ExecutionMode.ANY_SUCCESS,false,CIConfiguration.Auto));println(ci.javaClass.simpleName);ci.setBuildProgress(-1);return}
    val dir=File(args[0]).apply{mkdirs()}
    val pool=mutableListOf<Map<String,Any?>>()
    for(mode in ExecutionMode.values())for(fast in listOf(false,true))for(kind in listOf("Added","Passing","Failing","Passed","Failed"))for(event in listOf("started","passed","failed","ignored","assumption","incomplete","finalIncomplete","retry","remove","removeNegative","removeMax"))pool+=poolRow(mode,fast,kind,event)
    for(event in listOf("started","passed","failed","retry","removeMax"))pool+=poolRow(ExecutionMode.ALL_SUCCESS,false,"Passing",event,seedState=TestState.Passing(Int.MAX_VALUE,Int.MIN_VALUE,Int.MAX_VALUE))
    pool+=poolRow(ExecutionMode.ANY_SUCCESS,false,"Added","passed",fail=true)
    pool+=poolRow(ExecutionMode.ANY_SUCCESS,false,"Added","passed",fail=true,seedState=TestState.Added(1,0))
    for(event in listOf("started","passed","retry","remove"))pool+=poolRow(ExecutionMode.ANY_SUCCESS,false,"Added",event,unknown=true)
    // Non-fast done+1==total boundary: the next finished run completes the test.
    for(mode in ExecutionMode.values())for(seedState in listOf(TestState.Passing(2,0,1),TestState.Failing(2,0,1)))for(event in listOf("started","passed","failed","ignored","assumption","incomplete","finalIncomplete","retry","remove","removeNegative","removeMax"))pool+=poolRow(mode,false,seedState.javaClass.simpleName,event,seedState=seedState)
    File(dir,"pool.jsonl").writeText(pool.joinToString("\n",postfix="\n"){gson.toJson(it)})
    File(dir,"run.jsonl").writeText(runRows().joinToString("\n",postfix="\n"){gson.toJson(it)})
    val empty=PoolProgressAccumulator(DevicePoolId("test"),TestShard(emptyList()),config(ExecutionMode.ANY_SUCCESS,false),Track())
    val error=try{empty.aggregateResult();"none"}catch(e:Exception){e.javaClass.name+": "+e.message}
    val round=listOf(Float.NaN,Float.POSITIVE_INFINITY,Float.NEGATIVE_INFINITY,0.005f,0.0049999994f,-0.005f,0.165f,0.995f,167772.16f).map {v-> mapOf("bits" to v.toRawBits(),"rounded" to try{(v*100f).roundToInt().toString()}catch(e:Exception){e.javaClass.name+": "+e.message})}
    File(dir,"edges.jsonl").writeText(gson.toJson(mapOf("emptyAggregate" to error,"emptyProgressBits" to empty.progress().toRawBits(),"round" to round))+"\n")
}
