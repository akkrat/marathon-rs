// WP 6.5: actual Kotlin provider/device/installer on strict fake-adb servers.
import com.malinskiy.marathon.android.adam.AdamDeviceProvider
import com.malinskiy.marathon.android.adam.AdamAndroidDevice
import com.malinskiy.marathon.android.AndroidTestBundleIdentifier
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.AdbEndpoint
import com.malinskiy.marathon.config.vendor.android.AllureConfiguration
import com.malinskiy.marathon.device.DeviceProvider
import com.malinskiy.marathon.time.SystemTimer
import kotlinx.coroutines.*
import java.io.File
fun main(): Unit {
try { runBlocking {
    val install = System.getenv("PROBE_MODE") == "install"
    val fixtures = File(System.getenv("PROBE_FIXTURES"))
    val vendor = VendorConfiguration.AndroidConfiguration(
        androidSdk = File("/nonexistent"),
        applicationOutput = if (install) File(fixtures, "app-debug.apk") else null,
        testApplicationOutput = if (install) File(fixtures, "app-debug-androidTest.apk") else null,
        splitApks = null, extraApplicationsOutput = null,
        autoGrantPermission = true, installOptions = "-custom", mockLocation = false,
        allureConfiguration = AllureConfiguration(enabled = false),
        adbServers = System.getenv("PROBE_PORTS").split(',').map { AdbEndpoint("127.0.0.1", it.toInt()) }
    )
    val config = Configuration.Builder(name = "android-probe", outputDir = File("/tmp/android-probe-output")).apply { vendorConfiguration = vendor }.build()
    val provider = AdamDeviceProvider(config, AndroidTestBundleIdentifier(), vendor, Track(), SystemTimer(java.time.Clock.systemUTC()))
    provider.initialize()
    val devices = mutableListOf<AdamAndroidDevice>()
    withTimeout(15000) {
        while (devices.size < 2) {
            val event = provider.subscribe().receive()
            check(event is DeviceProvider.DeviceEvent.DeviceConnected)
            devices.add(event.device as AdamAndroidDevice)
        }
    }
    if (install) {
        withTimeout(10000) { for (device in devices.sortedBy { it.serialNumber }) device.prepare(config) }
        println("{\"case\":\"install-matrix\",\"value\":\"two prepared devices\"}")
    } else {
        val event = withTimeout(15000) { provider.subscribe().receive() }
        check(event is DeviceProvider.DeviceEvent.DeviceDisconnected)
        println("{\"case\":\"multi-server-disconnect\",\"value\":\"connected=2|disconnected=1|healthy=${event.device.healthy}|network=${event.device.networkState}\"}")
    }
    devices.forEach { it.dispose() }
    provider.terminate()
    kotlin.system.exitProcess(0)
}

} catch (e: Throwable) { e.printStackTrace(); kotlin.system.exitProcess(1) }
}
