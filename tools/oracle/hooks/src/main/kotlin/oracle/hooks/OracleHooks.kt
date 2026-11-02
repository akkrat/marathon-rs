package oracle.hooks

import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonPrimitive
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.ReceiveChannel
import java.io.File
import java.io.Writer
import java.lang.ref.WeakReference
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.attribute.PosixFilePermissions
import java.time.Clock
import java.time.Instant
import java.time.ZoneId
import java.util.Date
import java.util.Locale
import java.util.TreeMap
import java.util.IdentityHashMap
import kotlin.reflect.full.memberProperties
import kotlin.reflect.full.primaryConstructor
import kotlin.reflect.jvm.isAccessible
import java.util.UUID
import kotlin.random.Random

/**
 * The single seam through which the patched Kotlin marathon reads every hooked source of nondeterminism (PLAN.md §6.3,
 * docs/design/nondeterminism.md). Each call names its call site with a stable label from the hook inventory.
 *
 * Without the system property `oracle.hooks.tape` the service is inactive and every method is exactly the expression it
 * replaced, so the patched build behaves like the reference build. With it, the tape (docs/design/replay-tapes.md) decides:
 * - the wall and monotonic clocks, which advance only as their tape mode says;
 * - each random draw, taken in order from the draw tape of the call site's label;
 * - each ID, taken from the ID tape of the call site's label;
 * - with `oracle.hooks.trace`, every hooked call is appended to that file as one JSON line.
 *
 * The replay driver (tools/oracle/module, `oracle scenario`) additionally installs the virtual scheduler: a test dispatcher
 * that drives the hooked `runBlocking` and whose clock the `scheduler` clock modes read. The mailbox registry gives it the
 * actor half of the quiescence probe (PLAN.md §6.5).
 */
object OracleHooks {
    const val TAPE_PROPERTY = "oracle.hooks.tape"
    const val TRACE_PROPERTY = "oracle.hooks.trace"

    private val tape: Tape? = System.getProperty(TAPE_PROPERTY)?.let { Tape.read(File(it)) }

    @JvmField
    val active: Boolean = tape != null

    private val barriers = tape?.barriers?.takeIf { it.isNotEmpty() }?.let { Barriers(it, ::fail) }

    private val trace: Writer? = System.getProperty(TRACE_PROPERTY)?.takeIf { active }?.let { File(it).bufferedWriter() }

    private val lock = Any()
    private var seq = 0L
    private val calls = TreeMap<String, Int>()
    private val cursors = HashMap<String, Int>()
    private val failures = ArrayList<String>()
    private val mailboxes = ArrayList<Pair<String, WeakReference<Any>>>()

    @Volatile
    private var schedulerClock: (() -> Long)? = null

    @Volatile
    private var replayLoop: ReplayLoop? = null

    /** Runs the block of a hooked `runBlocking` on the replay driver's test dispatcher */
    interface ReplayLoop {
        fun <T> run(label: String, block: suspend CoroutineScope.() -> T): T
    }

    init {
        System.getProperty("oracle.hooks.report")?.takeIf { active }?.let { path ->
            Runtime.getRuntime().addShutdownHook(Thread {
                File(path).writeText(report() + "\n")
                flush()
            })
        }
    }

    // ---- wall clock --------------------------------------------------------------------------------------------------------

    fun currentTimeMillis(label: String): Long {
        if (!active) return System.currentTimeMillis()
        return wall(label)
    }

    fun instant(label: String): Instant {
        if (!active) return Instant.now()
        return Instant.ofEpochMilli(wall(label))
    }

    fun date(label: String): Date {
        if (!active) return Date()
        return Date(wall(label))
    }

    /** The injected [Clock]: [real] when inactive, otherwise a clock whose every read is a wall read of [label] */
    fun clock(label: String, real: Clock): Clock {
        if (!active) return real
        return TapeClock(label, real.zone)
    }

    // ---- monotonic clock ---------------------------------------------------------------------------------------------------

    fun nanoTime(label: String): Long {
        if (!active) return System.nanoTime()
        val spec = tape!!.monotonic
        return synchronized(lock) {
            record("monotonic", label, readOverride(label) ?: when (spec.mode) {
                Tape.ClockMode.REAL -> System.nanoTime()
                Tape.ClockMode.FROZEN -> spec.start
                Tape.ClockMode.SCHEDULER -> spec.start + schedulerElapsedMillis() * 1_000_000
                Tape.ClockMode.STEP -> spec.start + count(label) * spec.step
            })
        }
    }

    // ---- randomness --------------------------------------------------------------------------------------------------------

    /** `kotlin.random.Random.nextInt()` */
    fun nextInt(label: String): Int {
        if (!active) return Random.nextInt()
        val draw = nextDraw(label)
        val value = if (draw.isJsonPrimitive && draw.asJsonPrimitive.isNumber) draw.asInt else fail("$label: draw $draw is not an int")
        return record("draw", label, value)
    }

