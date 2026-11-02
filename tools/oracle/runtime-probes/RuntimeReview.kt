@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class, kotlinx.coroutines.ObsoleteCoroutinesApi::class)
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import kotlinx.coroutines.test.*
fun row(key: String, value: Any?) = println("{\"case\":\"$key\",\"value\":\"$value\"}")
fun main() = runTest {
    val root = SupervisorJob()
    val scope = CoroutineScope(root + StandardTestDispatcher(testScheduler))
    lateinit var child: Job
    val task = scope.launch {
        child = launch { delay(Long.MAX_VALUE) }
        throw CancellationException("body cancelled")
    }
    runCurrent(); task.join(); child.join()
    row("cancelled-body:parent", "${task.isCompleted},${task.isCancelled}")
    row("cancelled-body:child", "${child.isCompleted},${child.isCancelled}")
    row("cancelled-body:supervisor-active", root.isActive)
    val owner = Job(root)
    val receiver = scope.actor<Unit>(context = owner, capacity = Channel.UNLIMITED) {
        for (message in channel) throw CancellationException("handler cancelled")
    }
    receiver.send(Unit); runCurrent()
    row("actor:owner-active", owner.isActive)
    row("actor:owner-children", owner.children.count())
    row("actor:receiver-closed", receiver.isClosedForSend)
    var calls = 0
    val handler = CoroutineExceptionHandler { _, _ -> calls++ }
    val logging = CoroutineScope(root + StandardTestDispatcher(testScheduler) + handler)
    logging.launch { }
    logging.launch { throw CancellationException("cancel") }
    logging.launch { error("failure") }
    runCurrent()
    row("exception-handler:calls", calls)
    calls = 0
    logging.launch {
        val intermediate = Job(coroutineContext[Job])
        launch(intermediate) { error("nested") }
        intermediate.complete()
    }
    runCurrent()
    row("exception-handler:nested-calls", calls)
    calls = 0
    logging.launch {
        try { withTimeout(5) { error("caught") } } catch (_: IllegalStateException) { }
    }
    runCurrent()
    row("exception-handler:caught-calls", calls)
    val order = mutableListOf<String>()
    val parent = Job()
    val callbackChild = Job(parent)
    callbackChild.invokeOnCompletion { order.add("early-child") }
    parent.complete()
    callbackChild.invokeOnCompletion { order.add("late-child") }
    parent.invokeOnCompletion { order.add("parent") }
    callbackChild.complete()
    row("job:parent-child-callbacks", order.joinToString(","))
    for (state in listOf("failed", "successful", "pending", "cancelled")) {
        val cell = CompletableDeferred<Int>()
        when (state) {
            "failed" -> cell.completeExceptionally(IllegalStateException("stored-failure"))
            "successful" -> cell.complete(7)
            "cancelled" -> cell.cancel(CancellationException("stored-cancel"))
        }
        var result = "none"
        launch(Job().also { it.cancel(CancellationException("waiter")) }, start = CoroutineStart.UNDISPATCHED) {
            try { cell.await(); result = "success" }
            catch (error: Throwable) { result = error.javaClass.simpleName + ":" + error.message }
        }.join()
        row("deferred:cancelled-waiter-$state", result)
        cell.cancel()
    }
    root.cancel()
}
