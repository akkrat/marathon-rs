import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.*
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.auth.SshAuthentication as ExecutorAuth
import com.malinskiy.marathon.apple.cmd.remote.rsync.*
import com.malinskiy.marathon.apple.configuration.Transport
import com.malinskiy.marathon.apple.ios.AppleSimulatorDevice
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.apple.*
import com.malinskiy.marathon.log.MarathonLogging
import kotlinx.coroutines.*
import java.io.File
import java.time.Duration
import net.schmizz.sshj.common.SSHException

private val host = "127.0.0.1"
private val port = System.getenv("FW_PORT").toInt()
private val root = File(System.getenv("FW_WORK"))
private fun emit(name: String, value: Any?) = println(com.google.gson.Gson().toJson(mapOf("case" to name, "value" to value)))
private fun failure(e: Throwable) = generateSequence(e) { it.cause }.take(10).map {
    mapOf("class" to it.javaClass.simpleName, "message" to it.message, "reason" to (it as? SSHException)?.disconnectReason?.toString())
}.toList()
private fun connect(password: String = "fake-password", ka: Long = 0): SshjCommandExecutor = SshjCommandExecutorFactory(connectionAttempts=1, keepAliveInterval=Duration.ofMillis(ka),keepAliveMaxCount=2).connect(host,port,ExecutorAuth.PasswordAuthentication("fixture",password),debug=false)
private suspend fun CommandExecutor.run(vararg args: String) = execute(args.toList(),Duration.ofSeconds(10),Duration.ofSeconds(10),emptyMap(),null).await()
private fun auth(key: Boolean): SshAuthentication = if(key) SshAuthentication.PublicKeyAuthentication("fixture",File(System.getenv("FW_KEY"))) else SshAuthentication.PasswordAuthentication("fixture","fake-password")
private fun bridge(key: Boolean, debug: Boolean=false, ka:Long=1500): RsyncFileBridge {
    val cfg=Configuration.Builder("fake-worker",File(root,"reports")).apply {this.debug=debug}.build()
    return RsyncFileBridge(RsyncTarget(host,port),cfg,SshConfiguration(debug=debug,keepAliveInterval=Duration.ofMillis(ka),keepAliveMaxCount=4),RsyncConfiguration(),auth(key))
}
// Invoke the real pushGeneratedFile method without constructing the simulator
// world (WP 0.4b). Only its transport/transfer fields are initialized; no method
// is replaced or copied. The fixture never calls provisioning or disposal.
private fun device(executor:CommandExecutor, bridge:FileBridge, debug:Boolean): AppleSimulatorDevice {
    val field=sun.misc.Unsafe::class.java.getDeclaredField("theUnsafe").apply {isAccessible=true}
    val unsafe=field.get(null) as sun.misc.Unsafe
    val d=unsafe.allocateInstance(AppleSimulatorDevice::class.java) as AppleSimulatorDevice
    fun set(name:String,value:Any) {AppleSimulatorDevice::class.java.getDeclaredField(name).apply{isAccessible=true}.set(d,value)}
    set("transport",Transport.Ssh(host,port,auth(true)))
    set("vendorConfiguration",VendorConfiguration.IOSConfiguration(ssh=SshConfiguration(debug=debug)))
    set("commandExecutor",executor);set("fileBridge",bridge);set("logger",MarathonLogging.logger {});set("serialNumber","fixture")
    return d
}
private suspend fun transfers(ex:CommandExecutor) {
    check(ex.run("mkdir","-p","/tmp/marathon/shared").successful)
    val plist="""<?xml version="1.0"?><plist version="1.0"><dict><key>TestBundlePath</key><string>__TESTROOT__/shared/tests.xctest</string><key>TestHostPath</key><string>/tmp/marathon/shared/runner.app</string><key>Opaque</key><string>do not replace /tmp/marathon in opaque text</string></dict></plist>"""
    val dst="/tmp/marathon/shared/marathon.xctestrun"
    check(ex.writeFile("old".toByteArray(),dst,Duration.ofSeconds(5)))
    check(ex.writeFile(plist.toByteArray(),dst,Duration.ofSeconds(5)))
    check(ex.writeFile(ByteArray(32768){(it%251).toByte()},"/tmp/marathon/max",Duration.ofSeconds(5)))
    check(!ex.writeFile(ByteArray(32769),"/tmp/marathon/oversize-guard",Duration.ofSeconds(5)))
    check(!ex.writeFile(byteArrayOf(1),"/tmp/marathon/with space",Duration.ofSeconds(5)))
    check(ex.run("mkdir","-p","$dst.marathon-tmp").successful)
    check(!ex.writeFile("failed".toByteArray(),dst,Duration.ofSeconds(5)))
    check(ex.run("rm","-rf","$dst.marathon-tmp").successful)
    emit("writefile",true)
    File(root,"expected.xctestrun").writeText(plist)
    val large=File(root,"large.bin").apply{writeBytes(ByteArray(32769){(it%251).toByte()})}
    val small=File(root,"small.bin").apply{writeBytes(byteArrayOf(0,1,-1,10))}
    check(device(ex,bridge(true),false).pushGeneratedFile(large,"/tmp/marathon/large.bin"))
    check(device(ex,bridge(true,true,999),true).pushGeneratedFile(small,"/tmp/marathon/debug.bin"))
    emit("actual_push_generated_fallbacks",true)
    for (key in listOf(true,false)) {
        val b=bridge(key)
        check(b.send(File(root,"source"),"/tmp/marathon/tree/"))
        val back=File(root,if(key)"back-key" else "back-password").apply{mkdirs()}
        check(b.receive("/tmp/marathon/tree/",back))
        emit(if(key)"roundtrip_key" else "roundtrip_password",true)
    }
}
private fun observed(cap:Long,body:()->Unit):Pair<String,Long> {
    var error:Throwable?=null;val start=System.nanoTime();val t=Thread{try{body()}catch(e:Throwable){error=e}}.apply{isDaemon=true;start()};t.join(cap)
    return (if(t.isAlive)"still_waiting" else if(error==null)"returned" else "failed:"+com.google.gson.Gson().toJson(failure(error!!))) to ((System.nanoTime()-start)/1000000)
}
fun main(args:Array<String>) {
    try {runBlocking {
        when(args[0]) {
            "standard"->connect().use {ex->
                val result=ex.run("probe");check(result.stdout==listOf("hello"));check(result.stderr==listOf("error"));check(result.exitCode==7)
                val held=ex.execute(listOf("held"),Duration.ofSeconds(10),Duration.ofSeconds(10),emptyMap(),null)
                delay(100);held.terminate();check(held.await().exitCode==143);check(ex.run("probe").exitCode==7)
                check(ex.run("echo","it's").exitCode==2)
                transfers(ex);emit("standard",true)
            }
            "auth_reject"->{check(runCatching{connect("wrong-fixture-password").close()}.isFailure);emit("auth_reject",true)}
            "keyboard"->connect().use {check(it.run("probe").exitCode==7);emit("keyboard",true)}
            "reject"->connect().use {ex->check(runCatching{ex.run("probe")}.isFailure);check(ex.connected);emit("exec_reject",true)}
            "connect_fault"->{val error=runCatching{connect().close()}.exceptionOrNull();check(error!=null);emit("connect_fault",failure(error))}
            "post_auth"->connect().use {ex->delay(300);val error=runCatching{ex.run("probe")}.exceptionOrNull();check(error!=null);emit("post_auth",failure(error))}
            "exec_fault"->connect().use {ex->val error=runCatching{ex.run("probe")}.exceptionOrNull();check(error!=null);emit("exec_fault",failure(error))}
            "healthy_keepalive"->connect(ka=1000).use {ex->val result=ex.execute(listOf("busy"),Duration.ofSeconds(20),Duration.ofSeconds(20),emptyMap(),null).await();check(result.exitCode==0);check(ex.connected);emit("healthy_keepalive",true)}
            "keepalive"->connect(ka=1000).use {ex->val started=System.nanoTime();val error=runCatching{ex.run("busy")}.exceptionOrNull();check(error!=null);emit("keepalive",mapOf("ms" to (System.nanoTime()-started)/1000000,"error" to failure(error)))}
            "t15"->connect().use {ex->val session=ex.execute(listOf("held"),Duration.ofMinutes(5),Duration.ofMinutes(5),emptyMap(),null);delay(100);val first=observed(40000){session.close()};emit("first_close",first);check(first.first.startsWith("failed:"));check(first.second in 28000..38000)}
            "t15h"->connect().use {ex->val session=ex.execute(listOf("held"),Duration.ofSeconds(1),Duration.ofMinutes(5),emptyMap(),null);val first=observed(40000){runBlocking{session.await()}};emit("timeout_close",first);check(first.first.startsWith("failed:"));check(first.second in 29000..39000);val later=observed(60000){session.close()};emit("later_join",later);check(later.first=="still_waiting")}
            "cleanup"->connect().use {ex->check(ex.run("find","/tmp/marathon","-mindepth","1","-maxdepth","1","-exec","rm","-rf","{}","+").successful);emit("cleanup",true)}
            else->error("unknown case")
        }
    };emit("completion","completed")}catch(e:Throwable){emit("completion",mapOf("outcome" to "failed","error" to failure(e)));throw e}
}
