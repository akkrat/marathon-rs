// Actual Android device and artifact listeners on the strict fake; no adb executable.
import com.google.gson.Gson
import com.malinskiy.adam.*
import com.malinskiy.marathon.android.*
import com.malinskiy.marathon.android.adam.*
import com.malinskiy.marathon.android.model.*
import com.malinskiy.marathon.android.executor.listeners.*
import com.malinskiy.marathon.android.executor.listeners.filesync.*
import com.malinskiy.marathon.android.executor.listeners.video.*
import com.malinskiy.marathon.android.executor.listeners.screenshot.*
import com.malinskiy.marathon.android.executor.listeners.profiling.*
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.*
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.*
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.device.screenshot.ScreenCapturer
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.report.attachment.*
import com.malinskiy.marathon.io.*
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import java.io.File
import java.net.InetAddress
import java.time.Clock
import java.time.Duration
fun row(case:String,value:Any?)=println(Gson().toJson(linkedMapOf("case" to case,"value" to value)))
suspend fun seen(prefix:String,count:Int=1){withTimeout(10000){while(File(System.getenv("PROBE_TRANSCRIPT")).readLines().count{it.contains(prefix)}<count){delay(2)}}}
fun main():Unit=runBlocking {
 val mode=System.getenv("PROBE_MODE");val out=File(System.getenv("PROBE_OUTPUT"));val files=FileManager(4096,255,out);val pool=DevicePoolId("pool");val test=TestIdentifier("pkg.clazz","method");val batch=TestBatch(listOf(test.toTest()),"batch")
 val pbtxt=File(out.parentFile,"config.pbtxt").apply{writeText("pid: \${TRACING_TARGET_PID}, pkg: \${TRACING_TARGET_PACKAGE}")}
 val sync=FileSyncConfiguration(pull=mutableSetOf(FileSyncEntry("screenshots")))
 val v=VendorConfiguration.AndroidConfiguration(androidSdk=File("/nonexistent"),applicationOutput=null,testApplicationOutput=null,splitApks=null,extraApplicationsOutput=null,mockLocation=false,allureConfiguration=AllureConfiguration(enabled=false),fileSyncConfiguration=if(mode=="filesync-coverage")sync else FileSyncConfiguration(),timeoutConfiguration=TimeoutConfiguration(screenrecorder=if(mode=="video-timeout")Duration.ofMillis(80)else Duration.ofMinutes(5)))
 val config=Configuration.Builder(name="artifact-probe",outputDir=out).apply{vendorConfiguration=v;isCodeCoverageEnabled=mode=="filesync-coverage"}.build()
 val client=AndroidDebugBridgeClientFactory().apply{host=InetAddress.getLoopbackAddress();port=System.getenv("PROBE_PORT").toInt()}.build()
 val ids=AndroidTestBundleIdentifier();val device=AdamAndroidDevice(client,DeviceStateTracker(),LogcatManager(),Dispatchers.IO,"emulator-5554",config,v,ids,Track(),SystemTimer(Clock.systemUTC()),SerialStrategy.AUTOMATIC)
 device.setup()
 val info=InstrumentationInfo("com.example","com.example.test","Runner")
 val bundle=AndroidTestBundle(null,File("unused.apk"),null,null).apply{javaClass.getDeclaredField("instrumentationInfo\$delegate").apply{isAccessible=true}.set(this,lazyOf(info))};ids.put(test.toTest(),bundle)
 val listener:AndroidTestRunListener=when(mode){
  "video","video-timeout"->ScreenRecorderTestBatchListener(files,pool,"batch",device,VideoConfiguration(),ScreenRecordingPolicy.ON_ANY,this)
  "screenshot"->ScreenCapturerTestRunListener(files,pool,"batch",device,ScreenRecordingPolicy.ON_ANY,ScreenshotConfiguration(delayMs=60000),Duration.ofSeconds(5),this)
  "filesync-coverage"->FileSyncTestRunListener(pool,device,v.fileSyncConfiguration,files)
  else->ProfilingRunListener(files,pool,batch,device,ProfilingConfiguration(enabled=true,pbtxt=pbtxt),ids,this)
 }
 val attachments=mutableListOf<List<String>>()
 if(listener is AttachmentProvider)listener.registerListener(object:AttachmentListener{override fun onAttachment(test:com.malinskiy.marathon.test.Test,attachment:Attachment){attachments.add(listOf(attachment.name?:"",attachment.type.name,attachment.file.name))}})
 listener.beforeTestRun(info);listener.testRunStarted("run",1)
 if(mode!="filesync-coverage"){
  listener.testStarted(test)
  when(mode){
   "video"->seen("shell:screenrecord")
   "video-timeout"->seen("shell:screenrecord",2)
   "screenshot"->{val cap=listener.javaClass.getDeclaredField("screenCapturer").apply{isAccessible=true}.get(listener) as ScreenCapturer;withTimeout(10000){while(cap.capturesTaken==0)delay(2)}}
   else->seen("perfetto --txt")
  }
  listener.testEnded(test,emptyMap())
 }
 listener.afterTestRun();row("scenario:$mode:attachments",attachments)
 row("scenario:$mode:files",out.walkTopDown().filter{it.isFile}.map{listOf(it.relativeTo(out).path.replace(device.toDeviceInfo().safeSerialNumber,"SERIAL"),it.length().toString())}.sortedBy{it[0]}.toList())
 device.dispose();client.close();kotlin.system.exitProcess(0)
}
