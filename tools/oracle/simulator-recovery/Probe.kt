package com.malinskiy.marathon.apple.ios.device

import com.google.gson.Gson
import com.malinskiy.marathon.apple.ios.StartupPhasesTest
import java.io.File

fun emit(name: String, value: Any) = println(Gson().toJson(linkedMapOf("case" to name, "value" to value)))
fun main(args: Array<String>) {
    val scratch = File(args[0])
    fun run(type: Class<*>, name: String, vararg parameters: Any) {
        val logger = org.slf4j.LoggerFactory.getLogger("AppleSimulatorProvider") as ch.qos.logback.classic.Logger
        val appender = ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply { list = java.util.concurrent.CopyOnWriteArrayList(); start() }
        logger.addAppender(appender)
        val test = type.getDeclaredConstructor().newInstance()
        val root = File(scratch, "case-${scratch.list()!!.size}").apply { mkdirs() }
        type.getMethod("setRoot", File::class.java).invoke(test, root)
        type.methods.filter { it.getAnnotation(org.junit.jupiter.api.BeforeEach::class.java) != null }.forEach { it.invoke(test) }
        try {
            val method = type.methods.single { it.name == name }
            method.invoke(test, *parameters)
            val evidence = linkedMapOf<String, Any>("passed" to true)
            type.declaredFields.firstOrNull { it.name == "events" }?.apply { isAccessible = true }?.get(test)?.let { raw ->
                val bySerial = mutableMapOf<String,java.util.IdentityHashMap<Any,Int>>()
                evidence["events"] = (raw as List<*>).map { event ->
                    val device = when(event) {
                        is com.malinskiy.marathon.device.DeviceProvider.DeviceEvent.DeviceConnected -> event.device
                        is com.malinskiy.marathon.device.DeviceProvider.DeviceEvent.DeviceDisconnected -> event.device
                        else -> error("unknown event")
                    }
                    val identities = bySerial.getOrPut(device.serialNumber) { java.util.IdentityHashMap() }
                    mapOf("kind" to event.javaClass.simpleName, "serial" to device.serialNumber, "allocation" to identities.getOrPut(device) { identities.size + 1 })
                }.sortedBy { it["serial"].toString() }
            }
            type.declaredFields.firstOrNull { it.name == "connections" }?.apply { isAccessible = true }?.get(test)?.let { connections ->
                evidence["references"] = org.mockito.kotlin.mockingDetails(connections).invocations.filter { it.method.name in setOf("create", "createOrThrow", "connectWorker", "dispose") }.groupingBy { it.method.name }.eachCount().toSortedMap()
            }
            type.declaredFields.firstOrNull { it.name == "worker" }?.apply { isAccessible = true }?.get(test)?.let { worker ->
                if(worker is FakeSimulatorWorker) evidence["commands"] = worker.commands.groupingBy { it }.eachCount().entries.sortedBy { it.key.joinToString("\u0000") }.map { mapOf("argv" to it.key,"count" to it.value) }
            }
            evidence["logs"] = appender.list.map { mapOf("level" to it.level.toString(), "message" to it.formattedMessage, "error" to it.throwableProxy?.className) }.filterNot { it["message"]!!.contains("replaced existing device") || it["message"]!!.startsWith("Processing updates") || it["message"]!!.contains("Using dynamic") }.filterNot { it["message"]!!.startsWith("Connected to worker") }
            emit(name + parameters.joinToString(prefix="[", postfix="]"), evidence)
        } catch (e: java.lang.reflect.InvocationTargetException) {
            throw e.targetException
        } finally {
            logger.detachAppender(appender)
            type.methods.filter { it.getAnnotation(org.junit.jupiter.api.AfterEach::class.java) != null }.forEach { it.invoke(test) }
        }
    }
    supplemental(scratch)
    for (type in listOf(AppleSimulatorProviderReconnectTest::class.java, AppleSimulatorProviderWorkerRetryTest::class.java)) {
        for (method in type.methods.filter { it.getAnnotation(org.junit.jupiter.api.Test::class.java) != null }.sortedBy { it.name }) run(type, method.name)
    }
    for (name in listOf("a late simulator prepared after the late join cut-off leaves the run before its first batch", "a simulator that fails to boot is set up again by its provider, as often as a lost one")) run(StartupPhasesTest::class.java, name)
}
