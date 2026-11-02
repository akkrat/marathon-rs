// WP 6.1: adam 0.5.8's client, as Kotlin marathon ships it, against a raw byte stub.
// Each case prints one JSON line {"case": ..., "value": ...}; the outputs are fixtures/oracle/adb-client-probes.jsonl.
import com.malinskiy.adam.AndroidDebugBridgeClient
import com.malinskiy.adam.AndroidDebugBridgeClientFactory
import com.malinskiy.adam.request.device.AsyncDeviceMonitorRequest
import com.malinskiy.adam.request.device.ListDevicesRequest
import com.malinskiy.adam.request.misc.GetAdbServerVersionRequest
import com.malinskiy.adam.request.shell.AsyncCompatShellCommandRequest
import com.malinskiy.adam.request.shell.v1.ChanneledShellCommandRequest as V1Channel
import com.malinskiy.adam.request.shell.v1.ShellCommandRequest
import com.malinskiy.adam.request.shell.v2.ChanneledShellCommandRequest as V2Channel
import com.malinskiy.adam.request.shell.v2.ShellCommandInputChunk
import com.malinskiy.adam.request.shell.v2.ShellCommandResultChunk
import com.malinskiy.adam.transport.vertx.VertxSocketFactory
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.SendChannel
import java.io.DataInputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import kotlin.concurrent.thread

fun esc(s: String) = buildString {
    for (c in s) when {
        c == '"' -> append("\\\"")
        c == '\\' -> append("\\\\")
        c < ' ' || c.code > 126 -> append("\\u%04x".format(c.code))
        else -> append(c)
    }
}

fun row(case: String, value: Any?) = println("{\"case\":\"${esc(case)}\",\"value\":\"${esc(value.toString())}\"}")

fun describe(e: Throwable) = "${e.javaClass.name}: ${e.message}"

/** Serves one connection with [script], then closes it. */
fun stub(script: (DataInputStream, OutputStream) -> Unit): Int {
    val server = ServerSocket(0, 1, InetAddress.getLoopbackAddress())
    thread(isDaemon = true) {
        server.accept().use { s ->
            try {
                script(DataInputStream(s.getInputStream()), s.getOutputStream())
            } catch (e: Exception) {
                System.err.println("stub: ${describe(e)}")
            }
        }
        server.close()
    }
    return server.localPort
}

fun DataInputStream.request(): String {
    val len = ByteArray(4).also { readFully(it) }
    val payload = ByteArray(String(len).toInt(16)).also { readFully(it) }
    return String(payload)
}

fun OutputStream.send(bytes: ByteArray) {
    write(bytes); flush()
}

fun OutputStream.send(text: String) = send(text.toByteArray())

fun client(port: Int, factory: VertxSocketFactory = VertxSocketFactory(), name: String = "127.0.0.1"): AndroidDebugBridgeClient =
    AndroidDebugBridgeClientFactory().apply {
        this.port = port
        // As marathon resolves the default adbServers host (`AdamDeviceProvider.kt:93`).
        host = InetAddress.getByName(name)
        socketFactory = factory
    }.build()

/** A scope like the device's (`AdamAndroidDevice.kt:128`): no Job, only a handler, so a failing producer just
 * closes its channel. `CoroutineScope(context)` would add a Job. */
val deviceScope = object : CoroutineScope {
    override val coroutineContext = Dispatchers.IO + CoroutineExceptionHandler { _, _ -> }
}

fun v2Exit(code: Int): Unit = runBlocking {
    val port = stub { input, out ->
        input.request(); out.send("OKAY")
        input.request(); out.send("OKAY")
        out.send(byteArrayOf(3, 1, 0, 0, 0, code.toByte()))
        Thread.sleep(200)
    }
    val updates = client(port).execute(V2Channel("x", Channel<ShellCommandInputChunk>()), deviceScope, "s")
    val codes = mutableListOf<Int?>()
    for (chunk in updates) codes.add(chunk.exitCode)
    row("v2-exit-$code", codes.joinToString(","))
}

fun handshakeEof(): Unit = runBlocking {
    val port = stub { input, out -> input.request(); out.send("OK") }
    try {
        row("handshake-eof", client(port).execute(GetAdbServerVersionRequest()))
    } catch (e: Exception) {
        row("handshake-eof", describe(e))
    }
}

fun trackDevicesEof(): Unit = runBlocking {
    val port = stub { input, out ->
        input.request(); out.send("OKAY")
        out.send("000Eserial\tdevice\n")
        // The producer is waiting for the next message when the server goes away.
        Thread.sleep(300)
    }
    val updates = client(port).execute(AsyncDeviceMonitorRequest(), deviceScope)
    row("track-devices-first", updates.receive().joinToString { "${it.serial}=${it.state}" })
    val next = updates.receiveCatching()
    row("track-devices-eof", next.exceptionOrNull()?.let(::describe) ?: "closed=${next.isClosed} value=${next.getOrNull()}")
}

