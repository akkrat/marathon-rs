package oracle.hooks

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.time.Clock

class HookRuntimeTest {
    @TempDir lateinit var dir: File

    @Test fun runtime_clocks_draws_ids_and_exhaustion() {
        val tape = File(dir, "tape.json")
        tape.writeText("""{"format":"marathon-replay/tape@1",
          "clocks":{"wall":{"mode":"frozen","start":"2020-01-01T00:00:00Z"},"monotonic":{"mode":"scheduler","startNanos":7},"scheduler":{"mode":"virtual"}},
          "reads":{"specific":[10,20]},"draws":{"int":[-4],"pick":["b"]},"ids":{"temp-file":{"pattern":"%d"},"temp-dir":{"pattern":"%d"},"identity":{"pattern":"%08x"},"id":{"values":["00000000-0000-0000-0000-000000000001"]}}}
        """)
        val process = ProcessBuilder(File(System.getProperty("java.home"), "bin/java").path,
            "-Doracle.hooks.tape=${tape.path}", "-Djava.io.tmpdir=${dir.path}", "-cp", System.getProperty("oracle.test.classpath"),
            "oracle.hooks.HookRuntimeProbe").redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        assertEquals(0, process.waitFor(), output)
    }
}

data class PrivateConfiguration(private val bundle: String)

object HookRuntimeProbe {
    @JvmStatic fun main(args: Array<String>) {
        var scheduler = 0L
        OracleHooks.installReplayLoop(object : OracleHooks.ReplayLoop {
            override fun <T> run(label: String, block: suspend CoroutineScope.() -> T): T = kotlinx.coroutines.runBlocking(block = block)
        }) { scheduler }
        check(OracleHooks.currentTimeMillis("wall") == 1577836800000L)
        check(OracleHooks.nanoTime("mono") == 7L)
        scheduler = 4
        check(OracleHooks.nanoTime("mono") == 4000007L)
        check(OracleHooks.clock("wall", Clock.systemUTC()).millis() == 1577836800000L)
        check(OracleHooks.currentTimeMillis("specific") == 10L)
        check(OracleHooks.currentTimeMillis("specific") == 20L)
        check(OracleHooks.nextInt("int") == -4)
        check(OracleHooks.pick("pick", listOf("a", "b")) { it } == "b")
        check(OracleHooks.uuid("id") == "00000000-0000-0000-0000-000000000001")
        check(OracleHooks.runBlocking("run") { 42 } == 42)
        check(OracleHooks.identities("identity", PrivateConfiguration("literal@deadbeef")) == "PrivateConfiguration(bundle=literal@deadbeef)")
        val temp = OracleHooks.createTempFile("temp-file", "private-", ".tmp")
        val directory = OracleHooks.createTempDirectory("temp-dir", "private-dir-")
        check(java.nio.file.attribute.PosixFilePermissions.toString(java.nio.file.Files.getPosixFilePermissions(temp)) == "rw-------")
        check(java.nio.file.attribute.PosixFilePermissions.toString(java.nio.file.Files.getPosixFilePermissions(directory)) == "rwx------")
        val literal = "contact.foo@deadbeef"
        check(OracleHooks.identities("identity", literal) == literal)
        val array = arrayOf("value")
        val rendered = OracleHooks.identities("identity", array)
        check(rendered.startsWith("[Ljava.lang.String;@"))
        check(OracleHooks.identities("identity", array) == rendered)
        check(OracleHooks.identities("identity", array.toString()) == array.toString())
        check(OracleHooks.identities("identity", listOf(literal)) == "[$literal]")
        check(OracleHooks.identities("identity", IdentityConfig(literal)) == "IdentityConfig(contact=$literal)")
        check(OracleHooks.identities("identity", RedactedConfig("secret")) == "<redacted>")
        kotlinx.coroutines.runBlocking {
            val gate = Barriers(listOf("site:a", "site:b"))
            try { gate.event("site", "unknown") {} } catch (_: OracleTapeException) {}
            gate.event("site", "a") {}
            gate.event("site", "b") {}
            try { gate.event("site", "b") {} } catch (_: OracleTapeException) {}
            val duplicate = Barriers(listOf("site:first", "site:later"))
            val waiting = launch { duplicate.event("site", "later") {} }
            kotlinx.coroutines.yield()
            try { duplicate.event("site", "later") {} } catch (_: OracleTapeException) {}
            waiting.cancel()
        }
        val recorded = com.google.gson.JsonParser.parseString(OracleHooks.report()).asJsonObject.getAsJsonArray("failures")
        check(recorded.size() == 3) { recorded }

        val channel = kotlinx.coroutines.channels.Channel<Int>(1)
        OracleHooks.mailbox("mailbox", "probe", channel)
        channel.trySend(1)
        check(OracleHooks.busyMailboxes().isNotEmpty())
        channel.tryReceive()
        check(OracleHooks.busyMailboxes().isEmpty())
        channel.close()
        check(OracleHooks.busyMailboxes().isEmpty())
        for (call in listOf<() -> Unit>({ OracleHooks.nextInt("int") }, { OracleHooks.uuid("missing") },
            { OracleHooks.currentTimeMillis("specific") })) {
            var failed = false
            try { call() } catch (_: OracleTapeException) { failed = true }
            check(failed)
        }
        check(OracleHooks.report().contains("\"unused\":{}"))
    }
}

data class IdentityConfig(val contact: String)

data class RedactedConfig(val secret: String) { override fun toString(): String = "<redacted>" }
