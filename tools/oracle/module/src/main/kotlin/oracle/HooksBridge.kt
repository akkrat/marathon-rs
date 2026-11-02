package oracle

import kotlinx.coroutines.CoroutineScope

/**
 * What the replay driver needs from OracleHooks. The oracle module is built against the reference clone too, which has no
 * OracleHooks, so the driver reaches it through this interface: the patched clone compiles `src/hooks/kotlin`, whose
 * `oracle.HooksBridgeImpl` implements it, and the reference clone falls back to [None].
 */
interface HooksBridge {
    /** Whether the patched build runs with a tape */
    val active: Boolean

    /** Makes the hooked `runBlocking` run on [loop] and the `scheduler` clock modes read [schedulerMillis] */
    fun install(loop: Loop, schedulerMillis: () -> Long)

    /** The registered actor mailboxes and event channels that hold undelivered messages */
    fun busyMailboxes(): List<String>

    /** OracleHooks.report(): hooked calls per label, unused tape entries, tape failures (JSON) */
    fun report(): String?

    fun flush()

    interface Loop {
        fun <T> run(label: String, block: suspend CoroutineScope.() -> T): T
    }

    object None : HooksBridge {
        override val active = false
        override fun install(loop: Loop, schedulerMillis: () -> Long) = Unit
        override fun busyMailboxes(): List<String> = emptyList()
        override fun report(): String? = null
        override fun flush() = Unit
    }

    companion object {
        fun load(): HooksBridge = try {
            Class.forName("oracle.HooksBridgeImpl").getDeclaredConstructor().newInstance() as HooksBridge
        } catch (e: ClassNotFoundException) {
            None
        }
    }
}
