// Pinned, unchanged marathon e47352f2 and adam 0.5.8 classes; loopback only.
import com.google.gson.Gson
import com.malinskiy.adam.*
import com.malinskiy.adam.request.Feature
import com.malinskiy.adam.request.testrunner.*
import com.malinskiy.adam.request.forwarding.*
import com.malinskiy.adam.request.reverse.*
import com.malinskiy.marathon.android.*
import com.malinskiy.marathon.android.adam.*
import com.malinskiy.marathon.android.adam.execution.ArgumentsFactory
import com.malinskiy.marathon.android.executor.listeners.*
import com.malinskiy.marathon.android.model.AndroidTestBundle
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.*
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.execution.TestBatchResults
import com.malinskiy.marathon.test.Test as MTest
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import java.io.*
import java.net.*
import java.time.*
import java.util.concurrent.CountDownLatch
import kotlin.concurrent.thread
fun row(case:String, value:Any?)=println(Gson().toJson(linkedMapOf("case" to case,"value" to value)))
fun vendor()=VendorConfiguration.AndroidConfiguration(androidSdk=File("/nonexistent"),applicationOutput=null,testApplicationOutput=null,splitApks=null,extraApplicationsOutput=null,applicationPmClear=false,testApplicationPmClear=false,allureConfiguration=AllureConfiguration(enabled=false),mockLocation=false)
fun config(v:VendorConfiguration.AndroidConfiguration, coverage:Boolean=false,batchTimeout:Long=3000)=Configuration.Builder(name="runner-probe",outputDir=File(System.getProperty("java.io.tmpdir"),"output")).apply {vendorConfiguration=v;isCodeCoverageEnabled=coverage;testOutputTimeoutMillis=1000;testBatchTimeoutMillis=batchTimeout}.build()
val timer=SystemTimer(Clock.fixed(Instant.EPOCH,ZoneOffset.UTC))
fun device(client:AndroidDebugBridgeClient,v:VendorConfiguration.AndroidConfiguration,serial:String,identifier:AndroidTestBundleIdentifier=AndroidTestBundleIdentifier()):AdamAndroidDevice = AdamAndroidDevice(client,DeviceStateTracker(),LogcatManager(),Dispatchers.IO,serial,config(v),v,identifier,Track(),timer,SerialStrategy.AUTOMATIC).apply{supportedFeatures=emptyList()}
class Stub(val acceptShell:Boolean=true,val body:(DataInputStream,OutputStream)->Unit):AutoCloseable {
 val server=ServerSocket(0,10,InetAddress.getLoopbackAddress())
 val client=AndroidDebugBridgeClientFactory().apply { host=InetAddress.getLoopbackAddress();port=server.localPort }.build()
 val services=java.util.Collections.synchronizedList(mutableListOf<String>())
 val worker=thread(isDaemon=true) { while(!server.isClosed) {val socket=try{server.accept()}catch(e:Exception){break};thread(isDaemon=true){socket.use { socket.soTimeout=1000; val i=DataInputStream(it.getInputStream());val o=it.getOutputStream();try{val n=String(i.readNBytes(4)).toInt(16);val transport=String(i.readNBytes(n));o.write("OKAY".toByteArray());o.flush();val m=String(i.readNBytes(4)).toInt(16);services.add(String(i.readNBytes(m)));if(!acceptShell)return@use;o.write("OKAY".toByteArray());o.flush();body(i,o)}catch(e:Exception){} }}} }
 override fun close(){client.close();server.close();worker.join()}
}
fun bundle(info:InstrumentationInfo)=AndroidTestBundle(null,File("unused.apk"),null,null).apply {
 javaClass.getDeclaredField("instrumentationInfo\$delegate").apply{isAccessible=true}.set(this,lazyOf(info))
}
class Events(val throwEvent:Boolean=false,val throwAfter:Boolean=false):AndroidTestRunListener {
 val values=mutableListOf<String>()
 override suspend fun beforeTestRun(info:InstrumentationInfo?){values.add("before:${info?.instrumentationPackage}")}
 override suspend fun testRunStarted(runName:String,testCount:Int){values.add("run:$runName:$testCount")}
 override suspend fun testStarted(test:com.malinskiy.marathon.android.model.TestIdentifier){values.add("start:${test.testName}");if(throwEvent)error("callback")}
 override suspend fun testIgnored(test:com.malinskiy.marathon.android.model.TestIdentifier){values.add("ignored:${test.testName}")}
 override suspend fun testEnded(test:com.malinskiy.marathon.android.model.TestIdentifier,testMetrics:Map<String,String>){values.add("end:${test.testName}")}
 override suspend fun testRunFailed(errorMessage:String){values.add("failed:$errorMessage")}
 override suspend fun testRunEnded(elapsedTime:Long,runMetrics:Map<String,String>){values.add("ended:$elapsedTime")}
 override suspend fun testRunFailing(error:String,stackTrace:String){values.add("failing")}
 override suspend fun afterTestRun(){values.add("after");if(throwAfter)error("after")}
}
fun main():Unit=runBlocking {
 val v=vendor()
 val client=AndroidDebugBridgeClientFactory().apply{port=12345;host=InetAddress.getLoopbackAddress()}.build()
 for((label,input) in listOf("numtests" to "INSTRUMENTATION_STATUS: numtests=many\nINSTRUMENTATION_STATUS_CODE: 1\n", "malformed" to "garbage line\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n")) {
  val request=TestRunnerRequest("test",InstrumentOptions(),emptyList(),this)
  try { request.convertChunk(com.malinskiy.adam.request.shell.v2.ShellCommandResultChunk(stdout=input.toByteArray())) } catch(e:Throwable) { row("parser-error:$label",listOf(e.javaClass.name,e.message,e.cause?.javaClass?.name)) }
 }
 val default=InstrumentOptions(); row("options:default",default.toString())
 val all=InstrumentOptions(pkg=listOf("com.example"),clazz=listOf("com.example.TestClass"),functional=true,unit=true,filterSize=InstrumentationSizeOption.MEDIUM,performance=true,debug=true,log=true,emma=true,coverageFile="/sdcard/codecov",overrides=linkedMapOf("test" to "test"));row("options:all",all.toString())
 row("options:lists-false-collision",InstrumentOptions(pkg=listOf("p","q"),clazz=listOf("C#m[0]","C#space\\ name"),functional=false,unit=false,filterSize=InstrumentationSizeOption.LARGE,performance=false,debug=false,log=false,emma=false,overrides=linkedMapOf("class" to "override","log" to "false")).toString())
 row("request:default",TestRunnerRequest("com.example.test",default,emptyList(),this).cmd)
 row("request:all",TestRunnerRequest("com.example.test",default,emptyList(),this,runnerClass="com.example.test.AndroidTestRunner",noWindowAnimations=true,noHiddenApiChecks=true,noIsolatedStorage=true,userId=1000,abi="x86",profilingOutputPath="/sdcard/profiling",outputLogPath="/sdcard/log").cmd)
 for(serial in listOf("emulator-5554","emulator- +42 ","emulator-2147483647","emulator-2147483648","emulator-bad","physical","127.0.0.1:5555")) {
  val d=device(client,v,serial)
  for(mask in 0..7){val a=v.copy(testAccessConfiguration=TestAccessConfiguration(adb=mask and 1!=0,console=mask and 2!=0,grpc=mask and 4!=0,consoleToken=" token "),instrumentationArgs=linkedMapOf("coverage" to "user","com.malinskiy.adam.android.ADB_PORT" to "user","class" to "user"));row("args:$serial:$mask",ArgumentsFactory(d).generate(config(a,true),a).entries.map{listOf(it.key,it.value)})}
  d.dispose()
 }
 val d=device(client,v,"physical")
 d.portForwardingRules["adb"]=ReversePortForwardingRule("serial",RemoteTcpPortSpec(24680),LocalTcpPortSpec(12345))
 val access=v.copy(testAccessConfiguration=TestAccessConfiguration(adb=true,console=false,grpc=false))
 row("args:reverse",ArgumentsFactory(d).generate(config(access),access).entries.map{listOf(it.key,it.value)})
 d.portForwardingRules.clear();d.dispose()
 val meta=listOf(MetaProperty("example.Annotation",mapOf("value" to "x")))
 val identifier=AndroidTestBundleIdentifier();val dev=device(client,v,"emulator-5554",identifier)
 val tests=listOf(MTest("pkg","Clazz","null",meta),MTest("pkg","Clazz","space name",meta))
 val prepare=AndroidDeviceTestRunner::class.java.declaredMethods.single{it.name=="prepareTestRunnerRequest"}.apply{isAccessible=true}
 row("request:batch",(prepare.invoke(AndroidDeviceTestRunner(dev,identifier),config(v,true),v,InstrumentationInfo("app","test","Runner"),TestBatch(tests,"batch"),"/data/data/app/coverage/coverage-batch.ec") as TestRunnerRequest).cmd)
 val deferred=CompletableDeferred<TestBatchResults>()
 val results=TestResultsListener(TestBatch(listOf(MTest("pkg","Clazz","method",meta)),"batch"),dev,deferred,timer,DevicePoolId("pool"),emptyList())
 results.testStarted(com.malinskiy.marathon.android.model.TestIdentifier("pkg.Clazz","method"));results.testEnded(com.malinskiy.marathon.android.model.TestIdentifier("pkg.Clazz","method"),emptyMap());results.afterTestRun();row("results:N1",deferred.await().finished.single().test.metaProperties.size)
 val compositeEvents=Events();CompositeTestRunListener(listOf(compositeEvents)).testRunFailing("e","s");row("listener:N2",compositeEvents.values)
 val ignored=MTest("pkg","Clazz","ignored",listOf(MetaProperty("org.junit.Ignore",emptyMap())))
 val events=Events();AndroidDeviceTestRunner(dev,identifier).execute(config(v),TestBatch(listOf(ignored),"batch"),events);row("runner:all-ignored",events.values)
 val duplicateIgnoredEvents=Events();AndroidDeviceTestRunner(dev,identifier).execute(config(v),TestBatch(listOf(ignored,ignored.copy(metaProperties=emptyList())),"batch"),duplicateIgnoredEvents);row("runner:ignored-equality",duplicateIgnoredEvents.values)
 val hookEvents=mutableListOf<String>()
 val hookTest=MTest("p","C","m",emptyList());identifier.put(hookTest,bundle(InstrumentationInfo("app","test","Runner")))
 val hook=object:AndroidTestRunListener {
  override suspend fun beforeTestRun(info:InstrumentationInfo?){hookEvents.add("before");try{awaitCancellation()}finally{hookEvents.add("before-cleanup")}}
  override suspend fun testRunFailed(errorMessage:String){hookEvents.add("failed:$errorMessage")}
  override suspend fun afterTestRun(){hookEvents.add("after")}
 }
 AndroidDeviceTestRunner(dev,identifier).execute(config(v,batchTimeout=10),TestBatch(listOf(hookTest),"batch"),hook)
 row("runner:suspended-hook-timeout",hookEvents)
 dev.dispose();client.close()
 Stub{_,o->o.write("INSTRUMENTATION_CODE: -1\n".toByteArray());o.flush()}.use{s->
  val ids=AndroidTestBundleIdentifier();val dd=device(s.client,v,"emulator-5554",ids)
  val a=MTest("pkg","One","one",emptyList());val b=MTest("pkg","Two","two",emptyList())
  ids.put(a,bundle(InstrumentationInfo("app","test1","Runner")));ids.put(b,bundle(InstrumentationInfo("app","test2","Runner")))
  val e=Events();AndroidDeviceTestRunner(dd,ids).execute(config(v),TestBatch(listOf(a,b,ignored),"batch"),e);row("runner:multiple-services",s.services);row("runner:multiple-events",e.values);dd.dispose()
 }
 // Cancellation is suspended receive cancellation, despite NonCancellable.isActive.
 val ch=kotlinx.coroutines.channels.Channel<Int>();val waiting=CompletableDeferred<Unit>();val job=launch {try{waiting.complete(Unit);while(NonCancellable.isActive){ch.receive()}}finally{row("remote:N3","finally")}};waiting.await();job.cancelAndJoin();row("remote:N3-cancelled",job.isCancelled)
 // A finally/after exception overrides the callback failure and skips channel.cancel.
 val holding=CountDownLatch(1)
 val observed=CompletableDeferred<String>()
 Stub{i,o->o.write("INSTRUMENTATION_STATUS: class=p.C\nINSTRUMENTATION_STATUS: test=m\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\n".toByteArray());o.flush();holding.await();observed.complete(try{if(i.read()==-1)"closed" else "bytes"}catch(e:SocketTimeoutException){"still-open"})}.use{s->
  val ids=AndroidTestBundleIdentifier();val dd=device(s.client,v,"emulator-5554",ids);val t=MTest("p","C","m",emptyList());ids.put(t,bundle(InstrumentationInfo("app","test","Runner")));val e=Events(true,true)
  try{AndroidDeviceTestRunner(dd,ids).execute(config(v),TestBatch(listOf(t),"batch"),e)}catch(ex:Throwable){row("runner:after-precedence",ex.message)}
  row("runner:after-events",e.values);holding.countDown();row("runner:after-producer",observed.await());dd.dispose()
 }
 Stub(false){_,_->}.use{s->
  val ids=AndroidTestBundleIdentifier();val dd=device(s.client,v,"emulator-5554",ids);val t=MTest("p","C","m",emptyList());ids.put(t,bundle(InstrumentationInfo("app","test","Runner")));val e=Events()
  AndroidDeviceTestRunner(dd,ids).execute(config(v),TestBatch(listOf(t),"batch"),e);row("runner:handshake-eof-events",e.values);dd.dispose()
 }
 Stub{_,o->o.write("INSTRUMENTATION_CODE: -1\n".toByteArray());o.flush()}.use{s->
  val ids=AndroidTestBundleIdentifier();val dd=device(s.client,v,"emulator-5554",ids);val t=MTest("p","C","m",emptyList());ids.put(t,bundle(InstrumentationInfo("app","test","Runner")))
  AndroidDeviceTestRunner(dd,ids).execute(config(v),TestBatch(listOf(t),"batch"),Events());s.client.close();val e=Events()
  AndroidDeviceTestRunner(dd,ids).execute(config(v),TestBatch(listOf(t),"batch"),e);row("runner:closed-client-events",e.values);dd.dispose()
 }
 kotlin.system.exitProcess(0)
}