    /** `items.random()`. A draw names the element by [key]; the tape does not depend on the iteration order of [items] */
    fun <T> pick(label: String, items: Collection<T>, key: (T) -> String): T {
        if (!active) return items.random()
        val draw = nextDraw(label)
        val wanted = if (draw.isJsonPrimitive && draw.asJsonPrimitive.isString) draw.asString else fail("$label: draw $draw is not a key")
        val keys = items.map(key)
        val chosen = items.firstOrNull { key(it) == wanted } ?: fail("$label: draw \"$wanted\" is not one of $keys")
        record("draw", label, wanted)
        return chosen
    }

    // Inspect values before formatting: identity-shaped user strings are literal data.
    private val identities = IdentityHashMap<Any, String>()
    fun identities(label: String, value: Any?): String {
        if (!active) return value?.toString() ?: ""
        if (value == null) return ""
        if (value is String) return value
        if (value.javaClass.getMethod("toString").declaringClass == Any::class.java) {
            return synchronized(lock) {
                identities.getOrPut(value) { value.javaClass.name + "@" + record("id", label, nextId(label)) }
            }
        }
        if (value is Collection<*>) return value.joinToString(", ", "[", "]") { if (it == null) "null" else identities(label, it) }
        if (value is Map<*, *>) return value.entries.joinToString(", ", "{", "}") {
            "${if (it.key == null) "null" else identities(label, it.key)}=${if (it.value == null) "null" else identities(label, it.value)}"
        }
        // Kotlin data-class rendering follows primary-constructor order. Only replace child identities;
        // strings (even strings equal to an object's original rendering) never enter the identity table.
        if (value::class.isData) {
            val properties = value::class.memberProperties.associateBy { it.name }
            val children = value::class.primaryConstructor!!.parameters.map {
                val getter = properties.getValue(it.name!!).getter
                getter.isAccessible = true
                it.name!! to getter.call(value)
            }
            val prefix = value.javaClass.simpleName + "("
            val original = children.joinToString(", ", prefix, ")") { "${it.first}=${it.second}" }
            // Custom renderers can redact credentials or change syntax. Never reconstruct those.
            if (original == value.toString()) {
                return children.joinToString(", ", prefix, ")") {
                    "${it.first}=${if (it.second == null) "null" else identities(label, it.second)}"
                }
            }
        }
        return value.toString()
    }

    // ---- IDs ---------------------------------------------------------------------------------------------------------------

    /** `UUID.randomUUID().toString()` */
    fun uuid(label: String): String {
        if (!active) return UUID.randomUUID().toString()
        return record("id", label, nextId(label))
    }

    /** `kotlin.io.path.createTempFile(prefix, suffix)`; on tape the random part of the name is the next ID of [label] */
    fun createTempFile(label: String, prefix: String? = null, suffix: String? = null): Path {
        if (!active) return kotlin.io.path.createTempFile(prefix, suffix)
        val name = (prefix ?: "") + nextId(label) + (suffix ?: ".tmp")
        return Files.createFile(tmpDir().resolve(name), PosixFilePermissions.asFileAttribute(
            PosixFilePermissions.fromString("rw-------"))).also { record("id", label, it.toString()) }
    }

    /** `Files.createTempDirectory(prefix)`; on tape the random part of the name is the next ID of [label] */
    fun createTempDirectory(label: String, prefix: String?): Path {
        if (!active) return Files.createTempDirectory(prefix)
        val name = (prefix ?: "") + nextId(label)
        return Files.createDirectory(tmpDir().resolve(name), PosixFilePermissions.asFileAttribute(
            PosixFilePermissions.fromString("rwx------"))).also { record("id", label, it.toString()) }
    }

    // ---- scheduling and quiescence ----------------------------------------------------------------------------------------

    /** `kotlinx.coroutines.runBlocking(block = block)`, or the replay driver's test dispatcher when one is installed */
    fun <T> runBlocking(label: String, block: suspend CoroutineScope.() -> T): T {
        val loop = replayLoop
        if (!active || loop == null) {
            if (active && tape!!.scheduler == Tape.SchedulerMode.VIRTUAL) {
                fail("$label: the tape asks for the virtual scheduler, which only the replay driver (oracle scenario) provides")
            }
            return kotlinx.coroutines.runBlocking(block = block)
        }
        record("scheduler", label, "replay loop")
        return loop.run(label, block)
    }

    suspend fun <T> event(label: String, key: String, block: suspend () -> T): T {
        val gate = barriers ?: return block()
        return gate.event(label, key) { record("event", label, key); block() }
    }

    /** Registers an actor mailbox or event channel for the quiescence probe and returns it unchanged */
    fun <C : Any> mailbox(label: String, name: String, channel: C): C {
        if (!active) return channel
        synchronized(lock) {
            val indexed = "$name#${mailboxes.count { it.first.startsWith("$name#") } + 1}"
            mailboxes.add(indexed to WeakReference(channel))
            record("mailbox", label, indexed)
        }
        return channel
    }

    /** The registered mailboxes that hold undelivered messages; empty when the actor half of quiescence holds */
    @OptIn(ExperimentalCoroutinesApi::class)
    fun busyMailboxes(): List<String> = synchronized(lock) {
        mailboxes.mapNotNull { (name, ref) ->
            val channel = ref.get() as? ReceiveChannel<*> ?: return@mapNotNull null
            name.takeIf { !channel.isClosedForReceive && !channel.isEmpty }
        }
    }

