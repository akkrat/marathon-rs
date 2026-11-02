import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessCommandExecutor
import com.malinskiy.marathon.apple.device.ConnectionFactory
import com.malinskiy.marathon.apple.configuration.Transport
import com.malinskiy.marathon.apple.cmd.remote.rsync.*
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.apple.*
import com.github.fracpete.processoutput4j.output.CollectingProcessOutput
import kotlinx.coroutines.*
import java.io.File
import java.net.InetAddress
import java.time.Duration

fun main(args:Array<String>) = runBlocking {
    fun emit(case:String, value:Any?) = println(com.google.gson.Gson().toJson(mapOf("case" to case, "value" to value)))
    if(args[0]=="reachability") {
        for(host in listOf("127.0.0.1", "::1", "192.0.2.1", "2001:db8::1", "255.255.255.255", "224.0.0.1")) {
            emit(host,runCatching { InetAddress.getByName(host).isReachable(150) }.fold({it},{it.javaClass.name+": "+it.message}))
        }
        return@runBlocking
    }
    if(args[0]=="timeout") {
        val output=CollectingProcessOutput().apply {timeOut=1}
        output.monitor(ProcessBuilder("sh","-c","echo started; exec sleep 10"))
        emit("timeout",mapOf("exit" to output.exitCode,"stdout" to output.stdOut,"stderr" to output.stdErr,"timedOut" to output.hasTimedOut()))
        return@runBlocking
    }
    val root=File(args[1]);root.mkdirs()
    if(args[0]=="programs") {
        emit("rsync-program",com.github.fracpete.rsync4j.RSync().source("/tmp").destination("/tmp").version(true).builder().command().first())
        emit("sshpass-program",com.github.fracpete.rsync4j.SshPass().password("fake-password").builder().command().first())
        return@runBlocking
    }
    if(args[0]=="empty-options") {
        val cfg=Configuration.Builder("probe",File(root,"reports")).apply {debug=false}.build()
        val source=File(root,"src").apply {mkdirs()}
        val dest=File(root,"dest").apply {mkdirs()}
        for(remotePath in listOf("/usr/bin/rsync","")) for(password in listOf("fake-password","")) {
            val bridge=RsyncFileBridge(RsyncTarget("127.0.0.1",22222),cfg,SshConfiguration(keepAliveInterval=Duration.ofMillis(1500)),RsyncConfiguration(remotePath=remotePath),SshAuthentication.PasswordAuthentication("fixture",password))
            emit("send",bridge.send(source,"/tmp/marathon/tree/"))
            emit("receive",bridge.receive("/tmp/marathon/tree/",dest))
        }
        return@runBlocking
    }
    if(args[0]=="process-errors") {
        val cfg=Configuration.Builder("probe",File(root,"reports")).apply {debug=false}.build()
        val b=RsyncFileBridge(RsyncTarget("127.0.0.1",22222),cfg,SshConfiguration(),RsyncConfiguration(),SshAuthentication.PublicKeyAuthentication("fixture",File("/fixture/key")))
        val src=File(root,"source").apply {writeText("payload")}
        emit("send",try {b.send(src,"/tmp/marathon/dst")} catch(e:Exception) {e.javaClass.name})
        emit("receive",try {b.receive("/tmp/marathon/src",File(root,"back"))} catch(e:Exception) {e.javaClass.name})
        return@runBlocking
    }
    if(args[0]=="semantics") {
        val cfg=Configuration.Builder("probe",File(root,"reports")).apply {debug=false}.build()
        val factory=ConnectionFactory(cfg,SshConfiguration(shareWorkerConnection=true),RsyncConfiguration(),Duration.ofMillis(100),8) { {KotlinProcessCommandExecutor()} }
        val first=Transport.Ssh("127.0.0.1",22222,SshAuthentication.PublicKeyAuthentication("first",File("/fixture/first")),false)
        val second=first.copy(authentication=SshAuthentication.PublicKeyAuthentication("second",File("/fixture/second")))
        val (a,ba)=factory.createOrThrow(first);val(b,bb)=factory.createOrThrow(second)
        emit("bridge-auth-alias",ba===bb);emit("executor-auth-distinct",a!==b)
        val src=File(root,"source").apply {writeText("first")}
        emit("second-bridge-send",bb.send(src,"/tmp/marathon/target"))
        factory.dispose(a);factory.dispose(b)
        val transfers=SharedTransfers();var count=0
        transfers.send("worker:22",src,"/dst",{true}) {count++;true}
        src.writeText("changed")
        transfers.send("worker:22",src,"/dst",{true}) {count++;true}
        emit("same-path-after-byte-change",count)
        transfers.send("worker:22",src,"/dst",{null}) {count++;true}
        emit("null-existence",count)
        transfers.send("worker:22",src,"/dst",{false}) {count++;true}
        emit("missing-destination",count)
        val ex=KotlinProcessCommandExecutor();val bytes=ByteArray(256){it.toByte()}+"<key>ENV</key><string>\$HOME '\"`;|&><*?</string>\n".toByteArray()
        val dst=File(root,"bytes")
        emit("write-bytes",ex.writeFile(bytes,dst.path,Duration.ofSeconds(5)) && dst.readBytes().contentEquals(bytes))
        emit("write-mode",java.nio.file.attribute.PosixFilePermissions.toString(java.nio.file.Files.getPosixFilePermissions(dst.toPath())))
        ex.close()
        return@runBlocking
    }
    if(args[0]=="gates" || args[0]=="deadlines") {
        val cfg=Configuration.Builder("probe",File(root,"reports")).apply {debug=false}.build()
        val bridge=RsyncFileBridge(RsyncTarget("127.0.0.1",22222),cfg,SshConfiguration(),RsyncConfiguration(),SshAuthentication.PublicKeyAuthentication("fixture",File("/fixture/key")))
        val src=File(root,"source").apply {writeText("payload")}
        suspend fun entered(label:String) {withTimeout(10000) {while(!File(root,"$label.entered").exists()) delay(5)}}
        val send=async(Dispatchers.IO) {bridge.send(src,"/tmp/marathon/send1")}; entered("send1")
        val receive=async(Dispatchers.IO) {bridge.receive("/tmp/marathon/receive1",File(root,"back1"))}; entered("receive1")
        if(args[0]=="gates") {
            val send2=async(Dispatchers.IO) {bridge.send(src,"/tmp/marathon/send2")}
            val receive2=async(Dispatchers.IO) {bridge.receive("/tmp/marathon/receive2",File(root,"back2"))}; entered("receive2")
            emit("send2-entered-before-release",File(root,"send2.entered").exists())
            File(root,"release").writeText("")
            emit("gates",listOf(send.await(),send2.await(),receive.await(),receive2.await()))
        } else {
            emit("receive-after-timeout",withTimeout(40000){receive.await()})
            emit("send-still-running-after-receive-timeout",!send.isCompleted)
            File(root,"release").writeText("")
            emit("send-after-release",send.await())
        }
        return@runBlocking
    }
    val source=File(root,"src").apply{mkdirs()}
    val dest=File(root,"dest").apply{mkdirs()}
    for(key in listOf(false,true)) for(debug in listOf(false,true)) for(sshDebug in listOf(false,true)) {
        val cfg=Configuration.Builder("probe",File(root,"reports")).apply{this.debug=debug}.build()
        val auth=if(key) SshAuthentication.PublicKeyAuthentication("fixture",File("/fixture/key")) else SshAuthentication.PasswordAuthentication("fixture","fake-password")
        val bridge=RsyncFileBridge(RsyncTarget("127.0.0.1",22222),cfg,SshConfiguration(debug=sshDebug,keepAliveInterval=Duration.ofMillis(1500)),RsyncConfiguration(),auth)
        emit("send-$key-$debug-$sshDebug", bridge.send(source,"/tmp/marathon/tree/"))
        emit("receive-$key-$debug-$sshDebug", bridge.receive("/tmp/marathon/tree/",dest))
    }
}