fun devicesLineWithoutTab(): Unit = runBlocking {
    val port = stub { input, out -> input.request(); out.send("OKAY"); out.send("0010a\tdevice\nno-tab\n") }
    try {
        row("devices-line-without-tab", client(port).execute(ListDevicesRequest()))
    } catch (e: Exception) {
        row("devices-line-without-tab", describe(e))
    }
}

fun idle(delayMs: Long): Unit = runBlocking {
    val port = stub { input, out ->
        input.request(); out.send("OKAY")
        input.request(); out.send("OKAY")
        Thread.sleep(delayMs)
        out.send("late\nx0")
    }
    val started = System.nanoTime()
    val outcome = try {
        val r = client(port, VertxSocketFactory(idleTimeout = 1)).execute(ShellCommandRequest("x"), "s")
        "output=${r.output} exit=${r.exitCode}"
    } catch (e: Exception) {
        describe(e)
    }
    val seconds = (System.nanoTime() - started) / 100_000_000 / 10.0
    // Elapsed time in tenths of a second, floored: the idle timer fires at about 1 s, not 1 ms.
    row("idle-1ms-delay-$delayMs", "$outcome after>=${seconds}s".replace(Regex(">=1\\.[0-3]s"), ">=1.0s"))
}

fun v1Split(): Unit = runBlocking {
    val port = stub { input, out ->
        input.request(); out.send("OKAY")
        input.request(); out.send("OKAY")
        val bytes = "aжb".toByteArray()
        out.send(bytes.copyOfRange(0, 2))
        Thread.sleep(200)
        out.send(bytes.copyOfRange(2, bytes.size))
    }
    val updates = client(port).execute(V1Channel("x"), deviceScope, "s")
    val parts = mutableListOf<String>()
    for (part in updates) parts.add(part)
    row("v1-split-utf8", parts.joinToString("|"))
}

fun oversized() {
    val bytes = ShellCommandRequest("y".repeat(0x10000)).serialize()
    row("oversized-header", String(bytes, 0, 6))
    row("oversized-length", bytes.size)
}

fun closed(): Unit = runBlocking {
    val trackPort = stub { input, out ->
        input.request(); out.send("OKAY")
        out.send("000Eserial\tdevice\n")
        Thread.sleep(2000)
    }
    val logcatPort = stub { input, out ->
        input.request(); out.send("OKAY")
        input.request(); out.send("OKAY")
        out.send("line\n")
        Thread.sleep(2000)
    }
    val factory = VertxSocketFactory()
    val track = client(trackPort, factory).execute(AsyncDeviceMonitorRequest(), deviceScope)
    track.receive()
    val logcat = client(logcatPort, factory).execute(V1Channel("logcat"), deviceScope, "s")
    logcat.receive()
    client(trackPort, factory).close()
    val t = track.receiveCatching()
    row("close-track-devices", t.exceptionOrNull()?.let(::describe) ?: "closed=${t.isClosed}")
    val l = logcat.receiveCatching()
    row("close-v1-stream", l.exceptionOrNull()?.let(::describe) ?: "closed=${l.isClosed} value=${l.getOrNull()}")
    try {
        row("close-new-request", client(trackPort, factory).execute(GetAdbServerVersionRequest()))
    } catch (e: Throwable) {
        // Not an Exception: marathon's `catch (e: Exception)` blocks let it through.
        row("close-new-request", describe(e))
        row("close-new-request-is-Exception", e is Exception)
    }
}

fun connectRefused(): Unit = runBlocking {
    val server = ServerSocket(0, 1, InetAddress.getLoopbackAddress())
    val port = server.localPort
    server.close()
    try {
        client(port).execute(GetAdbServerVersionRequest())
    } catch (e: Exception) {
        row("connect-refused", describe(e).replace(":$port", ":<port>"))
        row("connect-refused-is-ConnectException", e is java.net.ConnectException)
        row("connect-refused-cause", e.cause?.let(::describe))
    }
    // A DNS name is kept beside the address it resolved to (R1-wonder-3).
    try {
        client(port, name = "localhost").execute(GetAdbServerVersionRequest())
    } catch (e: Exception) {
        row("connect-refused-named", describe(e).replace(":$port", ":<port>"))
    }
}

// Review round 2 (R1-sol-1 to -7).

/** A 32 KiB stdout packet of which only the first 16 KiB piece has arrived. */
fun v2PartialPacket(): Unit = runBlocking {
    val port = stub { input, out ->
        input.request(); out.send("OKAY")
        input.request(); out.send("OKAY")
        out.send(byteArrayOf(1, 0, 128.toByte(), 0, 0)); out.send(ByteArray(16384) { 120 })
        Thread.sleep(500)
        out.send(ByteArray(16384) { 121 }); out.send(byteArrayOf(3, 1, 0, 0, 0, 0))
    }
    val updates = client(port).execute(V2Channel("x", Channel<ShellCommandInputChunk>()), deviceScope, "s")
    row("v2-partial-packet", withTimeoutOrNull(300) { updates.receive().stdout?.size })
    val rest = mutableListOf<String>()
    for (chunk in updates) rest.add(chunk.stdout?.size?.toString() ?: "exit=${chunk.exitCode}")
    row("v2-partial-packet-rest", rest.joinToString(","))
}

