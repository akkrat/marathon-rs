import com.google.gson.Gson
import com.malinskiy.marathon.analytics.internal.sub.*
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.analytics.internal.pub.Tracker
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.device.file.*
import ch.qos.logback.classic.Logger
import ch.qos.logback.core.read.ListAppender
import org.slf4j.LoggerFactory
import java.lang.reflect.Proxy
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.execution.listener.*
import com.malinskiy.marathon.report.Reporter
import com.malinskiy.marathon.report.attachment.*
import com.malinskiy.marathon.report.logs.*
import com.malinskiy.marathon.io.*
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.toTestName
import kotlinx.coroutines.runBlocking
import java.io.File
import java.time.Instant

val gson = Gson()
val rows = mutableListOf<Map<String,Any?>>()
fun row(id: String, vararg values: Pair<String,Any?>) { rows.add(linkedMapOf("id" to id, *values)) }
fun device(model: String = "first") = DeviceInfo(OperatingSystem("23"),"serial",model,"unknown",NetworkState.CONNECTED,emptyList(),true)
fun result(method: String = "test", status: TestStatus = TestStatus.PASSED, start: Long = 1, end: Long = 100, test: Test = Test("com","example",method,emptyList())) = TestResult(test,device(),"batch",status,start,end)
fun event(r: TestResult, final: Boolean = true, time: Long = 0, pool: String = "pool") = TestEvent(Instant.ofEpochMilli(time),DevicePoolId(pool),device(),r,final)
fun report(events: List<TestEvent>, connected: List<DeviceConnectedEvent> = listOf(DeviceConnectedEvent(Instant.EPOCH,DevicePoolId("pool"),device()))) = ExecutionReport(connected,emptyList(),emptyList(),emptyList(),events)
fun capture(block: ()->Unit): String? = try { block(); null } catch(e: Exception) { e.javaClass.name }

