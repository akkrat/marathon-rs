package com.malinskiy.marathon.apple.ios.device

import com.google.gson.Gson
import com.malinskiy.marathon.apple.ios.StartupPhasesTest
import java.io.File

fun emit(name: String, value: Any) = println(Gson().toJson(linkedMapOf("case" to name, "value" to value)))
fun main(args: Array<String>) {
    val scratch = File(args[0])
    fun run(type: Class<*>, name: String, vararg parameters: Any) {
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
            emit(name + parameters.joinToString(prefix="[", postfix="]"), evidence)
        } catch (e: java.lang.reflect.InvocationTargetException) {
            throw e.targetException
        } finally {
            type.methods.filter { it.getAnnotation(org.junit.jupiter.api.AfterEach::class.java) != null }.forEach { it.invoke(test) }
        }
    }
    supplemental(scratch)
    for (n in listOf(1,2,3,6)) run(StartupPhasesTest::class.java, "a worker sets up as many simulators at a time as provisioning concurrency allows", n)
    run(StartupPhasesTest::class.java, "provisioning and preparation log their timings without debug")
    run(AppleSimulatorProviderArtifactTest::class.java, "provider drains artifacts before device disposal and transport release")
    for (name in listOf("a worker planning failure releases its connection and does not cancel healthy workers", "initialize logs the effective xcresulttool timeout")) run(AppleSimulatorProviderTest::class.java, name)
    for (name in listOf("an update that adds a simulator sets up only that one and leaves the batches of the others running", "an update that removes a simulator disposes only that one, after its batch, and it is set up again once added back")) run(AppleSimulatorProviderUpdateTest::class.java, name)
    for (environment in listOf("local", "ssh")) run(WorkerProvisioningContextTest::class.java, "four unchanged simulators share one inventory and one set of worker facts", environment)
    for (n in listOf(1,4,8,16)) run(WorkerProvisioningContextTest::class.java, "harness command counts for existing simulators and new profiles of a worker", n)
    for (name in listOf("new profiles are planned from the shared inventory and set up from a lookup after creation", "a simulator deleted after the inventory was taken is not set up", "unavailable simulators are neither planned nor re-provisioned", "a slow inventory of one worker does not hold up other workers")) run(WorkerProvisioningContextTest::class.java, name)
}
