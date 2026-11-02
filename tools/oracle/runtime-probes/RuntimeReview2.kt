@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class, kotlinx.coroutines.ObsoleteCoroutinesApi::class)
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import kotlinx.coroutines.test.*
fun row(key: String, value: Any?) = println("{\"case\":\"$key\",\"value\":\"$value\"}")
fun Throwable.result() = javaClass.simpleName + ":" + message
suspend fun Channel<Int>.result(): String {
    val received = receiveCatching()
    received.exceptionOrNull()?.let { throw it }
    return if (received.isClosed) "closed" else "got:" + received.getOrThrow()
}
fun main() = runTest {
    // Both notifications happen before the already-suspended waiter resumes.
    for (state in listOf("failed", "successful", "cancelled")) {
        for (cancelFirst in listOf(false, true)) {
            val cell = CompletableDeferred<Int>()
            val waiter = Job()
            var result = "none"
            val task = launch(waiter, start = CoroutineStart.UNDISPATCHED) {
                try { result = "got:" + cell.await() }
                catch (error: Throwable) { result = error.result() }
            }
            if (cancelFirst) waiter.cancel(CancellationException("waiter"))
            when (state) {
                "failed" -> cell.completeExceptionally(IllegalStateException("stored-failure"))
                "successful" -> cell.complete(42)
                "cancelled" -> cell.cancel(CancellationException("stored-cancel"))
            }
            if (!cancelFirst) waiter.cancel(CancellationException("waiter"))
            runCurrent(); task.join()
            row("deferred:$state:cancel-first=$cancelFirst", result)
        }
    }
    val root = SupervisorJob()
    val owner = Job(root)
    val scope = CoroutineScope(owner + StandardTestDispatcher(testScheduler))
    var caught = "none"
    var cleanup = false
    var handlerJobIsReceiver = false
    lateinit var scopedChild: Job
    val receiver = scope.actor<Unit>(capacity = Channel.UNLIMITED) {
        for (message in channel) {
            handlerJobIsReceiver = coroutineContext[Job] !== owner
            // Marathon's actor-scoped launch uses the owner as parent.
            scopedChild = scope.launch { delay(Long.MAX_VALUE) }
            try { delay(Long.MAX_VALUE) }
            catch (error: CancellationException) { caught = error.message ?: "null"; throw error }
            finally { withContext(NonCancellable) { delay(1); cleanup = true } }
        }
    }
    receiver.send(Unit); runCurrent()
    val receiverJob = owner.children.first()
    receiverJob.cancel(CancellationException("receiver-only")); runCurrent()
    row("actor:receiver-cancelling-during-cleanup", !receiverJob.isCompleted && receiverJob.isCancelled)
    advanceTimeBy(1); runCurrent(); receiverJob.join()
    row("actor:receiver-completed", receiverJob.isCompleted)
    row("actor:owner-active", owner.isActive)
    row("actor:scoped-child-active", scopedChild.isActive)
    row("actor:handler-job-is-receiver", handlerJobIsReceiver)
    row("actor:caught", caught)
    row("actor:cleanup", cleanup)
    owner.cancel(); root.cancel()
    for (state in listOf("buffered", "closed-buffered", "closed", "failed", "cancelled", "pending")) {
        val channel = Channel<Int>(Channel.UNLIMITED)
        when (state) {
            "buffered" -> channel.trySend(42)
            "closed-buffered" -> { channel.trySend(42); channel.close() }
            "closed" -> channel.close()
            "failed" -> channel.close(IllegalStateException("closed-failure"))
            "cancelled" -> { channel.trySend(42); channel.cancel(CancellationException("channel")) }
        }
        var result = "none"
        launch(Job().also { it.cancel(CancellationException("waiter")) }, start = CoroutineStart.UNDISPATCHED) {
            try { result = channel.result() }
            catch (error: Throwable) { result = error.result() }
        }.join()
        row("channel:ready:$state", result)
        if (state == "pending") { channel.trySend(42); row("channel:pending-unaffected", channel.result()) }
        if (state == "closed-buffered") row("channel:drained", channel.result())
        channel.cancel()
    }
    for (state in listOf("buffered", "closed", "failed")) {
        for (cancelFirst in listOf(false, true)) {
            val channel = Channel<Int>(Channel.UNLIMITED)
            val waiter = Job()
            var result = "none"
            val task = launch(waiter, start = CoroutineStart.UNDISPATCHED) {
                try { result = channel.result() }
                catch (error: Throwable) { result = error.result() }
            }
            if (cancelFirst) waiter.cancel(CancellationException("waiter"))
            when (state) {
                "buffered" -> channel.trySend(42)
                "closed" -> channel.close()
                "failed" -> channel.close(IllegalStateException("closed-failure"))
            }
            if (!cancelFirst) waiter.cancel(CancellationException("waiter"))
            runCurrent(); task.join()
            row("channel:suspended:$state:cancel-first=$cancelFirst", result)
            channel.cancel()
        }
    }
    for (state in listOf("completed", "failed", "pending")) {
        val job = Job()
        when (state) {
            "completed" -> job.complete()
            "failed" -> job.completeExceptionally(IllegalStateException("joined-failure"))
        }
        var result = "none"
        launch(Job().also { it.cancel(CancellationException("waiter")) }, start = CoroutineStart.UNDISPATCHED) {
            try { job.join(); result = "joined" }
            catch (error: Throwable) { result = error.result() }
        }.join()
        row("join:cancelled-waiter:$state", result)
        job.cancel()
    }
}
