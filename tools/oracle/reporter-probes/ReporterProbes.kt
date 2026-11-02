@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")
import com.google.gson.GsonBuilder
import com.google.gson.Gson
import com.malinskiy.marathon.analytics.internal.sub.*
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.execution.command.parse.MarathonTestParseCommand
import com.malinskiy.marathon.io.FileManager
import com.malinskiy.marathon.json.FileSerializer
import com.malinskiy.marathon.report.*
import com.malinskiy.marathon.report.bill.BillingReporter
import com.malinskiy.marathon.report.device.DeviceInfoJsonReporter
import com.malinskiy.marathon.report.junit.JUnitReporter
import com.malinskiy.marathon.report.raw.RawJsonReporter
import com.malinskiy.marathon.report.test.TestJsonReporter
import com.malinskiy.marathon.report.stdout.StdoutReporter
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.time.Timer
import com.malinskiy.marathon.usageanalytics.tracker.UsageTracker
import com.linkedin.dex.parser.DecodedValue.*
import ch.qos.logback.classic.Logger
import ch.qos.logback.core.read.ListAppender
import org.slf4j.LoggerFactory
import java.io.*
import java.time.Instant

val gson=GsonBuilder().registerTypeAdapter(File::class.java,FileSerializer()).create()
fun dev(serial:String="xxyyzz",model:String="Android SDK built for x86")=DeviceInfo(OperatingSystem("23"),serial,model,"unknown",NetworkState.CONNECTED,listOf(DeviceFeature.SCREENSHOT,DeviceFeature.VIDEO),true)
fun res(method:String,status:TestStatus=TestStatus.PASSED,trace:String?=null, start:Long=1541675929849,end:Long=1541675941768,test:Test=Test("com","example",method,emptyList()))=TestResult(test,dev(),"stub-batch",status,start,end,trace)
fun ev(r:TestResult,final:Boolean=true,pool:String="myPool",device:DeviceInfo=dev(),instant:Instant=Instant.ofEpochMilli(3000))=TestEvent(instant,DevicePoolId(pool),device,r,final)
fun connected(pool:String="myPool",device:DeviceInfo=dev())=DeviceConnectedEvent(Instant.EPOCH,DevicePoolId(pool),device)
fun report(events:List<TestEvent>,connections:List<DeviceConnectedEvent> = listOf(connected()),preps:List<DevicePreparingEvent> = emptyList(),providers:List<DeviceProviderPreparingEvent> = emptyList(),disconnections:List<DeviceDisconnectedEvent> = emptyList())=ExecutionReport(connections,disconnections,preps,providers,events)
val trace="Exception in thread \"main\" java.lang.RuntimeException: A test exception\n  at com.example.stacktrace.StackTraceExample.methodB(StackTraceExample.java:13)\n  at com.example.stacktrace.StackTraceExample.methodA(StackTraceExample.java:9)\n  at com.example.stacktrace.StackTraceExample.main(StackTraceExample.java:5)"
val usage=object:UsageTracker{override fun trackEvent(event:com.malinskiy.marathon.usageanalytics.Event){};override fun meta(version:String,vendor:String,releaseMode:String){};override fun close(){}}
class FixedTimer(val elapsed:Long):Timer { override val startTimeMillis=0L;override val elapsedTimeMillis get()=elapsed;override fun currentTimeMillis()=elapsed;override fun measure(block:()->Unit):Long{block();return elapsed} }
fun main(args:Array<String>){
 val out=File(args[0]);out.mkdirs();File(out,"context.json").writeText(Gson().toJson(mapOf("cwd" to System.getProperty("user.dir"),"defaultStart" to 1000)))
 val scenarios=linkedMapOf<String,ExecutionReport>()
 scenarios["empty"]=report(emptyList(),emptyList())
 scenarios["passed"]=report((1..3).map{ev(res("test$it"))})
 scenarios["failed"]=report(listOf(ev(res("test1",TestStatus.FAILURE)),ev(res("test2",TestStatus.INCOMPLETE)),ev(res("test2",TestStatus.FAILURE)),ev(res("test3",TestStatus.FAILURE))))
 scenarios["traces"]=report(listOf(ev(res("test1",TestStatus.FAILURE,trace)),ev(res("test2",TestStatus.IGNORED)),ev(res("test2",TestStatus.INCOMPLETE)),ev(res("test3",TestStatus.ASSUMPTION_FAILURE,trace))))
 scenarios["fail-pass"]=report(listOf(ev(res("test1",TestStatus.FAILURE,trace),false),ev(res("test1"))))
 scenarios["pass-fail"]=report(listOf(ev(res("test1"),false),ev(res("test1",TestStatus.FAILURE,trace))))
 val edge="<&>\"'\n\r\t😀é"
 val weird=res(edge,TestStatus.FAILURE,"α]]>β]]>γ",0,100,Test("p$edge","C",edge,emptyList()))
 scenarios["xml-edges"]=report(listOf(ev(weird),ev(res("retry",trace=""),false),ev(res("retry",TestStatus.IGNORED,null),false),ev(res("retry",TestStatus.INCOMPLETE,"omit"),false),ev(res("retry",TestStatus.FAILURE,"")),ev(res("retry",TestStatus.PASSED,null))),listOf(connected("myPool"),connected("empty")))
 val arr=arrayOf(DecodedInt(7),DecodedNull,DecodedArrayValue(arrayOf(DecodedString("nested"))))
 val equal=arrayOf(DecodedInt(7),DecodedNull,DecodedArrayValue(arrayOf(DecodedString("nested"))))
 val metadata=linkedMapOf<String,Any?>("null" to null,"empty" to "","html" to "<>&='\u2028\u2029😀","boolean" to true,"byte" to 1.toByte(),"short" to 2.toShort(),"int" to 3,"long" to 4L,"char" to 'é',"float" to -0.0f,"double" to -0.0,"tinyFloat" to Float.MIN_VALUE,"largeDouble" to Double.MAX_VALUE,"list" to listOf(null,1,"x"),"map" to linkedMapOf("null" to null,"x" to 7),"array" to arr,"alias" to arr,"equal" to equal,"literal" to "[I@123abc","decoded" to DecodedArrayValue(arrayOf(DecodedNull,DecodedDouble(1.5))),"bytes" to byteArrayOf(1,2,-1),"chars" to charArrayOf('a','é'),"wrappers" to listOf(DecodedByte((-128).toByte()),DecodedShort((-32768).toShort()),DecodedLong(Long.MIN_VALUE),DecodedFloat(1.5f),DecodedChar('é'),DecodedBoolean(true),DecodedType("Lpkg/Type;"),DecodedEnum("ENUM")))
 val test=Test("pkg","Class","meta",listOf(MetaProperty("annotation",metadata)))
 val mr=res("meta",test=test).copy(device=dev("payload"),attachments=listOf(Attachment(File("relative/../<&>"),AttachmentType.LOG),Attachment(File("/tmp/./keep"),AttachmentType.SCREENSHOT_PNG,"")))
 scenarios["metadata"]=report(listOf(ev(mr,device=dev("event:serial"))))
 val changed=dev("shared","last")
 scenarios["multi"]=report(listOf(ev(res("dup",TestStatus.FAILURE),device=dev("shared"),pool="first"),ev(res("dup"),device=changed,pool="second"),ev(res("dup",TestStatus.INCOMPLETE),device=changed,pool="second"),ev(res("orphan"),pool="orphan",device=dev("missing"))),listOf(connected("first",dev("shared","first")),connected("second",changed),connected("second",dev("shared","newest"))),listOf(DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochSecond(4,999999),"shared"),DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochSecond(4,1),"shared"),DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochMilli(500),"unused")),listOf(DeviceProviderPreparingEvent(Instant.EPOCH,Instant.EPOCH,"missing")),listOf(DeviceDisconnectedEvent(Instant.ofEpochMilli(100),DevicePoolId("second"),changed)))
 scenarios["fractional"]=report(listOf(ev(res("negative"),instant=Instant.ofEpochSecond(0,999999))),preps=listOf(DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochSecond(1,1),"xxyyzz")))
 scenarios["overflow"]=report(listOf(ev(res("overflow",start=Long.MIN_VALUE,end=Long.MAX_VALUE)),ev(res("negative",start=100,end=1)),ev(res("zero",start=0,end=100))))
 scenarios["sort"]=report(listOf("\uE000","😀","a","a").map{ev(res(it,TestStatus.FAILURE),false)}+listOf(ev(res("\uE000",TestStatus.FAILURE)),ev(res("😀",TestStatus.FAILURE)),ev(res("i",TestStatus.INCOMPLETE),false)))
 scenarios["paths"]=report(listOf(ev(res("m".repeat(300)),pool="p / raw",device=dev("s:/serial")),ev(res("m".repeat(300),TestStatus.FAILURE),pool="p / raw",device=dev("s:/serial"))),listOf(connected("p / raw",dev("s:/serial"))))
 for((id,_) in listOf("duration-negative" to -1L,"duration-zero" to 0L,"duration-positive" to 1001L)) { scenarios[id]=scenarios["passed"]!! }
 val badTest=Test("p","C","nonfinite",listOf(MetaProperty("ann",mapOf("nan" to Double.NaN))))
 scenarios["fail-json"]=report(listOf(ev(res("nonfinite",test=Test("p","C","nonfinite",emptyList()))),ev(res("nonfinite",test=badTest))))
 scenarios["duplicate-finals"]=report(listOf(ev(res("dup",TestStatus.FAILURE)),ev(res("dup",TestStatus.FAILURE)),ev(res("dup",TestStatus.IGNORED))))
 scenarios["provider-only"]=report(listOf(ev(res("provider"))),providers=listOf(DeviceProviderPreparingEvent(Instant.EPOCH,Instant.ofEpochMilli(500),"xxyyzz")))
 scenarios["prepare-only"]=report(emptyList(),preps=listOf(DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochMilli(500),"xxyyzz")))
 val u="]]>π😀]]]>"
 scenarios["rv-retried"]=report(listOf(
  ev(res("ign",TestStatus.FAILURE,"f$u"),false),ev(res("ign",TestStatus.IGNORED,"skip$u")),
  ev(res("asm",TestStatus.FAILURE,"f"),false),ev(res("asm",TestStatus.ASSUMPTION_FAILURE,"a$u")),
  ev(res("flaky",TestStatus.FAILURE,"x$u"),false),ev(res("flaky",TestStatus.PASSED,null),false),ev(res("flaky",TestStatus.ASSUMPTION_FAILURE,""),false),ev(res("flaky",TestStatus.INCOMPLETE,"i"),false),ev(res("flaky")),
  ev(res("strict",TestStatus.PASSED,"p"),false),ev(res("strict",TestStatus.IGNORED,null),false),ev(res("strict",TestStatus.ASSUMPTION_FAILURE,"am"),false),ev(res("strict",TestStatus.INCOMPLETE,null),false),ev(res("strict",TestStatus.FAILURE,u)),
  ev(res("inc",TestStatus.FAILURE,"z"),false),ev(res("inc",TestStatus.INCOMPLETE,null))))
 scenarios["rv-unicode"]=report(listOf(ev(res("тест😀",TestStatus.FAILURE,"\u0000ctl\u0001",test=Test("пкг","Клас😀","тест😀",emptyList())),pool="пул😀",device=dev("сер😀")),ev(res("é",TestStatus.IGNORED),pool="пул😀",device=dev("сер😀"),final=false),ev(res("é"),pool="пул😀",device=dev("сер😀"))),listOf(connected("пул😀",dev("сер😀"))))
 scenarios["rv-no-tests"]=report(emptyList(),listOf(connected("a"),connected("b",dev("other"))),preps=listOf(DevicePreparingEvent(Instant.EPOCH,Instant.ofEpochMilli(5),"other")))
 val identities=linkedMapOf("array" to Integer.toHexString(System.identityHashCode(arr)),"equal" to Integer.toHexString(System.identityHashCode(equal)),"null" to Integer.toHexString(System.identityHashCode(DecodedNull)),"bytes" to Integer.toHexString(System.identityHashCode(metadata["bytes"])),"chars" to Integer.toHexString(System.identityHashCode(metadata["chars"])))
 File(out,"identities.json").writeText(Gson().toJson(identities))
 for((id,r) in scenarios){
  val dir=File(out,id);dir.mkdirs();val files=FileManager(0,if(id=="paths")120 else 0,dir)
  File(dir,"input.json").writeText(gson.toJson(linkedMapOf("connected" to r.deviceConnectedEvents.map{linkedMapOf("pool" to it.poolId.name,"device" to it.device)},"disconnected" to r.deviceDisconnectedEvents.map{linkedMapOf("pool" to it.poolId.name,"device" to it.device)},"preparing" to r.devicePreparingEvents.map{linkedMapOf("startSeconds" to it.start.epochSecond,"startNanos" to it.start.nano,"finishSeconds" to it.finish.epochSecond,"finishNanos" to it.finish.nano,"serial" to it.serialNumber)},"provider" to r.deviceProviderPreparingEvent.map{linkedMapOf("startSeconds" to it.start.epochSecond,"startNanos" to it.start.nano,"finishSeconds" to it.finish.epochSecond,"finishNanos" to it.finish.nano,"serial" to it.serialNumber)},"tests" to r.testEvents.map{linkedMapOf("instantSeconds" to it.instant.epochSecond,"instantNanos" to it.instant.nano,"pool" to it.poolId.name,"device" to it.device,"result" to (if(id=="fail-json") it.testResult.copy(test=it.testResult.test.copy(metaProperties=emptyList())) else it.testResult),"nonfinite" to (id=="fail-json" && it.testResult.test.metaProperties.isNotEmpty()),"final" to it.final)})))
  val bill=BillingReporter(files,gson,usage)
  bill.javaClass.getDeclaredField("defaultStart").apply{isAccessible=true;set(bill,Instant.ofEpochMilli(1000))}
  val log=LoggerFactory.getLogger("com.malinskiy.marathon.report.bill.BillingReporter") as Logger
  log.level=ch.qos.logback.classic.Level.DEBUG;log.isAdditive=false
  val warnings=ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>();warnings.start();log.addAppender(warnings)
  val fileLog=LoggerFactory.getLogger("FileManager") as Logger;fileLog.isAdditive=false
  val stdout=ByteArrayOutputStream();val previous=System.out;System.setOut(PrintStream(stdout,true,"UTF-8"))
  try { listOf(DeviceInfoJsonReporter(files,gson),bill,JUnitReporter(dir),RawJsonReporter(files,gson),TestJsonReporter(files,gson),StdoutReporter(FixedTimer(when(id){"sort"->90061999;"duration-negative"->-1;"duration-zero"->0;"duration-positive"->1001;else->999}))).forEach{it.generate(r)} } catch(e:Exception){File(dir,"error.json").writeText(Gson().toJson(mapOf("class" to e.javaClass.name,"message" to e.message)))} finally {System.setOut(previous);log.detachAppender(warnings)}
  File(dir,"stdout").writeBytes(stdout.toByteArray());File(dir,"warnings.json").writeText(Gson().toJson(warnings.list.map{it.formattedMessage}))
 }
 val parse=File(out,"parse");parse.mkdirs();val command=MarathonTestParseCommand(parse)
 for((id,tests,flaky) in listOf(Triple("empty",emptyList(),emptyList()),Triple("one",listOf(Test("pkg","Class","method",emptyList())),emptyList()),Triple("several",listOf(Test("p","C","a",emptyList()),Test("p","C","a",emptyList())),emptyList()),Triple("flaky",listOf(test),listOf(Test("p","C","f",emptyList()))))){command.execute(tests,flaky,id)}
 val bad=Test("p","C","nonfinite",listOf(MetaProperty("ann",mapOf("nan" to Double.NaN))))
 val errors=linkedMapOf<String,String>()
 val badDir=File(out,"nonfinite");val badFiles=FileManager(0,0,badDir)
 try{TestJsonReporter(badFiles,gson).generate(report(listOf(ev(res("bad",test=bad)))))}catch(e:Exception){errors["test"]=e.javaClass.name+": "+e.message}
 RawJsonReporter(badFiles,gson).generate(report(listOf(ev(res("bad",test=bad)))))
 command.execute(listOf(bad),emptyList(),"nonfinite")
 val junitErrors=listOf("blocked-parent","directory-target").map{id->
  val dir=File(out,"junit-error-$id");dir.mkdirs();val target=File(dir,"out")
  if(id=="blocked-parent")target.writeText("blocker") else File(target,"tests/myPool/marathon_junit_report.xml").mkdirs()
  var error:Map<String,String?>?=null
  try{JUnitReporter(target).generate(scenarios["passed"]!!)}catch(e:Exception){error=linkedMapOf("class" to e.javaClass.name,"message" to e.message?.replace(dir.absolutePath,"<out>"))}
  linkedMapOf("id" to id,"error" to error)
 }
 File(out,"junit-errors.jsonl").writeText(junitErrors.joinToString("\n",postfix="\n"){Gson().toJson(it)})
 val mapper=command.javaClass.getDeclaredField("mapper").apply{isAccessible=true}.get(command) as com.fasterxml.jackson.databind.ObjectMapper
 File(out,"binary.jsonl").writeText(listOf(0,1,2,3,55,56,57,58,114,120).flatMap{size->listOf(false,true).map{list->
  val data=ByteArray(size){it.toByte()};val value:Any=if(list)listOf(data) else linkedMapOf("binary" to data)
  Gson().toJson(linkedMapOf("size" to size,"list" to list,"yaml" to mapper.writeValueAsString(value).toByteArray(Charsets.UTF_8).map{it.toInt() and 255},"gson" to gson.toJson(value).toByteArray(Charsets.UTF_8).map{it.toInt() and 255}))
 }}.joinToString("\n",postfix="\n"))
 File(out,"errors.json").writeText(Gson().toJson(errors))
}
