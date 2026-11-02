// Review round 1 cases (T15 variants, T21-T25) and the normalised outcome words shared with spikes/ssh-russh
import com.malinskiy.marathon.apple.cmd.CommandResult
import com.malinskiy.marathon.apple.cmd.CommandSession
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutor
import com.malinskiy.marathon.apple.extensions.Durations
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import net.schmizz.sshj.common.DisconnectReason
import net.schmizz.sshj.common.SSHException
import net.schmizz.sshj.connection.ConnectionException
import net.schmizz.sshj.transport.TransportException
import java.time.Duration
import java.util.concurrent.TimeoutException

private fun now() = System.nanoTime() / 1_000_000

private fun chain(e: Throwable) = generateSequence(e) { it.cause?.takeIf { next -> next !== it } }.take(16).toList()

/** The error kinds both harnesses print, so the comparison does not depend on class names */
fun kind(e: Throwable): String {
    val causes = chain(e)
    return when {
        causes.any { it is TimeoutException && it.message?.startsWith("idle timeout") == true } -> "idle timeout"
        causes.any { it is TimeoutException } -> "close timeout"
        causes.any { it is TransportException || (it is SSHException && it.disconnectReason == DisconnectReason.CONNECTION_LOST) } ->
            "connection lost"
        causes.any { it is ConnectionException } -> "exec rejected"
        causes.any { it is IllegalStateException && it.message == "Not connected" } -> "not connected"
        else -> "other"
    }
}

/** Runs [block] on its own thread and reports how it ended within [capMs]: returned, failed, or still waiting */
fun outcomeOf(capMs: Long, block: () -> Unit): Pair<String, Long> {
    var error: Throwable? = null
    val start = now()
    val thread = Thread { try { block() } catch (e: Throwable) { error = e } }.apply { isDaemon = true; start() }
    thread.join(capMs)
    val outcome = when {
        thread.isAlive -> "still waiting after ${capMs / 1000} s"
        error == null -> "returned"
        else -> "failed: ${kind(error!!)}"
    }
    return outcome to (now() - start)
}

fun awaitOutcome(session: CommandSession, capMs: Long): Pair<String, Long> {
    var result: CommandResult? = null
    val (outcome, ms) = outcomeOf(capMs) { result = runBlocking { session.await() } }
    return (if (outcome == "returned") result!!.show() else outcome) to ms
}

// T15: close() of an open session against `serve --hang-on-close`
suspend fun t15(executor: SshjCommandExecutor) {
    val session = executor.execute(listOf("sleep", "30"), Duration.ofMinutes(1), Durations.INFINITE, emptyMap(), null)
    delay(500)
    val (outcome, ms) = outcomeOf(90_000) { session.close() }
    out("T15.close", outcome)
    out("T15.info.close_ms", ms)
}

// T15h: the command timeout fires against `serve --hang-on-close`; then the caller closes the session
suspend fun t15h(executor: SshjCommandExecutor) {
    val session = executor.execute(listOf("sleep", "30"), Duration.ofSeconds(1), Durations.INFINITE, emptyMap(), null)
    val (outcome, ms) = awaitOutcome(session, 90_000)
    out("T15h.await", outcome)
    out("T15h.info.await_ms", ms)
    val (closeOutcome, closeMs) = outcomeOf(60_000) { session.close() }
    out("T15h.close_after", closeOutcome)
    out("T15h.info.close_after_ms", closeMs)
}

// T15t: the command timeout fires on a process that ignores TERM (real sshd)
suspend fun t15t(executor: SshjCommandExecutor) {
    val command = listOf("sh", "-c", "trap \"\" TERM; sleep 8; echo survived")
    val session = executor.execute(command, Duration.ofSeconds(1), Durations.INFINITE, emptyMap(), null)
    val (outcome, ms) = awaitOutcome(session, 90_000)
    out("T15t.await", outcome)
    out("T15t.info.await_ms", ms)
    out("T15t.close_after", outcomeOf(60_000) { session.close() }.first)
    out("T15t.after", executor.run(listOf("echo", "hello")).show())
}