fun main(args: Array<String>) = runBlocking {
    val out = File(args[0]); val tmp = File(args[1])
    val summary = report(listOf(event(result("orphan"),pool="test-only"),event(result("x",TestStatus.INCOMPLETE)),event(result("x",TestStatus.PASSED)),event(result("x",TestStatus.FAILURE)),event(result("a",TestStatus.ASSUMPTION_FAILURE)),event(result("zero",start=0,end=100)),event(result("negative",start=100,end=1)),event(result("overflow",start=Long.MIN_VALUE,end=Long.MAX_VALUE)))).summary
    val p = summary.pools.single()
    row("summary", "pools" to summary.pools.map{it.poolId.name}, "tests" to p.tests.map{it.test.method}, "passed" to p.passed, "failed" to p.failed,"ignored" to p.ignored,"duration" to p.durationMillis,"rawDuration" to p.rawDurationMillis,"rawIncomplete" to p.rawIncomplete,"flaky" to p.flaky)
    val first = report(emptyList(),listOf(DeviceConnectedEvent(Instant.EPOCH,DevicePoolId("b"),device()),DeviceConnectedEvent(Instant.EPOCH,DevicePoolId("a"),device()),DeviceConnectedEvent(Instant.EPOCH,DevicePoolId("b"),device("last")))).summary
    row("connections", "pools" to first.pools.map{it.poolId.name}, "models" to first.pools.flatMap{it.devices}.map{it.model})
    val same = result(); val equal = same.copy()
    for ((id, events) in listOf("same-object" to listOf(event(same),event(same)),"equal-distinct" to listOf(event(same),event(equal)))) {
        val s = report(events).summary.pools.single()
        row(id,"tests" to s.tests.size,"keys" to s.retries.size,"retryIndexes" to s.retries.values.map { list -> list.map { e -> events.indexOfFirst{ it === e } } })
    }
    val calls = mutableListOf<String>()
    val reporter1 = object: Reporter { override fun generate(report: ExecutionReport) { calls.add(report.testEvents.joinToString(","){it.testResult.test.method}); } }
    val generator = ExecutionReportGenerator(listOf(reporter1))
    listOf(event(result("late"),time=2),event(result("early"),time=1),event(result("equal"),time=1)).forEach{generator.track(it)}
    generator.close();generator.close(); row("sort-repeat-close","calls" to calls.toList())
    calls.clear()
    val bad = object: Reporter { override fun generate(report: ExecutionReport) { calls.add("bad"); throw IllegalStateException("report failed") } }
    val never = object: Reporter { override fun generate(report: ExecutionReport) { calls.add("never") } }
    val fail = ExecutionReportGenerator(listOf(bad,never))
    row("reporter-failure","error" to capture{fail.close()},"calls" to calls.toList())
    calls.clear()
    val internal = DelegatingTrackerInternal(listOf(object: TrackerInternalAdapter(){ override fun trackTest(event: TestEvent) { calls.add("a") }; override fun close(){ calls.add("close-a");throw IllegalStateException("close") } },object:TrackerInternalAdapter(){override fun trackTest(event:TestEvent){ calls.add("b") };override fun close(){calls.add("close-b")} }))
    internal.track(event(result())); val closeError=capture{internal.close()}; row("internal-order","calls" to calls.toList(),"error" to closeError)
    calls.clear()
    val track = Track()
    fun tracker(name:String, fail:Boolean=false):Tracker = object:Tracker {
        override fun deviceConnected(poolId:DevicePoolId, device:DeviceInfo){}; override fun deviceDisconnected(poolId:DevicePoolId, device:DeviceInfo){}
        override fun deviceProviderInit(serialNumber:String,startTime:Instant,finishTime:Instant){};override fun devicePreparing(serialNumber:String,startTime:Instant,finishTime:Instant){}
        override fun test(poolId:DevicePoolId,device:DeviceInfo,testResult:TestResult,final:Boolean){ calls.add(name); if(fail) throw IllegalArgumentException("track") }
    }
    val a=tracker("a"); track.add(a).add(a).add(tracker("bad",true)).add(tracker("never"))
    val trackError=capture{track.test(DevicePoolId("pool"),device(),result(),true)};row("fanout","calls" to calls.toList(),"error" to trackError)
    val provider=AttachmentProviderDelegate(); val collector=AttachmentCollector(listOf(provider,provider)); val test=Test("com","example","test",emptyList()); val attachment=Attachment(File(tmp,"missing"),AttachmentType.LOG,"log")
    provider.onAttachment(test,attachment);provider.onAttachment(test.copy(metaProperties=listOf(com.malinskiy.marathon.test.MetaProperty("annotation",emptyMap()))),attachment)
    row("attachments","count" to collector[test]?.size,"unknown" to collector[Test("","","unknown",emptyList())]?.size)
    val producer=object:LogProducer {val listeners=mutableListOf<LineListener>(); override fun addLineListener(lineListener:LineListener){listeners.add(lineListener)};override fun removeLineListener(lineListener:LineListener){listeners.remove(lineListener)} }
    val files=FileManager(0,0,File(tmp,"logs-out"));val listener=LogListener(device(),producer,DevicePoolId("pool"),"batch",LogWriter(files));val logs=AttachmentCollector(listOf(listener))
    listener.beforeTestRun();listener.onLine("preamble");listener.testStarted(test);listener.onLine("one");listener.onLine("");listener.testEnded(test);listener.onLine("tail");listener.afterTestRun()
    row("logs","testBytes" to logs[test]!!.single().file.readBytes().map{it.toInt() and 255},"batchBytes" to files.createFile(FileType.LOG,DevicePoolId("pool"),device(),testBatchId="batch").readBytes().map{it.toInt() and 255},"registered" to producer.listeners.size)
    val failed=LogListener(device(),producer,DevicePoolId("pool"),"bad",LogWriter(FileManager(0,0,File(tmp,"not-dir").apply{writeText("x")})))
    failed.testStarted(test);failed.onLine("discarded"); var saveError:String?=null;var saveMessage:String?=null;try{failed.testEnded(test)}catch(e:Exception){saveError=e.javaClass.name;saveMessage=e.message?.replace(tmp.absolutePath,"<tmp>")}; failed.testEnded(test);row("log-reset-failure","error" to saveError,"message" to saveMessage,"secondEmptySuccess" to true)
    val writer=LogWriter(files);val badText="a\uD800b";val f=writer.saveLogs(test,DevicePoolId("pool"),"utf",device(),listOf(badText,""));writer.appendLogs(test,DevicePoolId("pool"),device(),badText)
    row("log-utf16","saveBytes" to f.readBytes().map{it.toInt() and 255},"appendBytes" to files.createFile(FileType.LOG,DevicePoolId("pool"),device(),test).readBytes().map{it.toInt() and 255})
    val empty=File(tmp,"empty").apply{writeText("")};val nonempty=File(tmp,"nonempty").apply{writeText("x")};val directory=File(tmp,"directory").apply{mkdirs()}
    row("empty","missing" to attachment.empty,"empty" to Attachment(empty,AttachmentType.LOG).empty,"nonempty" to Attachment(nonempty,AttachmentType.LOG).empty,"directoryLength" to directory.length(),"directoryEmpty" to Attachment(directory,AttachmentType.LOG).empty)
    val sumOverflow = report(listOf(event(result("one",start=1,end=Long.MAX_VALUE)),event(result("two",start=1,end=Long.MAX_VALUE)))).summary.pools.single()
    row("sum-overflow", "duration" to sumOverflow.durationMillis, "rawDuration" to sumOverflow.rawDurationMillis)
    calls.clear()
    val snapshotTrack=Track()
    val added=tracker("added")
    val registering=object:Tracker by tracker("unused") { override fun test(poolId:DevicePoolId,device:DeviceInfo,testResult:TestResult,final:Boolean){calls.add("register");snapshotTrack.add(added)} }
    snapshotTrack.add(registering);snapshotTrack.test(DevicePoolId("pool"),device(),result(),true);val firstCalls=calls.toList();snapshotTrack.test(DevicePoolId("pool"),device(),result(),true)
    row("track-snapshot","first" to firstCalls,"second" to calls.toList())
    for(before in listOf(true,false)) {
        val partition=LogListener(device(),producer,DevicePoolId("pool"),"partition-$before",LogWriter(files)); val attached=AttachmentCollector(listOf(partition))
        partition.beforeTestRun();partition.testStarted(test);if(before)partition.onLine("tail");partition.testEnded(test);if(!before)partition.onLine("tail");partition.close();partition.afterTestRun()
        row("tail-$before","testBytes" to attached[test]?.single()?.file?.readBytes()?.map{it.toInt() and 255},"batchExists" to files.createFile(FileType.LOG,DevicePoolId("pool"),device(),testBatchId="partition-$before").exists())
    }
    val transferLog=LoggerFactory.getLogger("transfer-probe") as Logger
    transferLog.level=ch.qos.logback.classic.Level.DEBUG
    val appender=ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>();appender.start();transferLog.addAppender(appender)
    val klogger=mu.KotlinLogging.logger("transfer-probe")
    val transferDevice=Proxy.newProxyInstance(Device::class.java.classLoader,arrayOf(Device::class.java)){_,method,_->when(method.name){"getSerialNumber"->"serial";"getLogger"->klogger;else->throw UnsupportedOperationException(method.name)}} as Device
    val small=File(tmp,"small").apply{writeBytes(ByteArray(999))};val folder=File(tmp,"folder");File(folder,"sub").mkdirs();File(folder,"sub/file").writeBytes(ByteArray(1500))
    val fileReturn=transferDevice.measureFileTransfer(small){Thread.sleep(20);123}
    val fileError=capture{transferDevice.measureFileTransfer(small){Thread.sleep(20);throw IllegalArgumentException("file")}}
    val folderReturn=transferDevice.measureFolderTransfer(folder){Thread.sleep(20);456}
    val folderError=capture{transferDevice.measureFolderTransfer(folder){Thread.sleep(20);throw IllegalArgumentException("folder")}}
    // Times stay in the evidence; tests check shape/counts and the integer-kB case.
    row("transfer","fileReturn" to fileReturn,"fileError" to fileError,"folderReturn" to folderReturn,"folderError" to folderError,"messages" to appender.list.map{it.formattedMessage})
    out.resolve("probes.jsonl").writeText(rows.joinToString("\n",postfix="\n"){gson.toJson(it)})
}
