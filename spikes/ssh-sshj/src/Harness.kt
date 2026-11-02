// WP 0.3a: drives the Kotlin marathon's own sshj executor (SshjCommandExecutorFactory, SshjCommandExecutor,
// SshjCommandSession, PatchedSSHClient, writeFile, consumeOutput) through the T01-T20 matrix of docs/research/ssh.md.
//
// run.sh <case> [args]. The target comes from the environment:
//   SPIKE_HOST, SPIKE_PORT, SPIKE_USER, SPIKE_AUTH (password:<pw> | key:<path> | agent), SPIKE_KNOWN_HOSTS (path, empty
//   for accept-any), SPIKE_KA_MS, SPIKE_KA_MAX, SPIKE_ATTEMPTS, SPIKE_PAUSE_CMD / SPIKE_RESUME_CMD (rows T09/T10).
// Output is `key: value` lines; the ssh-russh harness prints the same keys, `*.info.*` keys are timings and messages that
// are not expected to be equal. The host address is printed as <host>.
import com.malinskiy.marathon.apple.cmd.CommandExecutor
import com.malinskiy.marathon.apple.cmd.CommandHost
import com.malinskiy.marathon.apple.cmd.CommandResult
import com.malinskiy.marathon.apple.cmd.CommandSession
import com.malinskiy.marathon.apple.cmd.consumeOutput
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutor
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorFactory
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandHost
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandSession
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.auth.SshAuthentication
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.shellCommand
import com.malinskiy.marathon.apple.cmd.writeFile
import com.malinskiy.marathon.apple.device.isTransientConnectionFailure
import com.malinskiy.marathon.apple.extensions.Durations
import com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException
import com.malinskiy.marathon.apple.logparser.parser.DeviceFailureReason
import com.malinskiy.marathon.execution.withRetrySync
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import mu.KotlinLogging
import net.schmizz.sshj.common.SSHException
import net.schmizz.sshj.connection.ConnectionException
import net.schmizz.sshj.connection.channel.direct.Session
import net.schmizz.sshj.transport.verification.HostKeyVerifier
import net.schmizz.sshj.transport.verification.OpenSSHKnownHosts
import net.schmizz.sshj.transport.verification.PromiscuousVerifier
import java.io.File
import java.io.IOException
import java.nio.charset.Charset
import java.security.MessageDigest
import java.time.Duration
import java.util.concurrent.atomic.AtomicInteger

private val logger = KotlinLogging.logger("harness")

private fun env(name: String, default: String? = null): String =
    System.getenv(name)?.takeIf { it.isNotEmpty() } ?: default ?: error("$name is not set")

private val host get() = env("SPIKE_HOST")
private val port get() = env("SPIKE_PORT", "22").toInt()
private val user get() = env("SPIKE_USER")

fun out(key: String, value: Any?) {
    val text = value.toString().replace(host, "<host>")
    println("$key: $text")
    System.out.flush()
}

fun describe(e: Throwable): String = generateSequence(e) { it.cause?.takeIf { next -> next !== it } }.take(6).joinToString(" <- ") {
    val reason = (it as? SSHException)?.disconnectReason?.let { r -> "[$r]" } ?: ""
    "${it.javaClass.simpleName}$reason(${it.message})"
}

private fun now() = System.nanoTime() / 1_000_000

fun verifier(): HostKeyVerifier = System.getenv("SPIKE_KNOWN_HOSTS")?.takeIf { it.isNotEmpty() }?.let { OpenSSHKnownHosts(File(it)) }
    ?: PromiscuousVerifier()

