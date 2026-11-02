// Original artifact listener classes, with a device proxy for isolated edge witnesses.
import com.google.gson.Gson
import com.malinskiy.marathon.android.*
import com.malinskiy.marathon.android.model.*
import com.malinskiy.marathon.android.executor.listeners.screenshot.*
import com.malinskiy.marathon.android.executor.listeners.profiling.*
import com.malinskiy.marathon.config.*
import com.malinskiy.marathon.config.vendor.android.*
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.device.screenshot.*
import com.malinskiy.marathon.io.*
import com.malinskiy.marathon.test.TestBatch
import kotlinx.coroutines.*
import java.io.File
import java.lang.reflect.Proxy
import java.time.Duration
import kotlin.coroutines.intrinsics.*
import kotlin.coroutines.*
fun row(case:String,value:Any?)=println(Gson().toJson(linkedMapOf("case" to case,"value" to value)))
suspend fun invoke(target:Any,name:String):Any?=suspendCoroutineUninterceptedOrReturn { c -> target.javaClass.declaredMethods.single{it.name==name}.apply{isAccessible=true}.invoke(target,c) }
fun main():Unit=runBlocking {
 var hold=false;var pending:Continuation<Any?>?=null;var ps="";val commands=mutableListOf<String>();lateinit var device:AndroidDevice
 device=Proxy.newProxyInstance(AndroidDevice::class.java.classLoader,arrayOf(AndroidDevice::class.java)){_,method,args->
  when(method.name){
   "getVersion"->com.android.sdklib.AndroidVersion(33,"REL")
   "getApiLevel"->33
   "getHealthy"->true
   "getSerialNumber"->"SERIAL"
   "getExternalStorageMount"->"/sdcard"
   "getFileManager"->RemoteFileManager(device)
   "getModel","getManufacturer"->"test"
   "getOperatingSystem"->OperatingSystem("33")
   "getAbi"->"x86"
   "getDeviceFeatures"->emptyList<DeviceFeature>()
   "getNetworkState"->NetworkState.CONNECTED
   "getOrientation","getInitialRotation"->Rotation.ROTATION_0
   "safeExecuteShellCommand","executeShellCommand","criticalExecuteShellCommand"->{commands.add(args[0] as String);if(hold && (args[0] as String).contains("perfetto --txt")){pending=args.last() as Continuation<Any?>;COROUTINE_SUSPENDED}else ShellCommandResult(if((args[0] as String).startsWith("ps"))ps else "",0)}
   "safePullFile"->{commands.add("pull:"+args[0]);Unit}
   "getScreenshot"->throw java.io.IOException("capture failed")
   "toString"->"device"
   else->null
  }
 } as AndroidDevice
 val klass=Class.forName("com.malinskiy.marathon.android.executor.listeners.video.ScreenRecorder")
 val recorder=klass.declaredConstructors.single().apply{isAccessible=true}.newInstance(device,VideoConfiguration())
 for((name,output) in listOf("normal" to "shell 123 1 screenrecord\n", "grep-last" to "shell 123 1 screenrecord\nshell 456 1 grep screenrecord\n", "bad" to "shell pid 1 screenrecord\n", "empty" to "", "tab" to "shell\t123 1 screenrecord\n")) {ps=output;row("pid:$name",invoke(recorder,"grepPid"))}
 ps="shell 123 1 screenrecord\nshell 456 1 grep screenrecord\n";commands.clear();invoke(recorder,"stopScreenRecord");row("stop:grep-last",commands.toList())
 val job=SupervisorJob();val events=mutableListOf<String>();job.cancelAndJoin();events.add("joined");val thread=Thread.currentThread().name;job.invokeOnCompletion{events.add("handler:"+(thread==Thread.currentThread().name))};events.add("next");row("completed-handler",events)
 for(pid in listOf<Int?>(null,42)){val lookup=org.apache.commons.text.StringSubstitutor(org.apache.commons.text.lookup.StringLookupFactory.INSTANCE.mapStringLookup(mapOf("TRACING_TARGET_PID" to pid,"TRACING_TARGET_PACKAGE" to "app")));row("substitution:$pid",lookup.replace("pid=\${TRACING_TARGET_PID},pkg=\${TRACING_TARGET_PACKAGE}"))}
 val files=FileManager(4096,255,File(System.getProperty("java.io.tmpdir"),"out"));val pool=DevicePoolId("pool");val test=TestIdentifier("pkg.clazz","method")
 val adam=AdamScreenCaptureTestRunListener(pool,device,files,"batch");commands.clear();adam.testEnded(test,mapOf("com.malinskiy.adam.junit4.android.screencapture.AdamScreenCaptureProcessor.v1" to "/sdcard/a.JPG"));row("adam:unknown-pulls",commands.toList());row("adam:logger",(adam.javaClass.getDeclaredField("logger").apply{isAccessible=true}.get(adam) as org.slf4j.Logger).name)
 val screenshot=ScreenCapturerTestRunListener(files,pool,"batch",device,ScreenRecordingPolicy.ON_FAILURE,ScreenshotConfiguration(delayMs=60000),Duration.ofSeconds(5),this)
 screenshot.testStarted(test);yield();screenshot.testAssumptionFailure(test,"assumed");screenshot.testEnded(test,emptyMap());row("screenshot:assumption-file",files.createFile(FileType.SCREENSHOT,pool,device.toDeviceInfo(),test.toTest(),"batch").exists())
 val bad=ProfilingRunListener(files,pool,TestBatch(listOf(test.toTest()),"batch"),device,ProfilingConfiguration(pbtxt=File("/nonexistent/config.pbtxt")),AndroidTestBundleIdentifier(),this)
 try{bad.beforeTestRun(null)}catch(e:Exception){row("profiling:missing-pbtxt",listOf(e.javaClass.name,e.message))}
 val profiling=ProfilingRunListener(files,pool,TestBatch(listOf(test.toTest()),"batch"),device,ProfilingConfiguration(),AndroidTestBundleIdentifier(),this)
 hold=true;profiling.testStarted(test);yield();commands.clear();profiling.testRunFailed("broken");row("profiling:run-failed",commands.toList());val active=profiling.javaClass.getDeclaredField("job").apply{isAccessible=true}.get(profiling) as Job;row("profiling:run-failed-job-active",active.isActive);pending!!.resume(null);active.join()
 row("profiling:overrides",profiling.javaClass.declaredMethods.map{it.name}.filter{it=="testRunFailed"})
}
