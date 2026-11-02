// Actual remote parser against a strict loopback fake. Original classes are unchanged.
import com.google.gson.Gson
import com.malinskiy.marathon.android.adam.*
import com.malinskiy.marathon.android.AndroidTestBundleIdentifier
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.*
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import java.io.File
import java.time.Clock
fun row(case:String,value:Any?)=println(Gson().toJson(linkedMapOf("case" to case,"value" to value)))
fun listeners(device:AdamAndroidDevice)=(device.javaClass.getDeclaredField("logcatListeners").apply{isAccessible=true}.get(device) as Collection<*>).size
fun main():Unit=runBlocking {
 val mode=System.getenv("PROBE_MODE")
 val file=File(System.getenv("PROBE_APK"))
 val v=VendorConfiguration.AndroidConfiguration(androidSdk=File("/nonexistent"),applicationOutput=null,testApplicationOutput=file,splitApks=null,extraApplicationsOutput=null,mockLocation=false,autoGrantPermission=false,allureConfiguration=AllureConfiguration(enabled=false),testParserConfiguration=TestParserConfiguration.RemoteTestParserConfiguration(instrumentationArgs=linkedMapOf("listener" to "com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer","other" to "value")),adbServers=listOf(AdbEndpoint("127.0.0.1",System.getenv("PROBE_PORT").toInt())))
 val config=Configuration.Builder(name="remote-probe",outputDir=File(System.getProperty("java.io.tmpdir"),"out")).apply{vendorConfiguration=v;testOutputTimeoutMillis=100}.build()
 val identifier=AndroidTestBundleIdentifier()
 val provider=AdamDeviceProvider(config,identifier,v,Track(),SystemTimer(Clock.systemUTC()))
 provider.initialize();val device=withTimeout(5000){provider.borrow()} as AdamAndroidDevice
 val parser=AmInstrumentTestParser(config,identifier,v)
 val logs=ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply{start()}
 (org.slf4j.LoggerFactory.getLogger("com.malinskiy.marathon.android.adam.AmInstrumentTestParser") as ch.qos.logback.classic.Logger).addAppender(logs)
 if(mode=="cancel") {
  val task=async { parser.extract(device) }
  while(listeners(device)==0){yield()}
  task.cancelAndJoin();row("remote:cancel",task.isCancelled)
 } else {
  try{val tests=parser.extract(device);row("remote:$mode:tests",tests.map{listOf(it.pkg,it.clazz,it.method,it.metaProperties.map{it.name})})}
  catch(e:Throwable){row("remote:$mode:error",listOf(e.javaClass.name,e.message?.replace(device.serialNumber,"SERIAL"),e.cause?.javaClass?.name))}
 }
 row("remote:$mode:listeners",listeners(device))
 row("remote:$mode:warnings",logs.list.filter{it.level==ch.qos.logback.classic.Level.WARN}.map{it.formattedMessage.replace(file.absolutePath,"APK")})
 device.dispose();provider.terminate();kotlin.system.exitProcess(0)
}
