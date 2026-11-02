@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
fun row(key: String, value: Any?) = println("{\"case\":\"$key\",\"value\":\"$value\"}")
fun main() = runTest {
    row("timeout:waits-children", withTimeoutOrNull(5) { launch { delay(10) }; "body" } == null)
    row("timeout:equal-delay", withTimeoutOrNull(5) { delay(5); "body" } == null)
    val parent = Job(); parent.cancel(); var returned = false
    launch(parent, start=CoroutineStart.UNDISPATCHED) { delay(0); returned = true }.join()
    row("delay:cancelled-zero", returned)
    val seen = mutableListOf<Int>(); val job = Job()
    job.invokeOnCompletion { seen.add(1) }; job.invokeOnCompletion { seen.add(2) }.dispose()
    job.complete(); job.invokeOnCompletion { seen.add(3) }
    row("job:callback-order", seen.joinToString(","))
    var failure = "none"
    val scope = CoroutineScope(SupervisorJob() + StandardTestDispatcher(testScheduler) + CoroutineExceptionHandler { _, error -> failure = error.message ?: "null" })
    val task = scope.launch {
        launch { delay(1); error("first") }
        launch { try { delay(100) } finally { error("second") } }
    }
    advanceUntilIdle(); task.join(); row("job:first-failure", failure); scope.cancel()
    row("timeout:zero-message", try { withTimeout(0) { } ; "none" } catch (e: TimeoutCancellationException) { e.message })
    var maxTimeout = false
    val maxTask = launch { maxTimeout = withTimeoutOrNull(Long.MAX_VALUE) { awaitCancellation() } == null }
    runCurrent()
    val forever = launch { delay(Long.MAX_VALUE) }
    runCurrent(); testScheduler.advanceTimeBy(Long.MAX_VALUE - currentTime); runCurrent()
    row("delay:max-after-advance", forever.isCompleted); forever.cancelAndJoin()
    row("timeout:max-after-advance", maxTimeout); maxTask.cancelAndJoin()
}