fun connect(attempts: AtomicInteger = AtomicInteger()): SshjCommandExecutor {
    val factory = SshjCommandExecutorFactory(
        connectionAttempts = env("SPIKE_ATTEMPTS", "4").toInt(),
        keepAliveInterval = Duration.ofMillis(env("SPIKE_KA_MS", "15000").toLong()),
        keepAliveMaxCount = env("SPIKE_KA_MAX", "4").toInt(),
    )
    val hostKeyVerifier = verifier()
    val newClient = { attempts.incrementAndGet(); factory.createClient(host, port, hostKeyVerifier, debug = env("SPIKE_SSH_DEBUG", "false").toBoolean()) }
    val auth = env("SPIKE_AUTH")
    if (auth == "agent") {
        // Same sequence as SshjCommandExecutorFactory.connect, with the agent signing instead of loadKeys
        val client = withRetrySync(env("SPIKE_ATTEMPTS", "4").toInt(), 0, logger) {
            newClient().also { client ->
                try {
                    client.connect(host, port)
                } catch (e: Throwable) {
                    client.close()
                    throw e
                }
            }
        }
        try {
            client.auth(user, AgentAuth())
        } catch (e: Throwable) {
            client.close()
            throw e
        }
        val sshHost = SshjCommandHost(host, port, SshAuthentication.PasswordAuthentication(user, ""), client.remoteCharset)
        return SshjCommandExecutor(sshHost, client, Channel.BUFFERED)
    }
    val authentication = when {
        auth.startsWith("password:") -> SshAuthentication.PasswordAuthentication(user, auth.removePrefix("password:"))
        auth.startsWith("key:") -> SshAuthentication.PublicKeyAuthentication(user, File(auth.removePrefix("key:")))
        else -> error("bad SPIKE_AUTH")
    }
    return factory.connect(newClient, host, port, authentication)
}

suspend fun CommandExecutor.run(
    command: List<String>,
    timeout: Duration = Duration.ofSeconds(60),
    idleTimeout: Duration = Durations.INFINITE,
): CommandResult = execute(command, timeout, idleTimeout, emptyMap(), null).await()

fun CommandResult.show(): String = "exit=$exitCode stdout=$stdout stderr=$stderr"

/** The sshj channel inside a session, for the raw exit status and signal */
fun SshjCommandSession.channel(): Session.Command =
    SshjCommandSession::class.java.getDeclaredField("command").apply { isAccessible = true }.get(this) as Session.Command

fun shell(command: String) {
    val process = ProcessBuilder("sh", "-c", command).inheritIO().start()
    process.waitFor()
}

fun main(args: Array<String>) {
    val case = args.firstOrNull() ?: error("usage: run.sh <case> [args]")
    runBlocking(Dispatchers.IO) {
        when (case) {
            "connect" -> caseConnect()
            "logging" -> loggingPolicy()
            "t20" -> t20()
            "shell-command" -> args.drop(1).let { out("wire", shellCommand(it)) }
            else -> connect().use { executor ->
                when (case) {
                    "t01" -> t01(executor)
                    "t02" -> t02(executor)
                    "t03" -> t03(executor)
                    "t04" -> t04(executor)
                    "t05" -> t05(executor)
                    "t06" -> t06(executor)
                    "t07" -> t07(executor)
                    "t08" -> t08(executor)
                    "t09" -> t09(executor, args[1].toLong())
                    "t11" -> t11(executor)
                    "t14" -> t14(executor)
                    "t15" -> { t15(executor); System.exit(0) }
                    "t15h" -> { t15h(executor); System.exit(0) }
                    "t15t" -> t15t(executor)
                    "t21" -> t21(executor)
                    "t22" -> t22(executor)
                    "t23" -> t23(executor)
                    "t24" -> t24(executor)
                    "t25" -> t25(executor)
                    "t16" -> t16(executor)
                    "t18" -> t18(executor)
                    else -> error("unknown case $case")
                }
            }
        }
    }
    // sshj's reader threads are not daemons
    System.exit(0)
}

// T12, T13, T17, T19: connect, run one command, classify the failure as the ConnectionFactory does
suspend fun caseConnect() {
    val attempts = AtomicInteger()
    val start = now()
    try {
        connect(attempts).use { executor ->
            out("connect.result", "ok ${executor.run(listOf("echo", "connected")).stdout}")
            out("connect.host_id", executor.host.id)
        }
    } catch (e: Throwable) {
        out("connect.result", "error")
        out("connect.info.error", describe(e))
        // ConnectionFactory.connect wraps every IOException into UnreachableHost
        val wrapped = if (e is IOException) DeviceFailureException(DeviceFailureReason.UnreachableHost, e) else e
        out("connect.transient", isTransientConnectionFailure(wrapped))
    }
    out("connect.attempts", attempts.get())
    out("connect.info.elapsed_ms", now() - start)
}

