@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.test.*

fun row(key: String, value: Any?) = println("{\"case\":\"$key\",\"value\":\"$value\"}")
fun main() = runTest {
    for (ms in listOf(0L, -1L, Long.MIN_VALUE, 1L, Long.MAX_VALUE / 2, Long.MAX_VALUE)) {
        val d = launch { delay(ms) }
        runCurrent()
        row("delay:$ms", if (d.isCompleted) "immediate" else "pending")
        d.cancelAndJoin()
        var entered = false
        val t = launch { withTimeoutOrNull(ms) { entered = true; delay(Long.MAX_VALUE) } }
        runCurrent()
        row("timeout:$ms", if (t.isCompleted) "immediate:$entered" else "pending:$entered")
        t.cancelAndJoin()
    }
    row("nested-timeout", try { withTimeoutOrNull(100) { withTimeout(1) { delay(10) } }; "null" } catch (e: TimeoutCancellationException) { "inner" })
    val one = Job(); val a = Job(one)
    val single = one.children.iterator(); row("single:first", single.next() === a)
    val b = Job(one); a.complete(); row("single:attached-observed", single.hasNext()); b.complete(); one.complete()
    val many = Job(); val c = Job(many); val d = Job(many)
    val list = many.children.iterator(); list.next(); val e = Job(many); c.complete()
    row("list:second", list.next() === d); d.complete()
    row("list:attached-observed", list.hasNext() && list.next() === e); e.complete(); many.complete()
    val cell = CompletableDeferred<Int>(); row("deferred:value-wins", cell.complete(7)); row("deferred:error-loses", cell.completeExceptionally(Exception("later"))); row("deferred:repeat", "${cell.await()},${cell.await()}")
    val cancelled = CompletableDeferred<Int>(); cancelled.cancel(); row("deferred:cancel-wins", !cancelled.complete(8))
    val waiting = CompletableDeferred<Int>(); val waiter = launch { waiting.await() }; runCurrent(); waiter.cancelAndJoin(); row("deferred:await-isolation", waiting.complete(9) && waiting.await() == 9)
    val ch = Channel<Int>(Channel.UNLIMITED); ch.send(1); ch.close(); row("channel:close-drains", ch.receive()); row("channel:send-closed", ch.trySend(2).isClosed)
    val discard = Channel<Int>(Channel.UNLIMITED); discard.send(1); discard.cancel(); row("channel:cancel-discards", discard.tryReceive().isClosed)
    val parent = Job(); val child = Job(parent); child.cancel(); row("job:child-cancel-isolated", parent.isActive); parent.complete()
    val standalone = Job(); Job(standalone).complete(); row("job:explicit-completion", standalone.isActive); standalone.complete()
    row("job:cancel-completes-standalone", Job().also { it.cancel() }.isCompleted)
}
