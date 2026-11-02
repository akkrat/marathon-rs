@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package oracle.pool

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.Level
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.AppenderBase
import org.slf4j.LoggerFactory
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.execution.queue.QueueMessage
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.test.TestBatch
import java.io.File

@Target(AnnotationTarget.FUNCTION)
@Retention(AnnotationRetention.RUNTIME)
annotation class ProbeTest
object Trace {
    var clock: () -> Long = {0L}
    val events=mutableListOf<Any>()
    val characterization=mutableListOf<List<Long>>()
    val tracks=mutableListOf<String>()
    val ids=linkedMapOf<String,String>()
    fun normalize(s: String): String = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").replace(s) { ids.getOrPut(it.value) {"batch-${ids.size+1}"} }
    fun message(target: String,pool: String,msg: Any) {
        val serial = msg.javaClass.declaredFields.firstOrNull {it.name=="device"}?.let {it.isAccessible=true;when(val d=it.get(msg)){is Device->d.serialNumber;is DeviceInfo->d.serialNumber;else->null}}
        val batch = msg.javaClass.declaredFields.firstOrNull {it.name=="batch"}?.let {it.isAccessible=true;(it.get(msg) as? TestBatch)?.tests?.map {it.method}}
        events.add(mapOf("t" to clock(),"target" to target,"pool" to pool,"message" to msg.javaClass.simpleName,"device" to serial,"batch" to batch))
    }
    fun terminated(pool: String) {events.add(mapOf("t" to clock(),"terminated" to pool))}
    fun reset() {events.clear();tracks.clear();characterization.clear();ids.clear();clock={0L}}
    fun row(name: String) = mapOf("name" to name,"events" to events.toList(),"tracker" to tracks.sorted(),"assertions" to "passed","characterization" to characterization.toList())
}
fun main(args:Array<String>) {
    val output=File(args[0]);output.mkdirs()
    val appender=object:AppenderBase<ILoggingEvent>() {override fun append(e: ILoggingEvent) {
        if(e.loggerName.startsWith("DevicePoolActor") || e.loggerName.startsWith("QueueActor") || e.loggerName=="Scheduler" || e.loggerName.startsWith("PoolProgressAccumulator"))
            Trace.events.add(mapOf("t" to Trace.clock(),"logger" to e.loggerName,"level" to e.level.toString(),"message" to Trace.normalize(e.formattedMessage)))
    }}.apply {start()}
    (LoggerFactory.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME) as Logger).apply {level=Level.DEBUG;addAppender(appender)}
    val mapper=jacksonObjectMapper()
    File(output,"tests.jsonl").bufferedWriter().use { writer ->
        for (type in listOf(DevicePoolActorSchedulingTest::class.java,DevicePoolActorBatchAccountingTest::class.java,DevicePoolActorSchedulingCharacterizationTest::class.java)) {
            for (method in type.declaredMethods.filter {it.isAnnotationPresent(ProbeTest::class.java)}.sortedBy {it.name}) {
                Trace.reset();val instance=type.getDeclaredConstructor().newInstance()
                try {method.invoke(instance)} catch(e:java.lang.reflect.InvocationTargetException) {throw e.targetException}
                finally {type.methods.firstOrNull {it.name=="teardown"}?.invoke(instance)}
                writer.write(mapper.writeValueAsString(Trace.row(method.name.replace(' ','_').lowercase())));writer.newLine()
            }
        }
    }
    schedulerProbes(output)
}