suspend fun t01(executor: SshjCommandExecutor) {
    out("T01.stdout", executor.run(listOf("echo", "Hello world"), Duration.ofSeconds(5), Duration.ofSeconds(1)).show())
    out("T01.stderr", executor.run(listOf("bash", "-c", "1>/dev/stderr echo Hello world"), Duration.ofSeconds(5), Duration.ofSeconds(1)).show())
    out("T01.exit", executor.run(listOf("sh", "-c", "exit 1"), Duration.ofSeconds(5), Duration.ofSeconds(1)).show())
    out("T01.host_id", executor.host.id)
    out("T01.charset", executor.host.charset)
}

suspend fun t02(executor: SshjCommandExecutor) {
    val seq = executor.run(listOf("sh", "-c", "seq 1 100000"), Duration.ofSeconds(30), Duration.ofSeconds(5))
    out("T02.seq", "exit=${seq.exitCode} stdout_lines=${seq.stdout.size} ordered=${seq.stdout == (1..100000).map { it.toString() }} stderr_lines=${seq.stderr.size}")

    executor.execute(listOf("sh", "-c", "echo start; seq 1 100000 1>&2; echo end"), Duration.ofSeconds(30), Duration.ofSeconds(5), emptyMap(), null).use { session ->
        val stdout = mutableListOf<String>()
        val stderr = mutableListOf<String>()
        val exit = session.consumeOutput(onStdout = { stdout.add(it) }, onStderr = { stderr.add(it) })
        out("T02.concurrent", "exit=$exit stdout=$stdout stderr_lines=${stderr.size} ordered=${stderr == (1..100000).map { it.toString() }}")
    }

    val session = executor.execute(listOf("sh", "-c", "echo start; exec seq 1 100000 1>&2"), Duration.ofSeconds(60), Duration.ofSeconds(30), emptyMap(), null)
    val error = runCatching { session.consumeOutput(onStdout = { error("consumer failed on $it") }, onStderr = {}) }.exceptionOrNull()
    val (outcome, ms) = outcomeOf(45_000) { session.close() }
    out("T02.consumer_failure", "error=${error?.message}")
    out("T02.close_outcome", outcome)
    out("T02.info.close_ms", ms)
}

val T03_ARGS = listOf(
    "plain", "a b", "\$HOME", "it's", "'already quoted'", "\"double quoted\"", "FOO=bar", "/p-a.t,h:+@x", "'half", "half'",
    "'mixed\"", "a\"b", "~", "semi;colon", "", "tab\tx", "back\\slash", "star*", "\$(echo injected)",
)

suspend fun t03(executor: SshjCommandExecutor) {
    T03_ARGS.forEachIndexed { index, arg ->
        val command = listOf("printf", "<%s>\\n", arg)
        out("T03.$index.wire", shellCommand(command))
        out("T03.$index.result", executor.run(command, Duration.ofSeconds(10)).show())
    }
}

suspend fun t04(executor: SshjCommandExecutor) {
    val result = executor.execute(listOf("sh", "-c", "echo F=\${F-unset}; pwd"), Duration.ofSeconds(10), Durations.INFINITE, mapOf("F" to "1"), "/tmp").await()
    out("T04.result", result.show().replace(Regex("/(Users/[^,\\]]*|config)"), "<home>"))
}

suspend fun t05(executor: SshjCommandExecutor) {
    val start = now()
    val result = executor.run(listOf("sleep", "1717"), Duration.ofMillis(10))
    out("T05.result", result.show())
    out("T05.info.elapsed_ms", now() - start)
    delay(1000)
    val survivor = executor.run(listOf("pgrep", "-f", "[s]leep 1717"))
    out("T05.remote_sleep_survives", survivor.exitCode == 0)
    if (survivor.exitCode == 0) executor.run(listOf("pkill", "-f", "[s]leep 1717"))
}

