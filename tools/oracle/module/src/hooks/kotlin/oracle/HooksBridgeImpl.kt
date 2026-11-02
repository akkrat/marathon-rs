package oracle

import kotlinx.coroutines.CoroutineScope
import oracle.hooks.OracleHooks

/** [HooksBridge] over OracleHooks; compiled only in the patched clone, where `:oracle-hooks` exists */
@Suppress("unused") // loaded by name from HooksBridge.load
class HooksBridgeImpl : HooksBridge {
    override val active: Boolean get() = OracleHooks.active

    override fun install(loop: HooksBridge.Loop, schedulerMillis: () -> Long) {
        if (!OracleHooks.active) return
        OracleHooks.installReplayLoop(object : OracleHooks.ReplayLoop {
            override fun <T> run(label: String, block: suspend CoroutineScope.() -> T): T = loop.run(label, block)
        }, schedulerMillis)
    }

    override fun busyMailboxes(): List<String> = OracleHooks.busyMailboxes()

    override fun report(): String? = if (OracleHooks.active) OracleHooks.report() else null

    override fun flush() = OracleHooks.flush()
}
