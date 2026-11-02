// WP 6.5: actual sdklib and provider against a loopback byte stub; never invokes adb.
import com.android.sdklib.AndroidVersion
import com.malinskiy.marathon.android.adam.AdamDeviceProvider
import com.malinskiy.marathon.android.AndroidTestBundleIdentifier
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.AdbEndpoint
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread
fun row(case: String, value: Any?) = println("{\"case\":\"$case\",\"value\":\"$value\"}")
fun main(): Unit = runBlocking {
    for ((sdk, code) in listOf(1 to "REL", 18 to "Preview", 33 to "REL", 33 to "UpsideDownCake", 34 to "", 34 to " REL ")) {
        val v = AndroidVersion(sdk, code)
        row("version:$sdk:$code", "${v.apiLevel}|${v.codename}|${v.featureLevel}|${v.apiStringWithoutExtension}|${v.isGreaterOrEqualThan(19)}")
    }
    row("logcat-chunk-lines", listOf("hel", "lo\n").flatMap { it.lines() }.joinToString("|"))
    val d = AndroidVersion.DEFAULT
    row("version:default", "${d.apiLevel}|${d.codename}|${d.featureLevel}|${d.apiStringWithoutExtension}")
    for (host in listOf("127.0.0.1", "::1", "2001:db8::a:0:1", "::ffff:10.0.0.1", "localhost")) row("address:$host", InetAddress.getByName(host).hostAddress)
    val server = ServerSocket(0, 10, InetAddress.getByName("127.0.0.1"))
    val monitors = AtomicInteger()
    thread(isDaemon = true) {
        while (!server.isClosed) {
            val socket = try { server.accept() } catch (e: Exception) { break }
            thread(isDaemon = true) { socket.use {
                val input = it.getInputStream()
                val len = String(input.readNBytes(4)).toInt(16)
                val cmd = String(input.readNBytes(len))
                val out = it.getOutputStream()
                fun send(body: String) { out.write(("OKAY" + "%04x".format(body.length) + body).toByteArray()); out.flush() }
                when (cmd) {
                    "host:version" -> send("0029")
                    "host:devices" -> send("offline-serial\toffline\n")
                    "host:track-devices" -> { monitors.incrementAndGet(); send("offline-serial\toffline\n"); Thread.sleep(100) }
                    else -> error(cmd)
                }
            } }
        }
    }
    val vendor = VendorConfiguration.AndroidConfiguration(androidSdk = File("/nonexistent"), applicationOutput = null, testApplicationOutput = null, splitApks = null, extraApplicationsOutput = null, adbServers = listOf(AdbEndpoint("127.0.0.1", server.localPort)))
    val config = Configuration.Builder(name = "probe", outputDir = File("/tmp/android-provider-probe")).apply { vendorConfiguration = vendor }.build()
    val provider = AdamDeviceProvider(config, AndroidTestBundleIdentifier(), vendor, Track(), SystemTimer(java.time.Clock.systemUTC()))
    provider.initialize()
    val job = provider.javaClass.getDeclaredField("providerJob").apply { isAccessible = true }.get(provider) as Job
    withTimeout(5000) { job.join() }
    delay(150)
    row("track-eof", "completed=${job.isCompleted}|cancelled=${job.isCancelled}|subscriptions=${monitors.get()}")
    provider.terminate()
    server.close()
    kotlin.system.exitProcess(0)
}
