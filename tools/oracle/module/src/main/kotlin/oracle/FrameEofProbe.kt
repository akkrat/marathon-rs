package oracle

import io.ktor.network.selector.ActorSelectorManager
import io.ktor.network.sockets.InetSocketAddress
import io.ktor.network.sockets.aSocket
import io.ktor.network.sockets.isClosed
import io.ktor.network.sockets.openReadChannel
import io.ktor.utils.io.readFully
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.nio.ByteBuffer
import kotlin.concurrent.thread
import kotlin.math.ceil
import kotlin.math.log2
import kotlin.math.pow
import kotlin.math.roundToInt

/**
 * WP 2.3b frame decoder evidence over a real loopback socket: a Java
 * `ServerSocket` writes the case bytes and closes, and the client side runs
 * the read half of `AppleSimulatorProvider.readSocket` (ktor
 * `aSocket(...).tcp().connect`, `readFully` of the 4 reserved bytes,
 * `readInt`, the 4096-byte reused buffer grown by powers of two, `readFully`
 * of the payload) until it throws. Decoding and binding are left out: they
 * are covered by the devices corpus. Each row records the frames read before
 * the failure and the exception class/message the provider's `catch` sees.
 */
object FrameEofProbe {
    fun generate(outDir: File) {
        val table = JsonlTable(
            "frame-eof-probe",
            "WP 2.3b: AppleSimulatorProvider.readSocket over a real ktor loopback socket closed by the peer after the case bytes",
            linkedMapOf(
                "id" to "case name",
                "bytes" to "hex of what the peer writes before closing",
                "result" to "{frames: [payload lengths read], error: {class, message, causes}}",
            ),
        )
        val selector = ActorSelectorManager(Dispatchers.IO)
        try {
            for ((id, bytes) in cases) {
                table.add(linkedMapOf("id" to id, "bytes" to hex(bytes), "result" to read(selector, bytes)))
            }
        } finally {
            selector.close()
        }
        table.writeTo(outDir)
    }

    private fun read(selector: ActorSelectorManager, bytes: ByteArray): Map<String, Any?> {
        val server = ServerSocket(0, 1, InetAddress.getLoopbackAddress())
        val sender = thread {
            server.accept().use { client ->
                client.getOutputStream().apply { write(bytes); flush() }
            }
        }
        val frames = mutableListOf<Int>()
        val error = try {
            runBlocking {
                aSocket(selector).tcp().connect(InetSocketAddress("127.0.0.1", server.localPort)).use { socket ->
                    // As in startDynamicProvider: one buffer, reused across frames.
                    var buffer = ByteBuffer.allocate(4096)
                    val readChannel = socket.openReadChannel()
                    // Wait for the peer's close so every case sees the whole stream.
                    sender.join()
                    while (!socket.isClosed) {
                        val reservedBytes = ByteArray(4)
                        readChannel.readFully(reservedBytes, 0, 4)
                        val length = readChannel.readInt()
                        buffer.clear()
                        if (length <= buffer.capacity()) {
                            buffer.limit(length)
                        } else {
                            val newCapacity = ceil(log2(length.toDouble())).roundToInt()
                            if (newCapacity in 1..20) {
                                buffer = ByteBuffer.allocate(2.0.pow(newCapacity.toDouble()).toInt())
                                buffer.limit(length)
                            } else {
                                throw RuntimeException("Device provider update too long: maximum is 2^20 bytes")
                            }
                        }
                        readChannel.readFully(buffer)
                        frames.add(length)
                    }
                }
            }
            null
        } catch (t: Throwable) {
            errorJson(t)
        } finally {
            sender.join()
            server.close()
        }
        return linkedMapOf("frames" to frames, "error" to error)
    }

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    // Payload bytes are a valid Marathondevices document padded with spaces,
    // so the Rust decoder can bind the complete frames these rows read.
    private fun frame(length: Int, payload: Int = length): ByteArray =
        byteArrayOf(9, 9, 9, 9) + ByteBuffer.allocate(4).putInt(length).array() +
            "workers: []".padEnd(payload).take(payload).toByteArray()

    private val cases: List<Pair<String, ByteArray>> = listOf(
        "boundary-empty" to byteArrayOf(),
        "reserved-1" to byteArrayOf(0),
        "reserved-3" to byteArrayOf(0, 0, 0),
        "length-0" to byteArrayOf(0, 0, 0, 0),
        "length-1" to byteArrayOf(0, 0, 0, 0, 0),
        "length-3" to byteArrayOf(0, 0, 0, 0, 0, 0, 0),
        "payload-10-got-0" to frame(10, 0),
        "payload-10-got-2" to frame(10, 2),
        "payload-10-got-9" to frame(10, 9),
        "payload-5000-got-100" to frame(5000, 100),
        "frame-then-boundary" to frame(11),
        "frame-then-reserved-2" to frame(11) + byteArrayOf(0, 0),
        "two-frames-then-length-2" to frame(11) + frame(5000) + byteArrayOf(0, 0, 0, 0, 0, 0),
        "zero-length-then-boundary" to frame(0),
        "negative-length" to frame(-1, 0),
        "too-long" to frame((1 shl 20) + 1, 0),
    )
}
