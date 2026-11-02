package com.malinskiy.marathon.apple.ios.device

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.analytics.internal.pub.Track
import com.malinskiy.marathon.apple.AppleTestBundleIdentifier
import com.malinskiy.marathon.apple.bin.xcrun.simctl.model.*
import com.malinskiy.marathon.apple.cmd.CommandExecutor
import com.malinskiy.marathon.apple.configuration.*
import com.malinskiy.marathon.apple.device.ConnectionFactory
import com.malinskiy.marathon.apple.ios.AppleSimulatorDevice
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.apple.SshConfiguration
import com.malinskiy.marathon.config.vendor.apple.RsyncConfiguration
import com.malinskiy.marathon.config.vendor.apple.SshAuthentication
import com.malinskiy.marathon.config.vendor.apple.ios.LifecycleConfiguration
import com.malinskiy.marathon.device.DeviceProvider.DeviceEvent
import com.malinskiy.marathon.time.SystemTimer
import java.io.File
import java.time.Clock
import java.time.Duration
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import org.mockito.kotlin.*

fun supplemental(root: File) = runBlocking {
    for (ready in listOf(false,true)) {
        val queue=java.util.concurrent.LinkedBlockingQueue<Runnable>()
        val dispatcher=object:CoroutineDispatcher(){ override fun dispatch(context:kotlin.coroutines.CoroutineContext,block:Runnable){queue.add(block)} }
        val entered=java.util.concurrent.CountDownLatch(1)
        val gate=java.util.concurrent.CountDownLatch(1)
        val released=java.util.concurrent.CountDownLatch(1)
        val count=java.util.concurrent.atomic.AtomicInteger()
        val result=async(dispatcher,start=CoroutineStart.UNDISPATCHED){
            detached({ _:Int -> count.incrementAndGet();released.countDown() }) { entered.countDown();gate.await();1 }
        }
        check(entered.await(10,java.util.concurrent.TimeUnit.SECONDS))
        if(ready){
            gate.countDown()
            val resume=queue.poll(10,java.util.concurrent.TimeUnit.SECONDS) ?: error("result did not become ready")
            result.cancel();resume.run()
        }else{
            result.cancel()
            val resume=queue.poll(10,java.util.concurrent.TimeUnit.SECONDS) ?: error("cancellation did not resume caller")
            resume.run();check(result.isCompleted);gate.countDown()
        }
        check(released.await(10,java.util.concurrent.TimeUnit.SECONDS))
        check(result.isCancelled && count.get()==1)
        emit(if(ready)"detached-ready-cancel" else "detached-cancel-blocked",mapOf("cancelled" to result.isCancelled,"released" to count.get()))
    }

    val mapper=ObjectMapper(YAMLFactory()).registerModule(KotlinModule.Builder().build())
    val omitted=mapper.readValue("type: iOS\n",VendorConfiguration::class.java) as VendorConfiguration.IOSConfiguration
    val defaults=omitted.workerConnection
    var offset=0L
    val offsets=mutableListOf(0L)
    for(attempt in 1..4){offset+=defaults.pauseAfter(attempt)!!.toMillis();offsets.add(offset)}
    emit("omitted-worker-defaults",mapOf("attempts" to defaults.attempts,"backoffMillis" to defaults.backoff.toMillis(),"intervalMillis" to defaults.retryInterval.toMillis(),"lateJoin" to defaults.lateJoin,"cutoffMillis" to defaults.lateJoinCutoff.toMillis(),"offsets" to offsets))
    val profile=AppleTarget.SimulatorProfile("iPhone 15 Pro",null,"test")
    val pending=PendingWorker(listOf(profile,profile))
    val first=pending.take()!!.size
    check(pending.offer(listOf(profile)))
    pending.close()
    val offered=pending.offer(listOf(profile))
    val retained=pending.take()!!.size
    val empty=pending.take()==null
    emit("pending-worker-drain-close",mapOf("firstCount" to first,"offerAfterClose" to offered,"retainedCount" to retained,"emptyTake" to empty,"offerAfterEmpty" to pending.offer(listOf(profile))))

    for (connectedLoss in listOf(false, true)) {
        val dir=File(root,"leases-$connectedLoss").apply{mkdirs()}
        val transport=Transport.Ssh("127.0.0.1",22222,SshAuthentication.PublicKeyAuthentication("fixture",File("/fixture/key")),false)
        val old=FakeSimulatorWorker(hostId=transport.id()).apply{add("a")}
        val fresh=FakeSimulatorWorker(hostId=transport.id()).apply{add("a")}
        val current=AtomicReference(old)
        val vendor=VendorConfiguration.IOSConfiguration(ssh=SshConfiguration(shareWorkerConnection=true),lifecycleConfiguration=LifecycleConfiguration(onDispose=emptySet()))
        val config=Configuration.Builder("test",dir).apply{vendorConfiguration=vendor}.build()
        val connections=ConnectionFactory(config,vendor.ssh,RsyncConfiguration(),Duration.ofMillis(100),8){{current.get()}}
        val factory=mock<SimulatorFactory>()
        val failing=java.util.concurrent.ConcurrentHashMap.newKeySet<AppleSimulatorDevice>()
        val disposed=java.util.concurrent.ConcurrentHashMap.newKeySet<AppleSimulatorDevice>()
        whenever(factory.create(any(),any(),any(),any(),any())).doSuspendableAnswer { call ->
            val executor=call.getArgument<CommandExecutor>(1)
            mock<AppleSimulatorDevice>{
                on{udid}doReturn "a";on{this.transport}doReturn transport;on{serialNumber}doReturn "a@${transport.id()}";on{commandExecutor}doReturn executor
                on{lost}doAnswer{it.mock in failing};on{this.disposed}doAnswer{it.mock in disposed}
                onBlocking{awaitSessionClosed(any())}doReturn true
                onBlocking{disposeAndJoin()}.doSuspendableAnswer{failing.remove(it.mock);disposed.add(it.mock as AppleSimulatorDevice);Unit}
            }
        }
        val provider=AppleSimulatorProvider(config,vendor,AppleTestBundleIdentifier(),GsonBuilder().registerTypeAdapter(SimctlDeviceList::class.java,SimctlDeviceListDeserializer()).create(),ObjectMapper(YAMLFactory()).registerModule(KotlinModule.Builder().build()),Track(),SystemTimer(Clock.systemUTC()),connections,factory)
        val file=File(dir,"Marathondevices").apply{writeText("workers:\n- transport: {type: ssh, addr: 127.0.0.1, port: 22222, checkReachability: false, authentication: {type: publicKey, username: fixture, key: /fixture/key}}\n  devices: [{type: simulator, udid: a}]\n")}
        // Initialize the source independently of transport-file authentication rendering.
        val tracker=AppleSimulatorProvider::class.java.getDeclaredField("deviceTracker").apply{isAccessible=true}.get(provider) as DeviceTracker
        provider.processUpdate(Marathondevices(listOf(Worker(transport,listOf(AppleTarget.Simulator("a"))))))
        val first=(withTimeout(5000){provider.subscribe().receive()} as DeviceEvent.DeviceConnected).device as AppleSimulatorDevice
        if(connectedLoss) failing.add(first) else { current.set(fresh);old.connected=false }
        // Invoke the actual monitoring scan with a structured scope; the recovery remains its child.
        val reconnect=AppleSimulatorProvider::class.java.declaredMethods.single{it.name=="reconnect"}.apply{isAccessible=true}
        suspendCoroutineCall { continuation -> reconnect.invoke(provider,this,continuation) }
        withTimeout(5000){while(provider.subscribe().receive() !is DeviceEvent.DeviceConnected) Unit}
        provider.terminate()
        val leases=ConnectionFactory::class.java.getDeclaredField("leases").apply{isAccessible=true}.get(connections) as Map<*,*>
        val remaining=leases.values.map { lease -> lease!!::class.java.getDeclaredField("users").apply{isAccessible=true}.get(lease) as Int }.sum()
        emit(if(connectedLoss)"connected-loss-leases" else "A90-worker-loss-leases",mapOf("remainingLeases" to leases.size,"remainingUsers" to remaining,"oldRemaining" to leases.containsKey(old),"freshRemaining" to leases.containsKey(fresh)))
        // The witness releases its intentionally leaked lease after observing it.
        if(leases.containsKey(old))connections.dispose(old)
    }
}
private suspend fun suspendCoroutineCall(body: (kotlin.coroutines.Continuation<Unit>)->Any?): Unit = kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn { continuation -> body(continuation) }
