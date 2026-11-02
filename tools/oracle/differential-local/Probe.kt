import com.google.gson.Gson
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessCommandExecutor
import com.malinskiy.marathon.apple.cmd.local.JvmFileBridge
import java.io.File
import java.time.Duration
import kotlinx.coroutines.runBlocking

fun main(args: Array<String>) = runBlocking {
    val root = File(args.single()).canonicalFile
    check(root.mkdir())
    val tree = File(root, "tree").apply { mkdir() }
    File(tree, "empty").mkdir()
    File(tree, "source").writeBytes(byteArrayOf(0, 1, -1, 10))
    val executor = KotlinProcessCommandExecutor()
    val argv = listOf("/bin/sh", "-c", "printf '%s\\n' \"\$MRS_VALUE\" \"\$PWD\" \"\$@\"; printf 'stderr\\n' >&2; exit 7", "probe", "\"'x'\"", "'", "\"", "''", "x'", "")
    val session = executor.execute(argv, Duration.ofSeconds(5), Duration.ofSeconds(5),
        mapOf("MRS_VALUE" to "literal 12345678-1234-1234-1234-123456789abc"), root.path)
    val result = try { session.await() } finally { session.close() }
    val bridge = JvmFileBridge()
    val sent = bridge.send(File(tree, "source"), File(tree, "copy").path)
    val receive = File(tree, "receive").apply { mkdir() }
    val received = bridge.receive(File(tree, "source").path, receive)
    val missing = bridge.receive(File(tree, "absent").path, File(tree, "missing-copy"))
    val observation = linkedMapOf<String, Any?>("argv" to argv, "stdout" to result.stdout,
        "stderr" to result.stderr, "exit" to result.exitCode, "send" to sent, "receive" to received,
        "missing" to missing, "connected" to executor.connected)
    executor.close()
    check(result.exitCode == 7 && sent && received && !missing)
    File(root, "observation.json").writeText(Gson().toJson(observation))
    File(root, "identities.json").writeText(Gson().toJson(mapOf("sandbox" to root.path)))
}