suspend fun t06(executor: SshjCommandExecutor) = coroutineScopeIO {
    val busy = executor.execute(listOf("sh", "-c", "while [ true ]; do date && sleep 0.1; done"), Duration.ofSeconds(5), Duration.ofSeconds(1), emptyMap(), null)
    val busyResult = async { busy.await() }
    delay(500)
    busy.terminate()
    val r = busyResult.await()
    out("T06.busy", "exit=${r.exitCode} stdout_in_3_10=${r.stdout.size in 3..10} stderr=${r.stderr}")
    out("T06.info.busy_lines", r.stdout.size)

    val sleeper = executor.execute(listOf("sleep", "1000"), Duration.ofMinutes(10), Duration.ofMinutes(10), emptyMap(), null)
    val sleeperResult = async { sleeper.await() }
    delay(1000)
    sleeper.terminate()
    out("T06.sleep", sleeperResult.await().show())
    out("T06.after", executor.run(listOf("echo", "hello")).show())
}

suspend fun t07(executor: SshjCommandExecutor) = coroutineScopeIO {
    out("T07.background", executor.run(listOf("sleep", "100000", "&"), Duration.ofMinutes(10)).show())
    out("T07.background_after", executor.run(listOf("echo", "hello")).show())

    val ignoring = listOf("sh", "-c", "trap \"\" TERM; sleep 5; echo survived")
    val session = executor.execute(ignoring, Duration.ofSeconds(30), Durations.INFINITE, emptyMap(), null)
    val result = async { session.await() }
    delay(1000)
    val start = now()
    session.terminate()
    out("T07.ignored_term", result.await().show())
    out("T07.info.ignored_term_ms", now() - start)

    // close() = TERM if open, close, unbounded join
    val closing = executor.execute(ignoring, Duration.ofSeconds(30), Durations.INFINITE, emptyMap(), null)
    val drained = async { runCatching { closing.await() } }
    delay(1000)
    val (outcome, ms) = outcomeOf(45_000) { closing.close() }
    out("T07.close_outcome", outcome)
    out("T07.info.close_ms", ms)
    observeAwait("T07.close_await", drained.await())
    out("T07.close_after", executor.run(listOf("echo", "hello")).show())
    delay(5000)
}

suspend fun t08(executor: SshjCommandExecutor) = coroutineScopeIO {
    val held = List(2) { executor.execute(listOf("sleep", "20"), Duration.ofMinutes(1), Durations.INFINITE, emptyMap(), null) }
    try {
        val third = executor.execute(listOf("echo", "third"), Duration.ofSeconds(10), Durations.INFINITE, emptyMap(), null)
        out("T08.third", "opened ${third.await().show()}")
    } catch (e: Throwable) {
        out("T08.third", "error is_connection_exception=${e is ConnectionException}")
        out("T08.info.third_error", describe(e))
    }
    out("T08.safe_execute", executor.safeExecute(Duration.ofSeconds(10), "echo", "safe"))
    out("T08.connected", executor.connected)
    held.forEach { it.terminate() }
    held.forEach { out("T08.held", it.await().show()) }
    out("T08.after", executor.run(listOf("echo", "after")).show())
}

// T09, T10: keep-alive loss while a command is in flight. The target is paused after 2 s; waitCapMs bounds the wait
suspend fun t09(executor: SshjCommandExecutor, waitCapMs: Long) = coroutineScopeIO {
    val client = SshjCommandExecutor::class.java.getDeclaredField("client").apply { isAccessible = true }.get(executor) as net.schmizz.sshj.SSHClient
    out("T09.effective_interval_s", client.connection.keepAlive.keepAliveInterval)
    out("T09.keep_alive_threads", Thread.getAllStackTraces().keys.filter { it.name.contains("keep-alive", ignoreCase = true) || it.name.contains("KeepAlive") }.map { "${it.name} alive=${it.isAlive}" })
    val session = executor.execute(listOf("sleep", "25"), Duration.ofMinutes(2), Durations.INFINITE, emptyMap(), null)
    val result = async { runCatching { session.await() } }
    delay(2000)
    shell(env("SPIKE_PAUSE_CMD"))
    val paused = now()
    var outcome: Result<CommandResult>? = null
    while (now() - paused < waitCapMs) {
        if (result.isCompleted) {
            outcome = result.await()
            break
        }
        delay(50)
    }
    val detectMs = now() - paused
    shell(env("SPIKE_RESUME_CMD"))
    if (outcome == null) {
        out("T09.result", "no loss detected within ${waitCapMs / 1000} s")
        outcome = result.await()
        out("T09.after_resume", outcome.fold({ it.show() }, { describe(it) }))
    } else {
        out("T09.result", outcome.fold({ it.show() }, { "error ${kind(it)}" }))
        out("T09.info.error", outcome.exceptionOrNull()?.let(::describe))
        out("T09.info.detect_ms", detectMs)
    }
    out("T09.connected", executor.connected)
}

