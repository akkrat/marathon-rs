import com.malinskiy.marathon.apple.*
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.apple.ios.device.FakeSimulatorWorker
import com.malinskiy.marathon.apple.model.*
import com.malinskiy.marathon.apple.test.*
import com.malinskiy.marathon.apple.listener.AppleTestRunListener
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.apple.*
import com.malinskiy.marathon.device.Device
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import org.mockito.kotlin.mock
import java.io.File
import java.lang.reflect.Proxy
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.time.Duration
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

fun throwable(e:Throwable?):Any? = e?.let { linkedMapOf("class" to it.javaClass.name,"message" to it.message,"cause" to throwable(it.cause),"suppressed" to it.suppressed.map { t -> throwable(t) }) }
class RecordingSession(val closing:()->Unit):CommandSession {
 override val stdout=Channel<String>().apply{close()};override val stderr=Channel<String>().apply{close()};override val exitCode=CompletableDeferred<Int?>(0);override val isAlive=false
 override suspend fun await()=CommandResult(emptyList(),emptyList(),0)
 override suspend fun drain()=Unit;override fun terminate()=Unit;override fun kill()=Unit;override fun close()=closing()
}
fun proxy(handler:(String,Array<out Any?>)->Any?):AppleDevice = Proxy.newProxyInstance(AppleDevice::class.java.classLoader,arrayOf(AppleDevice::class.java)) { _, method,a ->
 try {handler(method.name,a?:emptyArray())}catch(e:Throwable) {
  if(a?.lastOrNull() is Continuation<*>) { @Suppress("UNCHECKED_CAST") val c=a.last() as Continuation<Any?>;c.resumeWithException(e);COROUTINE_SUSPENDED }else throw e
 }
} as AppleDevice
fun suspendProxy(a:Array<out Any?>,action:suspend ()->Any?):Any? { @Suppress("UNCHECKED_CAST") val c=a.last() as Continuation<Any?>;return action.startCoroutineUninterceptedOrReturn(c) }
fun config(root:File):Pair<Configuration,VendorConfiguration> {
 fun bundle(name:String):File=File(root,name).apply {mkdirs();File(this,"Info.plist").writeText("""<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.${name.substringBefore('.')}</string><key>CFBundleName</key><string>${name.substringBefore('.')}</string></dict></plist>""");File(this,name.substringBefore('.')).writeBytes(byteArrayOf(0xcf.toByte(),0xfa.toByte(),0xed.toByte(),0xfe.toByte(),0,0,0,0))}
 val bundle=AppleTestBundleConfiguration().apply {app=bundle("App.app");xctest=bundle("Tests.xctest")}
 return Configuration.Builder("probe",File(root,"out")).build() to VendorConfiguration.IOSConfiguration(bundle=bundle)
}
fun detailedCases(root:File) = runBlocking {
 val (configuration,vendor)=config(File(root,"cases"))
 val logContext=org.slf4j.LoggerFactory.getILoggerFactory() as ch.qos.logback.classic.LoggerContext
 com.malinskiy.marathon.log.MarathonLogging.debug=true
 for(mode in listOf("runner-failed-after","runner-close-failure","runner-close-timeout","runner-cancel","xctest-empty","xctest-cancel","xctest-push-false","xctest-platform","nm-symbols","nm-rescan","nm-wrong-device","xctest-wrong-device","stderr")) {
  val calls=CopyOnWriteArrayList<String>();val commands=CopyOnWriteArrayList<List<String>>();val logs=CopyOnWriteArrayList<Any>();val release=CountDownLatch(1);val closed=CountDownLatch(1);val failureRecorded=CountDownLatch(1);var scanned=0;var current=0;val rejected=CompletableDeferred<Unit>();var temp:File?=null
  val appender=object:ch.qos.logback.core.AppenderBase<ch.qos.logback.classic.spi.ILoggingEvent>() {override fun append(e:ch.qos.logback.classic.spi.ILoggingEvent) {
   if(e.loggerName in listOf("NmTestParser","XCTestParser","Xcodebuild","com.malinskiy.marathon.apple.AppleDeviceTestRunner","com.malinskiy.marathon.apple.XcodebuildSession")) {
    val text=e.formattedMessage.replace(root.absolutePath,"<bundle>").replace(Regex("closed in \\d+ ms"),"closed in <ms> ms")
    logs.add(linkedMapOf("logger" to e.loggerName,"level" to e.level.toString(),"message" to text,"throwable" to e.throwableProxy?.let {ch.qos.logback.classic.spi.ThrowableProxyUtil.asString(it).lineSequence().filterNot { l->l.trimStart().startsWith("at ")||l.trimStart().startsWith("... ") }.joinToString("\n")}))
   }
  }};appender.context=logContext;appender.start();logContext.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME).addAppender(appender)
  val worker=FakeSimulatorWorker()
  worker.before={ command ->
   commands.add(command)
   when {
    command.first()=="sh" && command.last().contains("nm ")-> {
     val swift=command.last().contains("swift-demangle");scanned++
     if(mode=="nm-rescan") {
      if(current++>0) {current--;rejected.complete(Unit);throw net.schmizz.sshj.connection.channel.OpenFailException("session",net.schmizz.sshj.connection.channel.OpenFailException.Reason.RESOURCE_SHORTAGE,"no more sessions")}
      if(scanned==1) {rejected.await();delay(10)};current--
     }
     CommandResult(if(swift) listOf("\u00a0Tests.Class.testA(\u00a0","Tests.Class.testDISABLE()","Too.Many.Class.testX()","Tests.Class.testA(")else listOf("Class testB","Class  testC","Class testB extra","Class testB"),emptyList(),0)
    }
    else -> FakeSimulatorWorker.ok()
   }
  }
  val binary=AppleBinaryEnvironment(worker,configuration,TimeoutConfiguration(),gson)
  lateinit var fm:RemoteFileManager
  val listeners=mutableListOf<com.malinskiy.marathon.execution.listener.LineListener>()
  val device=proxy {name,a->when(name) {
   "getStoragePath"->"/tmp/marathon/udid";"getSerialNumber"->"udid@local";"getUdid"->"udid";"getSdk"->if(mode=="xctest-platform") Sdk.IPHONEOS else Sdk.IPHONESIMULATOR;"getBinaryEnvironment"->binary;"getRemoteFileManager"->fm
   "pushShared"->true
   "pushFile"->{temp=a[0] as File;mode!="xctest-push-false"}
   "executeWorkerCommand"->suspendProxy(a){ @Suppress("UNCHECKED_CAST") val command=a[0] as List<String>;worker.safeExecute(command,Duration.ofSeconds(10),Duration.ofSeconds(10),emptyMap(),null) }
   "addLineListener"->{listeners.add(a[0] as com.malinskiy.marathon.execution.listener.LineListener);calls.add("add listener");Unit}
   "removeLineListener"->{listeners.remove(a[0]);calls.add("remove listener");Unit}
   "executeTestRequest"->{val session=a[1] as XcodebuildSession;session.produce(open={RecordingSession {
    if(mode in listOf("runner-failed-after","runner-close-failure","runner-close-timeout")) failureRecorded.await()
    calls.add("closing")
    if(mode=="runner-close-timeout"||mode=="runner-cancel") release.await(3,TimeUnit.SECONDS)
    if(mode=="runner-close-failure") throw java.io.IOException("channel close not confirmed")
    calls.add("closed");closed.countDown()
   }}) {
    if(mode.startsWith("runner")) {
     send(listOf(TestRunStartedEvent))
     if(mode=="runner-cancel") throw CancellationException("cancelled")
     send(listOf(TestRunFailed("Test runner exited",com.malinskiy.marathon.apple.logparser.parser.DeviceFailureReason.Unknown),TestRunEnded))
    }else if(mode=="xctest-cancel") {send(listOf(TestStarted(Test("Tests","SampleTests","testSample",emptyList()))));throw CancellationException("parse cancelled")}
   }}
   else->error("unexpected $name")
  }}
  fm=RemoteFileManager(device)
  val identifier=AppleTestBundleIdentifier();var tests:List<Test>?=null;var error:Throwable?=null
  try {
   if(mode.startsWith("runner")) {
    val b=(vendor as VendorConfiguration.IOSConfiguration).bundle!!;val bundle=AppleTestBundle(b.app,b.testApp,b.xctest!!,Sdk.IPHONESIMULATOR)
    AppleDeviceTestRunner(device,identifier,Duration.ofMillis(20)).execute(configuration,bundle,TestBatch(emptyList()),object:AppleTestRunListener {
     override suspend fun testRunFailed(errorMessage:String,reason:com.malinskiy.marathon.apple.logparser.parser.DeviceFailureReason) {calls.add("failed:$reason")}
     override suspend fun afterTestRun() {calls.add("after");failureRecorded.countDown();if(mode=="runner-failed-after"||mode=="runner-cancel") throw IllegalStateException("no result bundle")}
    })
   } else if(mode.startsWith("nm")) {
    tests=NmTestParser(configuration,vendor,TestParserConfiguration.NmTestParserConfiguration(listOf(Regex(".*"))),identifier).extract(if(mode=="nm-wrong-device") mock<Device>() else device)
   } else if(mode.startsWith("xctest")) {
    tests=XCTestParser(configuration,vendor,identifier,mock<AppleApplicationInstaller<AppleDevice>>(),Duration.ofMillis(20)).extract(if(mode=="xctest-wrong-device")mock<Device>()else device)
   } else if(mode=="stderr") {
    com.malinskiy.marathon.log.MarathonLogging.debug=false
    worker.session={ command -> if(command.take(3)==listOf("xcrun","xcodebuild","test-without-building")) object:CommandSession {
      override val stdout=Channel<String>().apply {close()}
      override val stderr=Channel<String>(Channel.UNLIMITED).apply {listOf(""," \t"," warning ").forEach {trySend(it)};close()}
      override val exitCode=CompletableDeferred<Int?>(65);override val isAlive=false
      override suspend fun await()=error("unused");override suspend fun drain()=Unit;override fun terminate()=Unit;override fun kill()=Unit;override fun close()=Unit
    }else null}
    val processor=com.malinskiy.marathon.apple.ArtifactProcessor()
    val actual=com.malinskiy.marathon.apple.ios.AppleSimulatorDevice("udid",com.malinskiy.marathon.apple.configuration.Transport.Local,Sdk.IPHONESIMULATOR,binary,identifier,com.malinskiy.marathon.io.FileManager(0,0,File(root,"out")),configuration,vendor as VendorConfiguration.IOSConfiguration,worker,mock(),SharedTransfers(),processor,com.malinskiy.marathon.analytics.internal.pub.Track(),mock())
    actual.addLineListener(object:com.malinskiy.marathon.execution.listener.LineListener {override suspend fun onLine(line:String){calls.add("listener:$line")}})
    val session=XcodebuildSession()
    for(events in actual.executeTestRequest(TestRequest("/tmp/marathon/udid","/tmp/marathon/udid/marathon.xctestrun",coverage=false),session)) {}
    session.stop(Duration.ofSeconds(1));actual.dispose();processor.closeAndJoin();commands.addAll(worker.commands)
    com.malinskiy.marathon.log.MarathonLogging.debug=true

   }
  }catch(e:Throwable) {error=e}
  calls.add("returned")
  val ordered=calls.toList();release.countDown();closed.await(100,TimeUnit.MILLISECONDS);temp?.delete()
  row(linkedMapOf("case" to mode,"tests" to tests?.map {listOf(it.pkg,it.clazz,it.method)},"error" to throwable(error),"calls" to ordered,"commands" to commands,"logs" to logs))
  logContext.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME).detachAppender(appender)
 }
 // Bare Job semantics, including a successful late close after abandonment.
 val session=XcodebuildSession();row(linkedMapOf("case" to "session-before-produce","before" to session.closed,"failure" to throwable(session.stop(Duration.ofSeconds(1))),"closed" to session.closed,"abandoned" to session.abandoned))
 val release=CountDownLatch(1);val closing=CountDownLatch(1);val s=XcodebuildSession();val channel=s.produce(open={RecordingSession {closing.countDown();release.await(3,TimeUnit.SECONDS)}}){}
 for(e in channel){};closing.await(1,TimeUnit.SECONDS);val before=s.closed;val error=s.stop(Duration.ofMillis(20));release.countDown();val after=s.awaitClosed(Duration.ofSeconds(1));row(linkedMapOf("case" to "session-late-close","before" to before,"error" to throwable(error),"closed" to after,"abandoned" to s.abandoned))
 val terminal=mutableListOf<Any>()
 for(failed in listOf(false,true)) {
  val owner=XcodebuildSession();val events=owner.produce(open={RecordingSession {}}) {if(failed) throw java.io.IOException("output failed")}
  val before=events.receiveCatching();events.cancel();val after=events.receiveCatching();owner.stop(Duration.ofSeconds(1))
  terminal.add(linkedMapOf("failed" to failed,"before" to (before.exceptionOrNull()?.toString()?:"closed"),"after" to (after.exceptionOrNull()?.toString()?:"closed"),"closed" to owner.closed))
 }
 row(linkedMapOf("case" to "session-channel-cancel","results" to terminal))

}