// T21: stream boundaries, the shared idle clock and backpressure
suspend fun t21(executor: SshjCommandExecutor) {
    for ((label, chunks) in listOf(
        "utf8_split" to listOf(byteArrayOf(0xe2.toByte()), byteArrayOf(0x82.toByte(), 0xac.toByte(), 10)),
        "crlf_split" to listOf("left\r".toByteArray(), "\nright\n".toByteArray())
    )) {
        val lines = mutableListOf<String>()
        val buffer = com.malinskiy.marathon.apple.cmd.LineBuffer(Charsets.UTF_8) { lines.add(it) }
        for (chunk in chunks) { buffer.append(chunk); buffer.flush() }
        buffer.drain()
        out("T21.$label", lines)
    }

    runCatching { executor.run(listOf("sh", "-c", "yes ä漢字€ | head -n 100000")) }.fold({ r ->
        out("T21.utf8", "exit=${r.exitCode} lines=${r.stdout.size}")
        out("T21.info.utf8_corrupted_lines", r.stdout.count { it != "ä漢字€" })
    }, { out("T21.utf8", "failed: ${kind(it)}") })
    runCatching { executor.run(listOf("awk", "BEGIN{for(i=0;i<100000;i++) printf \"l%d\\r\\n\", i}")) }.fold({ r ->
        val ordered = r.stdout.withIndex().all { (i, l) -> l == "l$i" }
        out("T21.crlf", "exit=${r.exitCode} lines=${r.stdout.size} empty=${r.stdout.count { it.isEmpty() }} ordered=$ordered")
    }, { out("T21.crlf", "failed: ${kind(it)}") })
    val asymmetric = listOf("sh", "-c", "for i in 1 2 3 4 5 6; do echo e\$i 1>&2; sleep 0.5; done; echo out")
    out("T21.asymmetric_idle", runCatching { executor.run(asymmetric, Duration.ofSeconds(30), Duration.ofSeconds(1)) }
        .fold({ it.show() }, { "failed: ${kind(it)}" }))
    // Nobody reads for 5 s: the producer blocks once the buffers and the SSH window are full
    val command = listOf("sh", "-c", "s=\$(date +%s); seq 1 1000000; echo blocked_s=\$(( \$(date +%s) - s ))")
    val session = executor.execute(command, Duration.ofSeconds(60), Durations.INFINITE, emptyMap(), null)
    delay(5000)
    runCatching { session.await() }.fold({ r ->
        val blocked = r.stdout.lastOrNull()?.removePrefix("blocked_s=")?.toIntOrNull() ?: 0
        out("T21.backpressure", "exit=${r.exitCode} lines=${r.stdout.size} producer_blocked=${blocked >= 4}")
        out("T21.info.backpressure_blocked_s", blocked)
    }, { out("T21.backpressure", "failed: ${kind(it)}") })
}

// T22: cancelling the exit wait, and the isAlive snapshot
suspend fun t22(executor: SshjCommandExecutor) {
    val session = executor.execute(listOf("sh", "-c", "sleep 3.3; echo late"), Duration.ofSeconds(30), Durations.INFINITE, emptyMap(), null)
    out("T22.await_cancelled", withTimeoutOrNull(1000) { session.await() } == null)
    delay(300)
    out("T22.remote_still_running_after_cancel", executor.run(listOf("pgrep", "-f", "[s]leep 3.3")).exitCode == 0)
    delay(4000)
    val (outcome, ms) = outcomeOf(45_000) { session.close() }
    out("T22.close_after_cancel", outcome)
    out("T22.info.close_after_cancel_ms", ms)
    out("T22.after", executor.run(listOf("echo", "hello")).show())

    val finished = executor.execute(listOf("echo", "done"), Duration.ofSeconds(10), Durations.INFINITE, emptyMap(), null)
    out("T22.finished", finished.await().show())
    out("T22.is_alive_after_exit", finished.isAlive)
}

// T23 (fake --reject-exec): the server answers the exec request with failure
suspend fun t23(executor: SshjCommandExecutor) {
    try {
        val session = executor.execute(listOf("echo", "x"), Duration.ofSeconds(10), Durations.INFINITE, emptyMap(), null)
        out("T23.execute", "started; await: ${runCatching { session.await().show() }}")
    } catch (e: Exception) {
        out("T23.execute", "failed: ${kind(e)}")
        out("T23.info.error", describe(e))
    }
    out("T23.safe_execute", executor.safeExecute(Duration.ofSeconds(10), "echo", "x")?.show() ?: "null")
    out("T23.connected", executor.connected)
}

// T24 (fake --disconnect-on-exec=<reason>): the server disconnects instead of running the command
suspend fun t24(executor: SshjCommandExecutor) {
    val start = now()
    runCatching {
        executor.execute(listOf("sleep", "5"), Duration.ofSeconds(30), Durations.INFINITE, emptyMap(), null).await()
    }.fold({ out("T24.result", it.show()) }, {
        out("T24.result", "failed: ${kind(it)}")
        out("T24.info.error", describe(it))
    })
    out("T24.info.elapsed_ms", now() - start)
    delay(200)
    out("T24.connected", executor.connected)
}

// T25 (fake built with the keep-alive no-reply patch): output keeps flowing while keep-alive requests go unanswered
suspend fun t25(executor: SshjCommandExecutor) {
    val start = now()
    runCatching {
        executor.execute(listOf("sh", "-c", "while true; do echo tick; sleep 0.2; done"), Duration.ofSeconds(12),
            Durations.INFINITE, emptyMap(), null).await()
    }.also {
        // The command timeout is 12 s; a loss reported before it happened while output was still flowing
        out("T25.lost_while_output_flowed", it.isFailure && now() - start < 11_000)
    }.fold({
        out("T25.result", "exit=${it.exitCode}")
        out("T25.info.lines", it.stdout.size)
    }, {
        out("T25.result", "failed: ${kind(it)}")
        out("T25.info.error", describe(it))
    })
    out("T25.info.elapsed_ms", now() - start)
    out("T25.connected", executor.connected)
}

// Probe the actual factory, including WARN, which a quiet successful command cannot distinguish from ERROR.
fun loggingPolicy() {
    val debug = System.getenv("SPIKE_SSH_DEBUG") == "true"
    val config = com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.config.PerformanceDefaultConfig(debug)
    val logger = config.loggerFactory.getLogger("spike-log-policy")
    out("logging.levels", "debug=${logger.isDebugEnabled} info=${logger.isInfoEnabled} warn=${logger.isWarnEnabled} error=${logger.isErrorEnabled}")
    logger.debug("spike debug marker")
    logger.warn("spike warn marker")
    logger.error("spike error marker")
}
