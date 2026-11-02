package com.malinskiy.marathon.apple.ios.device

import com.google.gson.Gson
import kotlinx.coroutines.runBlocking
import java.time.Duration
import java.util.concurrent.ConcurrentHashMap

fun main() = runBlocking {
    val gson = Gson()
    val worker = FakeSimulatorWorker().apply {
        add("b3", "b1", "b2")
        add("custom", deviceType = "custom-type", runtime = "custom-runtime")
        simulators.getValue("b2").available = false
    }
    val commands = listOf(
        FakeSimulatorWorker.ENV, FakeSimulatorWorker.UNAME, FakeSimulatorWorker.XCODEBUILD_VERSION,
        listOf(FakeSimulatorWorker.SYSTEM_PROFILER, "any"), FakeSimulatorWorker.LIST,
        *listOf("HOME", "SIMULATOR_MODEL_IDENTIFIER", "SIMULATOR_RUNTIME_VERSION", "OTHER")
            .map { listOf("xcrun", "simctl", "getenv", "b1", it) }.toTypedArray(),
        listOf("xcrun", "simctl", "getenv", "missing", "HOME"),
        listOf("xcrun", "simctl", "delete", "missing"), listOf("unsupported")
    )
    worker.connected = false
    for (command in commands) {
        val result = worker.execute(command, Duration.ofSeconds(5), Duration.ofSeconds(5), emptyMap(), null).await()
        println(gson.toJson(mapOf("command" to command, "stdout" to result.stdout, "stderr" to result.stderr, "exit_code" to result.exitCode)))
    }
    System.err.println("ConcurrentHashMap b keys: " + ConcurrentHashMap<String, Int>().apply {
        listOf("b1", "b2", "b3").forEach { put(it, 1) }
    }.keys.toList())
}
