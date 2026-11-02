package oracle

import ch.qos.logback.classic.Level
import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.LoggerContext
import ch.qos.logback.classic.PatternLayout
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.ConsoleAppender
import ch.qos.logback.core.encoder.LayoutWrappingEncoder
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.log.TimingLog
import com.malinskiy.marathon.android.configuration.AndroidLogConfigurator
import com.malinskiy.marathon.apple.AppleLogConfigurator
import com.malinskiy.marathon.apple.XcodebuildStderrLog
import org.slf4j.LoggerFactory
import java.time.Instant

/** Real logger lookups and vendor appenders; only the library-owned event clock is frozen, as in HookPatternLayout. */
fun loggingProbe(args: List<String>) {
    val context = LoggerFactory.getILoggerFactory() as LoggerContext
    if (args.singleOrNull() == "fallback") {
        val base = MarathonLogging.logger(Level.TRACE, "fallback-probe")
        val underlying = base.underlyingLogger as Logger
        underlying.isAdditive = false
        val events = ch.qos.logback.core.read.ListAppender<ILoggingEvent>().apply { start() }
        underlying.addAppender(events)
        val logger = com.malinskiy.marathon.log.KLoggerDebug(base)
        for (level in listOf(Level.TRACE, Level.DEBUG, Level.INFO, Level.WARN, Level.ERROR)) {
            for (varargs in listOf(false, true)) {
                val pattern = "{} {} {}"
                val arguments = if (varargs) arrayOf("one", "two", "three") else arrayOf("one", "two")
                when (level) {
                    Level.TRACE -> if (varargs) logger.trace(pattern, *arguments) else logger.trace(pattern, arguments[0], arguments[1])
                    Level.DEBUG -> if (varargs) logger.debug(pattern, *arguments) else logger.debug(pattern, arguments[0], arguments[1])
                    Level.INFO -> if (varargs) logger.info(pattern, *arguments) else logger.info(pattern, arguments[0], arguments[1])
                    Level.WARN -> if (varargs) logger.warn(pattern, *arguments) else logger.warn(pattern, arguments[0], arguments[1])
                    Level.ERROR -> if (varargs) logger.error(pattern, *arguments) else logger.error(pattern, arguments[0], arguments[1])
                }
                val event = events.list.last()
                println(Json.compact(linkedMapOf("op" to "message", "inputLevel" to level.toString(), // Non-null debug(format, a, b) resolves to the non-null vararg overload.
                    "varargs" to (varargs || level == Level.DEBUG),
                    "pattern" to pattern, "arguments" to arguments.toList(), "level" to event.level.toString(), "output" to event.formattedMessage)))
            }
        }
        val other = java.lang.reflect.Proxy.newProxyInstance(mu.KLogger::class.java.classLoader, arrayOf(mu.KLogger::class.java)) { _, method, _ ->
            when (method.name) {
                "getUnderlyingLogger" -> org.slf4j.helpers.NOPLogger.NOP_LOGGER
                else -> error("unexpected method " + method.name)
            }
        } as mu.KLogger
        val change = MarathonLogging::class.java.getDeclaredMethod("changeInternalLogLevel", mu.KLogger::class.java, Level::class.java).apply { isAccessible = true }
        for (debug in listOf(false, true, true)) {
            MarathonLogging.debug = debug
            val bytes = java.io.ByteArrayOutputStream()
            val saved = System.out
            try {
                System.setOut(java.io.PrintStream(bytes, true, Charsets.UTF_8))
                check(change.invoke(MarathonLogging, other, null) is com.malinskiy.marathon.log.KLoggerDebug)
            } finally { System.setOut(saved) }
            println(Json.compact(linkedMapOf("op" to "warning", "debug" to debug, "backend" to "NOPLogger", "output" to bytes.toString(Charsets.UTF_8))))
        }
        return
    }
    if (args.firstOrNull() == "hierarchy") {
        val rows = mutableListOf<String>()
        for ((name, requested) in listOf("a" to Level.ERROR, "root" to Level.ERROR, "rOoT" to Level.WARN, "ROOT" to Level.INFO)) {
            val handle = MarathonLogging.logger(requested, name)
            for (child in listOf(name, "a", "a\$b", "a.b", "a\$b.c", "unrelated", "root", "rOoT", "ROOT")) {
                val logger = context.getLogger(child)
                val event = ch.qos.logback.classic.spi.LoggingEvent().apply {
                    level = Level.ERROR; loggerName = logger.name; threadName = "main"; timeStamp = 0; message = "probe"
                }
                val layout = PatternLayout().apply { this.context = context; pattern = "%d{HH:mm:ss.SSS} [%thread] %-5level %logger{36} - %msg%n"; start() }
                rows.add(Json.compact(linkedMapOf("setName" to name, "setLevel" to requested.toString(),
                    "handleName" to handle.name, "lookup" to child, "name" to logger.name,
                    "level" to logger.effectiveLevel.toString(), "output" to layout.doLayout(event))))
            }
        }
        java.io.File(args[1]).writeText(rows.joinToString("\n", postfix = "\n"))
        return
    }
    if (args.singleOrNull() == "failures") {
        for (sequence in listOf("\u001b[9999999999999m", "\u001b[2147483648G", "\u001b[?J", "\u001b[=J", "\u001b[?K", "\u001b[?m")) {
            val bytes = java.io.ByteArrayOutputStream()
            val stream = org.fusesource.jansi.io.AnsiOutputStream(bytes, { 80 }, org.fusesource.jansi.AnsiMode.Strip,
                null, org.fusesource.jansi.AnsiType.Redirected, org.fusesource.jansi.AnsiColors.Colors256, Charsets.UTF_8, null, null, false)
            val writes = listOf("before" + sequence + "after", "next event\n", "\u001b[31mcoloured\u001b[0m\n")
            for (input in writes) {
                val error = try { stream.write(input.toByteArray(Charsets.UTF_8)); null } catch (e: Exception) { e.javaClass.name }
                println(Json.compact(linkedMapOf("sequence" to sequence, "input" to input, "error" to error, "output" to bytes.toString(Charsets.UTF_8))))
            }
        }
        return
    }
    if (args.singleOrNull() == "details") {
        val names = listOf("", "a..bc.LongName", "com.😀name.Foo", "éé.package.Class", "a.".repeat(30) + "Class", "trailing.", ".leading")
        for (name in names) for (length in listOf(0, 1, 2, 5, 10, 36, 40, 48)) {
            val result = if (length == 0) name.substringAfterLast('.') else
                ch.qos.logback.classic.pattern.TargetLengthBasedClassNameAbbreviator(length).abbreviate(name)
            println(Json.compact(linkedMapOf("op" to "abbreviate", "logger" to name, "length" to length, "output" to String(result.toByteArray(Charsets.UTF_8), Charsets.UTF_8))))
        }
        val patterns = listOf("{} {}", "\\{} {}", "\\\\{} {}", "\\\\\\{} {}", "no args", "{}", "{} {} {}")
        for (pattern in patterns) for (arguments in listOf(emptyList<String>(), listOf("one"), listOf("one", "two", "three"))) {
            val result = org.slf4j.helpers.MessageFormatter.arrayFormat(pattern, arguments.toTypedArray()).message
            println(Json.compact(linkedMapOf("op" to "message", "pattern" to pattern, "arguments" to arguments, "output" to result)))
        }
        val sequences = listOf(
            "\u001b[C", "\u001b[2C", "\u001b[;C", "\u001b[0C",
            "\u001b[E", "\u001b[2;3E", "\u001b[;E",
            "\u001b[G", "\u001b[;G", "\u001b[?25G", "\u001b[=1G",
            "\u001b[5G", "\u001b[05G", "\u001b[5;6G", "\u001b[2147483647G",
            "\u001b[31mred\u001b[0m", "\u001b[2J", "\u001b[1;2H", "\u001b[?25l", "\u001b[?1049h", "\u001b[z", "\u001b[12z",
            "\u001b]0;title\u0007", "\u001b]2;title\u001b\\", "\u001b]8;;url\u001b\\link\u001b]8;;\u001b\\",
            "\u001b7", "\u001b8", "\u001b(0", "\u001b(B", "\u001bX", "\u001b[", "\u001b", "\u001b[broken",
            "\u001b[" + "1".repeat(110) + "m", "\u001b[38;2;255;100;50m", "\u001b[3K", "\u001b[10A", "\u001b[s", "\u001b[u"
        )
        for (sequence in sequences) {
            val input = "before" + sequence + "after"
            val bytes = java.io.ByteArrayOutputStream()
            val stream = org.fusesource.jansi.io.AnsiOutputStream(bytes, { 80 }, org.fusesource.jansi.AnsiMode.Strip,
                null, org.fusesource.jansi.AnsiType.Redirected, org.fusesource.jansi.AnsiColors.Colors256, Charsets.UTF_8, null, null, false)
            stream.write(input.toByteArray(Charsets.UTF_8))
            stream.close()
            println(Json.compact(linkedMapOf("op" to "ansi", "input" to input, "output" to bytes.toString(Charsets.UTF_8))))
        }
        val common = StackTraceElement("example.Main", "main", "Main.kt", 3)
        val cause = IllegalArgumentException("inner").apply { stackTrace = arrayOf(StackTraceElement("example.Inner", "run", "Inner.kt", 8), common) }
        val middle = RuntimeException("middle", cause).apply { stackTrace = arrayOf(StackTraceElement("example.Middle", "run", "Middle.kt", 9), common) }
        val outer = IllegalStateException("outer", middle).apply { stackTrace = arrayOf(StackTraceElement("example.Outer", "run", "Outer.kt", 10), common) }
        fun input(t: Throwable): Map<String, Any?> = linkedMapOf("class" to t.javaClass.name, "message" to t.message,
            "frames" to t.stackTrace.map { it.toString() }, "cause" to t.cause?.let { input(it) })
        val proxy = ch.qos.logback.classic.spi.ThrowableProxy(outer)
        val rendered = ch.qos.logback.classic.spi.ThrowableProxyUtil.asString(proxy)
        println(Json.compact(linkedMapOf("op" to "throwable", "throwable" to input(outer), "output" to rendered)))
        return
    }
    if (args.firstOrNull() == "state") {
        val states = mutableListOf<String>()
        fun row(op: String, name: String) {
            val l = context.getLogger(name)
            states.add(Json.compact(linkedMapOf("op" to op, "name" to name, "level" to l.effectiveLevel.toString())))
        }
        row("startup", "ROOT")
        row("startup", "io.netty.channel.Channel")
        row("startup", "io.vertx.core.Vertx")
        MarathonLogging.logger("early")
        row("lookup", "early")
        MarathonLogging.debug = false
        AndroidLogConfigurator().configure()
        row("configured", "early")
        MarathonLogging.logger("early")
        row("lookup", "early")
        MarathonLogging.logger(Level.TRACE, "early")
        row("explicit", "early")
        MarathonLogging.logger("early")
        row("lookup", "early")
        MarathonLogging.debug = true
        MarathonLogging.logger("early")
        row("lookup", "early")
        AppleLogConfigurator(true).configure()
        listOf("TimelineSummaryProvider", "net.schmizz.sshj.DefaultConfig", "net.schmizz.sshj.common.KeyType",
            "net.schmizz.sshj.common.ECDSAVariationsAdapter", "net.schmizz.sshj.transport.kex.Curve25519SHA256",
            "net.schmizz.sshj.transport.random.BouncyCastleRandom").forEach { row("apple", it) }
        MarathonLogging.logger("TimelineSummaryProvider")
        row("lookup", "TimelineSummaryProvider")
        java.io.File(args[1]).writeText(states.joinToString("\n", postfix = "\n"))
        return
    }
    when (args.single()) {
        "failure-m", "failure-G", "failure-private" -> AppleLogConfigurator(true).configure()
        "startup" -> Unit
        "android" -> AndroidLogConfigurator().configure()
        "apple" -> AppleLogConfigurator(false).configure()
        "compact" -> AppleLogConfigurator(true).configure()
        else -> error("unknown layout")
    }
    val root = context.getLogger(Logger.ROOT_LOGGER_NAME)
    val appender = root.iteratorForAppenders().next() as ConsoleAppender<ILoggingEvent>
    val old = appender.encoder as LayoutWrappingEncoder<ILoggingEvent>
    val original = (old.layout as PatternLayout).pattern
    val layout = object : PatternLayout() {
        override fun doLayout(event: ILoggingEvent): String = super.doLayout(object : ILoggingEvent by event {
            override fun getTimeStamp(): Long = 0
            override fun getInstant(): Instant = Instant.EPOCH
            override fun getNanoseconds(): Int = 0
        })
    }.apply { this.context = context; pattern = original; start() }
    appender.encoder = LayoutWrappingEncoder<ILoggingEvent>().apply {
        this.context = context
        this.layout = layout
        start()
    }
    if (args.single().startsWith("failure-")) {
        val sequence = when (args.single()) {
            "failure-m" -> "\u001b[9999999999999m"
            "failure-G" -> "\u001b[2147483648G"
            else -> "\u001b[?J"
        }
        val probe = MarathonLogging.logger(Level.TRACE, "probe")
        probe.error { "before" + sequence + "after" }
        probe.error { "next event" }
        probe.error { "third event" }
        context.stop()
        return
    }
    val logger = MarathonLogging.logger(Level.TRACE, "com.malinskiy.marathon.execution.queue.QueueActor")
    logger.trace { "trace" }
    logger.debug { "debug" }
    logger.info { "info ü" }
    logger.warn { "warning" }
    logger.info { "embedded \u001b[32mgreen\u001b[0m \u001b]0;title\u0007done" }
    logger.error { "failed\nsecond line" }
    TimingLog.info { "Phase complete (+1234 ms since start)" }
    XcodebuildStderrLog.log("simulator udid", "destination warning")
    context.stop()
}