suspend fun t11(executor: SshjCommandExecutor) {
    out("T11.keep_alive_threads", Thread.getAllStackTraces().keys.filter { it.name.contains("keep-alive", ignoreCase = true) || it.name.contains("KeepAlive") }.map { it.name })
    val start = now()
    out("T11.idle_60s", executor.run(listOf("sleep", "60"), Duration.ofMinutes(2)).show())
    out("T11.info.elapsed_ms", now() - start)
    out("T11.connected", executor.connected)
}

suspend fun t14(executor: SshjCommandExecutor) = coroutineScopeIO {
    out("T14.timeout_partial", executor.run(listOf("sh", "-c", "printf partial; sleep 3"), Duration.ofSeconds(1)).show())
    out("T14.partial_line", executor.run(listOf("printf", "a\\nb\\npartial")).show())
    out("T14.line_endings", executor.run(listOf("printf", "x\\r\\ny\\rz\\n\\n\\r\\nw")).show())
    val big = executor.run(listOf("sh", "-c", "seq 1 300000; exit 3"))
    out("T14.fast_exit_big", "exit=${big.exitCode} lines=${big.stdout.size} last=${big.stdout.lastOrNull()}")
    val both = executor.run(listOf("sh", "-c", "seq 1 50000; seq 1 50000 1>&2"))
    out("T14.both_streams_exit", "exit=${both.exitCode} stdout_lines=${both.stdout.size} stderr_lines=${both.stderr.size}")

    val flood = executor.execute(listOf("seq", "1", "100000000"), Duration.ofMinutes(1), Durations.INFINITE, emptyMap(), null)
    val drained = async { runCatching { flood.await() } }
    delay(300)
    val (outcome, ms) = outcomeOf(45_000) { flood.close() }
    out("T14.flood_close_outcome", outcome)
    out("T14.info.flood_close_ms", ms)
    observeAwait("T14.flood_await", drained.await())
    out("T14.after", executor.run(listOf("echo", "hello")).show())

    runCatching { executor.run(listOf("sh", "-c", "echo a; sleep 3; echo b"), Duration.ofSeconds(10), Duration.ofMillis(1000)) }
        .fold({ out("T14.idle_timeout", it.show()) }, {
            out("T14.idle_timeout", "failed: ${kind(it)}")
            out("T14.info.idle_timeout_error", describe(it))
        })
}

// A null exit code takes the timeout branch of SshjCommandExecutor.execute, which signals a closed channel: each signal
// gets a fresh connection, and whether the connection survives is part of the result
suspend fun t16(first: SshjCommandExecutor) {
    for (signal in listOf("KILL", "SEGV", "TERM", "INT", "HUP", "USR2")) {
        val executor = if (signal == "KILL") first else connect()
        val session = executor.execute(listOf("sh", "-c", "kill -$signal \$\$"), Duration.ofSeconds(10), Durations.INFINITE, emptyMap(), null) as SshjCommandSession
        val result = session.await()
        val channel = session.channel()
        out("T16.$signal", "exit=${result.exitCode} raw_status=${channel.exitStatus} raw_signal=${channel.exitSignal}")
        delay(500)
        out("T16.$signal.connection_survives", runCatching { executor.run(listOf("echo", "hello"), Duration.ofSeconds(10)).stdout == listOf("hello") }.getOrDefault(false))
        if (executor !== first) executor.close()
    }
}