    // ---- replay driver API -------------------------------------------------------------------------------------------------

    fun installReplayLoop(loop: ReplayLoop, schedulerMillis: () -> Long) {
        check(active && tape!!.scheduler == Tape.SchedulerMode.VIRTUAL) {
            "the replay driver needs -D$TAPE_PROPERTY with clocks.scheduler.mode virtual"
        }
        schedulerClock = schedulerMillis
        replayLoop = loop
    }

    /** JSON: hooked calls per label, unused tape entries and tape failures. Deterministic for a deterministic run */
    fun report(): String {
        val remainingBarriers = barriers?.remaining()
        return synchronized(lock) {
        val out = JsonObject()
        out.addProperty("active", active)
        out.add("calls", JsonObject().apply { calls.forEach { (label, n) -> addProperty(label, n) } })
        val left = JsonObject()
        tape?.let { t ->
            fun leftOf(section: String, label: String, size: Int) {
                val used = cursors["$section:$label"] ?: 0
                if (used < size) left.addProperty("$section.$label", size - used)
            }
            t.reads.toSortedMap().forEach { (label, values) -> leftOf("reads", label, values.size) }
            t.draws.toSortedMap().forEach { (label, values) -> leftOf("draws", label, values.size) }
            t.ids.toSortedMap().forEach { (label, spec) -> spec.values?.let { leftOf("ids", label, it.size) } }
        }
        remainingBarriers?.takeIf { it.isNotEmpty() }?.let {
            left.add("barriers", JsonArray().apply { it.forEach { token -> add(token) } })
        }
        out.add("unused", left)
        out.add("failures", JsonArray().apply { failures.forEach { add(it) } })
        out.toString()
        }
    }

    fun flush() {
        synchronized(lock) { trace?.flush() }
    }

    // ---- internals ---------------------------------------------------------------------------------------------------------

    internal fun wall(label: String): Long {
        val spec = tape!!.wall
        // Under the lock, so that a step-mode read and its count are one atomic step
        return synchronized(lock) {
            record("wall", label, readOverride(label) ?: when (spec.mode) {
                Tape.ClockMode.REAL -> System.currentTimeMillis()
                Tape.ClockMode.FROZEN -> spec.start
                Tape.ClockMode.SCHEDULER -> spec.start + schedulerElapsedMillis()
                Tape.ClockMode.STEP -> spec.start + count(label) * spec.step
            })
        }
    }

    private fun schedulerElapsedMillis(): Long =
        schedulerClock?.invoke() ?: fail("scheduler clock requested without an installed replay loop")

    /** How often [label] has been called before */
    private fun count(label: String): Long = synchronized(lock) { (calls[label] ?: 0).toLong() }

    private fun readOverride(label: String): Long? {
        val values = tape!!.reads[label] ?: return null
        return values[cursor("reads", label, values.size)]
    }

    private fun nextDraw(label: String): JsonElement {
        val values = tape!!.draws[label] ?: fail("$label: no draw tape")
        return values[cursor("draws", label, values.size)]
    }

    private fun nextId(label: String): String {
        val spec = tape!!.ids[label] ?: fail("$label: no ID tape")
        spec.values?.let { return it[cursor("ids", label, it.size)] }
        val n = spec.start + cursor("ids", label, Int.MAX_VALUE)
        return String.format(Locale.ROOT, spec.pattern!!, n)
    }

    private fun cursor(section: String, label: String, size: Int): Int = synchronized(lock) {
        val key = "$section:$label"
        val next = cursors[key] ?: 0
        if (next >= size) fail("$label: the $section tape is exhausted after $size entries")
        cursors[key] = next + 1
        next
    }

    private fun tmpDir(): Path = Paths.get(System.getProperty("java.io.tmpdir"))

    private fun <T> record(kind: String, label: String, value: T): T {
        synchronized(lock) {
            calls[label] = (calls[label] ?: 0) + 1
            trace?.let { writer ->
                val line = JsonObject()
                line.addProperty("seq", ++seq)
                line.addProperty("kind", kind)
                line.addProperty("label", label)
                line.add("value", when (value) {
                    is Number -> JsonPrimitive(value)
                    null -> null
                    else -> JsonPrimitive(value.toString())
                })
                writer.write(line.toString())
                writer.write("\n")
                writer.flush()
            }
        }
        return value
    }

    internal fun fail(message: String): Nothing {
        synchronized(lock) { failures.add(message) }
        System.err.println("oracle hooks: $message")
        throw OracleTapeException(message)
    }

    private class TapeClock(private val label: String, private val zone: ZoneId) : Clock() {
        override fun getZone(): ZoneId = zone
        override fun withZone(zone: ZoneId): Clock = TapeClock(label, zone)
        override fun instant(): Instant = Instant.ofEpochMilli(millis())
        override fun millis(): Long = OracleHooks.wall(label)
    }
}
