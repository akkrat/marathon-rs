@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package oracle.pool
import com.malinskiy.marathon.analytics.internal.pub.*
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.strategy.PoolingStrategyConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.progress.PoolProgressAccumulator
import com.malinskiy.marathon.test.StubDevice
import com.malinskiy.marathon.test.TestBatch
import com.malinskiy.marathon.test.Test
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.test.*
import org.mockito.kotlin.mock
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import java.io.File
import java.time.Instant

class ProbeDevice(override val serialNumber:String,override val model:String="test",val duration:Long=0,val holdPrepare:Boolean=false):Device by StubDevice(0,0,serialNumber=serialNumber) {
    var executions=0
    override suspend fun prepare(configuration:Configuration) {if(holdPrepare) awaitCancellation()}
    override suspend fun execute(configuration:Configuration,devicePoolId:DevicePoolId,testBatch:TestBatch,deferred:CompletableDeferred<TestBatchResults>) {
        executions++;if(duration<0) awaitCancellation() else delay(duration)
        deferred.complete(TestBatchResults(this,testBatch.tests.map {TestResult(it,toDeviceInfo(),testBatch.id,TestStatus.PASSED,0,1)}, emptyList(),emptyList()))
    }
}
class Provider:DeviceProvider {
    val channel=Channel<DeviceProvider.DeviceEvent>(Channel.UNLIMITED)
    override suspend fun initialize() {}
    override suspend fun borrow():Device=error("unused")
    override suspend fun terminate() {channel.close()}
    override fun subscribe()=channel
}
class Recording:Tracker {
    val connected=mutableListOf<String>();val disconnected=mutableListOf<String>()
    override fun deviceConnected(poolId:DevicePoolId,device:DeviceInfo) {connected.add("${poolId.name}:${device.serialNumber}")}
    override fun deviceDisconnected(poolId:DevicePoolId,device:DeviceInfo) {disconnected.add("${poolId.name}:${device.serialNumber}")}
    override fun test(poolId:DevicePoolId,device:DeviceInfo,testResult:TestResult,final:Boolean) {}
    override fun deviceProviderInit(serialNumber:String,startTime:Instant,finishTime:Instant) {}
    override fun devicePreparing(serialNumber:String,startTime:Instant,finishTime:Instant) {}
}
fun config(timeout:Long=1000,models:Boolean=false,includes:List<String> = emptyList(),excludes:List<String> = emptyList()) = Configuration.Builder("",File("")).apply {
    vendorConfiguration=VendorConfiguration.StubVendorConfiguration;analyticsTracking=false;deviceInitializationTimeoutMillis=timeout
    includeSerialRegexes=includes.map {Regex(it)};excludeSerialRegexes=excludes.map {Regex(it)}
    if(models) poolingStrategy=PoolingStrategyConfiguration.ModelPoolingStrategyConfiguration
}.build()
fun field(o:Any,name:String):Any? = o.javaClass.getDeclaredField(name).let {it.isAccessible=true;it.get(o)}
val shard=TestShard(listOf(Test("pkg","Scheduler","test1",emptyList())))
fun schedulerProbes(output:File) {
    val rows=mutableListOf<Map<String,Any?>>()
    fun scenario(name:String,timeout:Long=1000,models:Boolean=false,includes:List<String> = emptyList(),excludes:List<String> = emptyList(),body:suspend TestScope.(Provider,com.malinskiy.marathon.execution.Scheduler)->Unit) {
        Trace.reset()
        runTest {
            Trace.clock={testScheduler.currentTime}
            val parent=Job();val provider=Provider();val tracker=Recording()
            val scheduler=com.malinskiy.marathon.execution.Scheduler(provider,mock(),config(timeout,models,includes,excludes),shard,Track().add(tracker),mock(),null,StandardTestDispatcher(testScheduler)+parent)
            var result:Any?=null;var ended:Long?=null
            val execution=launch {try {result=scheduler.execute()}catch(e:Exception){result=mapOf("exception" to e.javaClass.name,"message" to e.message)};ended=testScheduler.currentTime}
            runCurrent();body(provider,scheduler);advanceUntilIdle()
            @Suppress("UNCHECKED_CAST") val pools=field(scheduler,"pools") as Map<DevicePoolId,DevicePoolActor>
            rows.add(mapOf("name" to name,"result" to result,"ended" to ended,"subscriptionAlive" to parent.children.any {it.isActive},"pools" to pools.keys.map {it.name}.sorted(),"connected" to tracker.connected,"disconnected" to tracker.disconnected,"events" to Trace.events.toList()))
            (field(scheduler,"job") as Job).cancel();parent.cancel();execution.cancel();runCurrent()
        }
    }
    for(timeout in listOf(0L,-1L,250L)) scenario("timeout-$timeout",timeout) {_,_->}
    for(at in listOf(0L,99L,100L,250L)) scenario("first-$at") {p,_->
        advanceTimeBy(at);runCurrent();p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(ProbeDevice("serial-A")));runCurrent()
    }
    scenario("late-connect") {p,_->
        val first=ProbeDevice("serial-A");val late=ProbeDevice("serial-B")
        p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(first));runCurrent()
        advanceTimeBy(200);runCurrent();p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(late));runCurrent()
        check(first.executions==1 && late.executions==0)
    }
    scenario("broadcast",models=true) {p,_->
        val a=ProbeDevice("serial-A","A",holdPrepare=true);val b=ProbeDevice("serial-B","B",holdPrepare=true)
        p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(a));p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(b));runCurrent()
        p.channel.send(DeviceProvider.DeviceEvent.DeviceDisconnected(a))
        runCurrent();p.channel.send(DeviceProvider.DeviceEvent.DeviceDisconnected(b));runCurrent()
    }
    scenario("short-circuit",models=true) {p,s->
        p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(ProbeDevice("serial-A","A",holdPrepare=true)))
        p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(ProbeDevice("serial-B","B",holdPrepare=true)));runCurrent()
        @Suppress("UNCHECKED_CAST") val pools=field(s,"pools") as Map<DevicePoolId,DevicePoolActor>
        pools.values.forEach {it.send(DevicePoolMessage.FromScheduler.Terminate)};runCurrent()
    }
    scenario("late-pool",models=true) {p,_->
        p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(ProbeDevice("serial-A","A",duration=200)));runCurrent()
        advanceTimeBy(150);runCurrent();p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(ProbeDevice("serial-B","B",duration=400)));runCurrent()
    }
    val matrix=listOf(Triple(emptyList<String>(),emptyList<String>(),"abc"),Triple(listOf("b"),emptyList(),"abc"),Triple(listOf("a.*"),emptyList(),"abc"),Triple(listOf("a.*"),listOf("abc"),"abc"),Triple(emptyList(),listOf("b"),"abc"),Triple(listOf("x","abc"),listOf("x"),"abc"))
    for((i,m) in matrix.withIndex()) scenario("filter-$i",250,includes=m.first,excludes=m.second) {p,_->val d=ProbeDevice(m.third);p.channel.send(DeviceProvider.DeviceEvent.DeviceConnected(d));runCurrent();p.channel.send(DeviceProvider.DeviceEvent.DeviceDisconnected(d));runCurrent()}
    // C27: keep a second device alive so the original pool survives replacement.
    Trace.reset();runTest {
        Trace.clock={testScheduler.currentTime};val parent=Job();val c=config();val id=DevicePoolId("C27")
        val progress=PoolProgressAccumulator(id,shard,c,Track());val pool=DevicePoolActor(id,c,progress,mock(),shard,mock(),parent,StandardTestDispatcher(testScheduler),null)
        val old=ProbeDevice("same",holdPrepare=true);val new=ProbeDevice("same",holdPrepare=true);val keeper=ProbeDevice("keeper",holdPrepare=true)
        pool.send(DevicePoolMessage.FromScheduler.AddDevice(old));pool.send(DevicePoolMessage.FromScheduler.AddDevice(keeper));runCurrent()
        pool.send(DevicePoolMessage.FromScheduler.RemoveDevice(old));pool.send(DevicePoolMessage.FromScheduler.AddDevice(new));runCurrent()
        pool.send(DevicePoolMessage.FromScheduler.RemoveDevice(old));runCurrent()
        @Suppress("UNCHECKED_CAST") val devices=field(pool,"devices") as Map<String,Any>
        check(devices.keys==setOf("keeper"));rows.add(mapOf("name" to "C27","devices" to devices.keys.toList(),"events" to Trace.events.toList()));parent.cancel();runCurrent()
    }
    // B4: enqueue the final outcome and Terminated before either actor is dispatched.
    Trace.reset();runTest {
        Trace.clock={testScheduler.currentTime};val parent=Job();val c=config();val id=DevicePoolId("B4")
        val progress=PoolProgressAccumulator(id,shard,c,Track());val pool=DevicePoolActor(id,c,progress,mock(),shard,mock(),parent,StandardTestDispatcher(testScheduler),null)
        val d=ProbeDevice("serial-A",duration=-1);pool.send(DevicePoolMessage.FromScheduler.AddDevice(d));runCurrent()
        pool.send(DevicePoolMessage.FromDevice.CompletedTestBatch(d,TestBatchResults(d,shard.tests.map {TestResult(it,d.toDeviceInfo(),"batch",TestStatus.PASSED,0,1)},emptyList(),emptyList())))
        pool.send(DevicePoolMessage.FromDevice.Terminated(d));runCurrent()
        check(progress.aggregateResult());rows.add(mapOf("name" to "B4","result" to true,"events" to Trace.events.toList()));parent.cancel();runCurrent()
    }
    File(output,"scheduler.jsonl").writeText(rows.joinToString("\n",postfix="\n") {jacksonObjectMapper().writeValueAsString(it)})
}