/** Records the commands that writeFile sends */
class Recording(private val delegate: CommandExecutor) : CommandExecutor by delegate {
    val commands = mutableListOf<List<String>>()
    override suspend fun execute(command: List<String>, timeout: Duration, idleTimeout: Duration, env: Map<String, String>, workdir: String?, charset: Charset): CommandSession {
        commands.add(command)
        return delegate.execute(command, timeout, idleTimeout, env, workdir, charset)
    }
    override val host: CommandHost get() = delegate.host
}

suspend fun t18(executor: SshjCommandExecutor) {
    val recording = Recording(executor)
    out("T18.small", recording.writeFile("hello\n".toByteArray(), "/tmp/spike-write-small", Duration.ofSeconds(30)))
    out("T18.small_wire", shellCommand(recording.commands.single()))
    val content = ByteArray(32 * 1024) { (it * 31 % 251).toByte() }
    out("T18.max", executor.writeFile(content, "/tmp/spike-write-max", Duration.ofSeconds(30)))
    val md5 = MessageDigest.getInstance("MD5").digest(content).joinToString("") { "%02x".format(it) }
    val remote = executor.run(listOf("md5sum", "/tmp/spike-write-max")).stdout.joinToString().substringBefore(' ')
    out("T18.max_md5_matches", remote == md5)
    out("T18.oversize", executor.writeFile(ByteArray(32 * 1024 + 1), "/tmp/spike-write-big", Duration.ofSeconds(30)))
    out("T18.bad_dst", executor.writeFile("x".toByteArray(), "/tmp/a b", Duration.ofSeconds(30)))
    out("T18.failing_dst", executor.writeFile("x".toByteArray(), "/nonexistent-dir/file", Duration.ofSeconds(30)))
    executor.run(listOf("rm", "-f", "/tmp/spike-write-small", "/tmp/spike-write-max"))
}

// T20: the Kotlin marathon's RsyncFileBridge sends SPIKE_T20_SRC and receives it back; the shims in t20/ record argv
suspend fun t20() {
    val auth = env("SPIKE_AUTH")
    val authentication = when {
        auth.startsWith("password:") -> com.malinskiy.marathon.config.vendor.apple.SshAuthentication.PasswordAuthentication(user, auth.removePrefix("password:"))
        else -> com.malinskiy.marathon.config.vendor.apple.SshAuthentication.PublicKeyAuthentication(user, File(auth.removePrefix("key:")))
    }
    val ssh = com.malinskiy.marathon.config.vendor.apple.SshConfiguration(
        authentication = authentication,
        keepAliveInterval = Duration.ofMillis(env("SPIKE_KA_MS", "15000").toLong()),
        keepAliveMaxCount = env("SPIKE_KA_MAX", "4").toInt(),
        debug = env("SPIKE_SSH_DEBUG", "false").toBoolean(),
    )
    val configuration = com.malinskiy.marathon.config.Configuration.Builder("spike", File(env("SPIKE_T20_OUT"))).apply {
        debug = env("SPIKE_DEBUG", "false").toBoolean()
    }.build()
    val bridge = com.malinskiy.marathon.apple.cmd.remote.rsync.RsyncFileBridge(
        com.malinskiy.marathon.apple.cmd.remote.rsync.RsyncTarget(host, port), configuration, ssh,
        com.malinskiy.marathon.config.vendor.apple.RsyncConfiguration(), authentication,
    )
    out("T20.send", bridge.send(File(env("SPIKE_T20_SRC")), env("SPIKE_T20_REMOTE")))
    out("T20.receive", bridge.receive(env("SPIKE_T20_REMOTE"), File(env("SPIKE_T20_BACK"))))
}

suspend fun <T> coroutineScopeIO(block: suspend kotlinx.coroutines.CoroutineScope.() -> T): T = withContext(Dispatchers.IO) { block() }

fun observeAwait(key: String, result: Result<CommandResult>) {
    out(key, result.fold({ "returned" }, {
        if (it is java.util.concurrent.CancellationException) "cancelled" else "failed"
    }))
    out("$key.info.result", result.fold({
        "exit=${it.exitCode} stdout_lines=${it.stdout.size} stderr_lines=${it.stderr.size}"
    }, { describe(it) }))
}