/** A write the server never reads, with a one-"millisecond" (one-second, N12) idle timeout. */
fun idleBlockedWrite(): Unit = runBlocking {
    val port = stub { _, _ -> Thread.sleep(3000) }
    val factory = VertxSocketFactory(idleTimeout = 1)
    val socket = factory.tcp(InetSocketAddress(InetAddress.getLoopbackAddress(), port))
    val outcome = withTimeoutOrNull(2000) {
        try {
            socket.writeFully(ByteArray(32 shl 20), 0, 32 shl 20); "ok"
        } catch (e: Throwable) {
            describe(e)
        }
    }
    row("idle-blocked-write", outcome)
    factory.close()
}

/** What the server sees of an idle connection when the factory closes: -1 is EOF. */
fun closePeerEof(): Unit = runBlocking {
    val eof = CompletableDeferred<Int>()
    val port = stub { input, _ -> eof.complete(input.read()) }
    val factory = VertxSocketFactory(idleTimeout = 0)
    factory.tcp(InetSocketAddress(InetAddress.getLoopbackAddress(), port))
    factory.close()
    row("close-peer-eof", withTimeoutOrNull(1000) { eof.await() })
}

fun closeBeforeFirstRequest(): Unit = runBlocking {
    val port = stub { input, out -> input.request(); out.send("OKAY00040029") }
    val c = client(port)
    c.close()
    row("close-before-first-request", try {
        c.execute(GetAdbServerVersionRequest())
    } catch (e: Throwable) {
        describe(e)
    })
    c.close()
}

/** A compat request whose close sends one more item after 400 ms; the first receive gives up after 150 ms. */
fun compatCloseCancelled(): Unit = runBlocking {
    val port = stub { input, out -> input.request(); out.send("OKAY") }
    val c = client(port)
    val request = object : AsyncCompatShellCommandRequest<Int>("x", emptyList(), coroutineScope = deviceScope) {
        override suspend fun convertChunk(response: ShellCommandResultChunk): Int? = null
        override suspend fun close(channel: SendChannel<Int>) {
            delay(400); channel.send(7)
        }
    }
    val updates = c.execute(request)
    row("compat-close-cancelled-first", withTimeoutOrNull(150) { updates.receive() })
    row("compat-close-cancelled-second", withTimeoutOrNull(1000) { updates.receive() })
    c.close()
}

fun connectRefusedIpv6(): Unit = runBlocking {
    val server = ServerSocket(0, 1, InetAddress.getByName("::1"))
    val port = server.localPort
    server.close()
    try {
        client(port, name = "::1").execute(GetAdbServerVersionRequest())
    } catch (e: Exception) {
        row("connect-refused-ipv6", describe(e).replace(":$port", ":<port>"))
    }
}

/** `InetSocketAddress.toString()`, as netty puts it in connect errors; replayed by `jcompat::net`. */
fun inetSocketAddress() {
    val v6 = InetAddress.getByName("::1").address
    val addresses = listOf(
        InetAddress.getByName("127.0.0.1"),
        InetAddress.getByName("::1"),
        InetAddress.getByName("2001:db8::a:0:1"),
        InetAddress.getByName("fe80::1"),
        InetAddress.getByName("::ffff:10.0.0.1"),
        InetAddress.getByName("::"),
        InetAddress.getByAddress("adb.example", v6),
        InetAddress.getByAddress("adb.example", byteArrayOf(10, 0, 0, 2)),
    )
    row("inet-socket-address", addresses.joinToString(" | ") { InetSocketAddress(it, 5037).toString() })
}

fun main() {
    val cases = listOf<Pair<String, () -> Unit>>(
        "v2-exit-255" to { v2Exit(255) },
        "v2-exit-128" to { v2Exit(128) },
        "handshake-eof" to ::handshakeEof,
        "track-devices" to ::trackDevicesEof,
        "devices-line-without-tab" to ::devicesLineWithoutTab,
        "idle-200" to { idle(200) },
        "idle-1500" to { idle(1500) },
        "v1-split-utf8" to ::v1Split,
        "oversized" to ::oversized,
        "close" to ::closed,
        "connect-refused" to ::connectRefused,
        "v2-partial-packet" to ::v2PartialPacket,
        "idle-blocked-write" to ::idleBlockedWrite,
        "close-peer-eof" to ::closePeerEof,
        "close-before-first-request" to ::closeBeforeFirstRequest,
        "compat-close-cancelled" to ::compatCloseCancelled,
        "connect-refused-ipv6" to ::connectRefusedIpv6,
        "inet-socket-address" to ::inetSocketAddress,
    )
    for ((name, case) in cases) {
        try {
            case()
        } catch (e: Throwable) {
            row("$name-probe-failed", describe(e))
        }
    }
    System.out.flush()
    // vert.x event loops are not daemon threads.
    Runtime.getRuntime().halt(0)
}
