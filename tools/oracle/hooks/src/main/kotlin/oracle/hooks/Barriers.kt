package oracle.hooks

import kotlinx.coroutines.CompletableDeferred

/** Ordered completion barriers for live transports. Unlike core replay this does not virtualize socket I/O. */
internal class Barriers(private val script: List<String>, private val failure: (String) -> Nothing = OracleHooks::fail) {
    private val lock = Any()
    private var cursor = 0
    private val waiting = HashMap<String, CompletableDeferred<Unit>>()

    suspend fun <T> event(label: String, key: String, block: suspend () -> T): T {
        val token = "$label:$key"
        val ready = synchronized(lock) {
            if (token !in script.drop(cursor)) failure("unexpected or exhausted barrier $token")
            if (token in waiting) failure("duplicate pending barrier $token")
            CompletableDeferred<Unit>().also {
                waiting[token] = it
                if (script[cursor] == token) it.complete(Unit)
            }
        }
        try {
            ready.await()
            return block()
        } finally {
            synchronized(lock) {
                waiting.remove(token)
                // Cancellation before admission does not consume a future event.
                if (script.getOrNull(cursor) == token) {
                    cursor++
                    waiting[script.getOrNull(cursor)]?.complete(Unit)
                }
            }
        }
    }

    fun remaining(): List<String> = synchronized(lock) { script.drop(cursor) }
}
