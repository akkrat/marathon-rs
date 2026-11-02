// WP 6.2: pinned adam 0.5.8, reference JDK. Loopback byte stub only; no adb process or devices.
import com.malinskiy.adam.*
import com.malinskiy.adam.request.Feature
import com.malinskiy.adam.request.pkg.*
import com.malinskiy.adam.request.pkg.multi.*
import com.malinskiy.adam.request.framebuffer.*
import com.malinskiy.adam.request.reverse.*
import com.malinskiy.adam.request.forwarding.*
import com.malinskiy.adam.request.sync.v1.StatFileRequest as Stat1
import com.malinskiy.adam.request.sync.v2.StatFileRequest as Stat2
import com.malinskiy.adam.request.sync.v2.ListFileRequest as List2
import com.malinskiy.adam.request.sync.v1.PushFileRequest as Push1
import com.malinskiy.adam.request.sync.v1.PullFileRequest as Pull1
import com.malinskiy.adam.request.sync.v2.PullFileRequest as Pull2
import kotlinx.coroutines.*
import java.io.*
import java.net.*
import java.nio.*
import java.security.MessageDigest
import kotlin.concurrent.thread

fun esc(s:String)=buildString { for(c in s) when(c) { '"'->append("\\\"");'\\'->append("\\\\");else->if(c<' '||c.code>126)append("\\u%04x".format(c.code)) else append(c) } }
fun row(k:String,v:Any?)=println("{\"case\":\"${esc(k)}\",\"value\":\"${esc(v.toString())}\"}")
fun hex(b:ByteArray)=b.joinToString(""){"%02x".format(it.toInt() and 255)}
fun digest(b:ByteArray)=hex(MessageDigest.getInstance("SHA-256").digest(b))
fun error(e:Throwable)="${e.javaClass.name}: ${e.message}; cause=${e.cause}"
fun le(vararg n:Int)=ByteBuffer.allocate(n.size*4).order(ByteOrder.LITTLE_ENDIAN).apply {n.forEach{putInt(it)}}.array()
fun DataInputStream.bytes(n:Int)=ByteArray(n).also{readFully(it)}
fun DataInputStream.command():String=String(bytes(String(bytes(4)).toInt(16)))
fun DataInputStream.sync():ByteArray {val tag=bytes(4);return tag+bytes(Integer.reverseBytes(readInt()))}
class Stub(count:Int=1,val script:(Int,DataInputStream,OutputStream)->Unit):AutoCloseable {
 val server=ServerSocket(0,1,InetAddress.getLoopbackAddress())
 val worker=thread { repeat(count){i->server.accept().use{s->s.soTimeout=10000;script(i,DataInputStream(s.getInputStream()),s.getOutputStream())}};server.close() }
 val client=AndroidDebugBridgeClientFactory().apply{port=server.localPort;host=InetAddress.getLoopbackAddress()}.build()
 override fun close(){client.close();worker.join();server.close()}
}
fun OutputStream.send(b:ByteArray){write(b);flush()}
fun OutputStream.send(s:String)=send(s.toByteArray())
fun main(args:Array<String>)=runBlocking {
 val root=File(args[0]);
 val jdkProfile=java.awt.color.ICC_Profile.getInstance(java.awt.color.ColorSpace.CS_sRGB).data
 File(root,"fixtures/oracle/adb-request-jdk-srgb.icc").writeBytes(jdkProfile)
 row("jdk-srgb-profile-sha256",digest(jdkProfile));val fixture=File(root,"fixtures/adam/fixture")
 val fakePort=System.getenv("MRS_REQUEST_FAKE_PORT")
 if(fakePort!=null){fake(root,fakePort.toInt());return@runBlocking}
 val tmp=kotlin.io.path.createTempDirectory("adam-request-probes").toFile()
 try {
  val apk=File(tmp,"sample-fake.apk").apply{writeBytes(File(fixture,"sample-fake.apk").readBytes())}
  for(features in listOf(emptyList(),listOf(Feature.CMD),listOf(Feature.CMD,Feature.ABB_EXEC))) {
   val label=features.joinToString(","){it.name}
   val pkg=SingleFileInstallationPackage(apk)
   row("create:$label",hex(CreateIndividualPackageSessionRequest(pkg,listOf(pkg),features,true,listOf("-g")).serialize()))
   row("write:$label",hex(WriteIndividualPackageRequest(apk,features,"id").serialize()))
   row("commit:$label",hex(InstallCommitRequest("id",features).serialize()))
   row("stream-validate:$label",StreamingPackageInstallRequest(apk,features,true).validate())
   if(features.isNotEmpty())row("stream:$label",hex(StreamingPackageInstallRequest(apk,features,true,listOf("--user","a'b")).serialize()))
  }
  for(response in listOf("Success [word-id]","Failure","Success no brackets","Success []")) {
   Stub {_,i,o->i.command();o.send("OKAY$response")}.use {s->
    try{row("create-response:$response",s.client.execute(CreateIndividualPackageSessionRequest(SingleFileInstallationPackage(apk),emptyList(),listOf(Feature.CMD),false)))}catch(e:Throwable){row("create-response:$response",error(e))}
   }
  }
  for(response in listOf("Success","Failure","")) {
   Stub {_,i,o->i.command();o.send("OKAY$response")}.use {s->try{row("commit-response:$response",s.client.execute(InstallCommitRequest("id",listOf(Feature.CMD))))}catch(e:Throwable){row("commit-response:$response",error(e))}}
   Stub {_,i,o->i.command();o.send("OKAY");val b=i.bytes(614);row("write-payload:$response",digest(b));o.send(response)}.use {s->try{row("write-response:$response",s.client.execute(WriteIndividualPackageRequest(apk,listOf(Feature.CMD),"id")))}catch(e:Throwable){row("write-response:$response",error(e).replace(apk.path,"<apk>"))}}
  }
  val fields=le(0,0,1,0,0,123,2,0,1000,0,1,1589042331,0,1589042332,0,1589042333,0)
  Stub {_,i,o->i.command();o.send("OKAY");row("stat2-wire",hex(i.sync()));o.send("LST2".toByteArray()+fields)}.use{s->row("stat2-n18",s.client.execute(Stat2("/x",listOf(Feature.STAT_V2))))}
  Stub {_,i,o->i.command();o.send("OKAY");row("list2-wire",hex(i.sync()));o.send("DNT2".toByteArray()+fields+le(1)+"aWHAT".toByteArray())}.use{s->row("list2-n18-unexpected-tag",s.client.execute(List2("/x",listOf(Feature.LS_V2))))}
  apk.setLastModified(0x01020304L*1000)
  Stub {_,i,o->i.command();o.send("OKAY");row("push-send",hex(i.sync()));val tag=i.bytes(4);val n=Integer.reverseBytes(i.readInt());row("push-data",hex(tag+le(n)+i.bytes(n)));row("push-done",hex(i.bytes(8)));o.send("OKAY")}.use{s->val p=mutableListOf<Double>();for(v in s.client.execute(Push1(apk,"/x"),this@runBlocking))p.add(v);row("push-progress",p)}
  for(v2 in listOf(false,true)) for(size in listOf(0,3)) {
   val local=File(tmp,"pull-$v2-$size")
   Stub {_,i,o->i.command();o.send("OKAY");row("pull-stat:$v2:$size",hex(i.sync()));o.send("STAT".toByteArray()+le(420,size,0));row("pull-recv:$v2:$size",hex(i.sync()));if(v2)row("pull-flags:$size",hex(i.bytes(8)));if(size>0)o.send("DATA".toByteArray()+le(3)+"abc".toByteArray());o.send("DONE".toByteArray()+le(0));i.read()}.use{s->val p=mutableListOf<Double>();val req=if(v2)Pull2("/x",local,listOf(Feature.SENDRECV_V2),coroutineContext=coroutineContext) else Pull1("/x",local,coroutineContext=coroutineContext);for(v in s.client.execute(req,this@runBlocking))p.add(v);row("pull-progress:$v2:$size",p)}
   row("pull-file:$v2:$size",if(local.exists())hex(local.readBytes()) else "absent")
  }
  for(reply in listOf("OKAY00041234","OKAY","OKAY0003bad","FAIL0003bad")) {
   Stub {_,i,o->row("reverse-wire:$reply",hex(i.command().toByteArray()));o.send("OKAY$reply")}.use{s->try{row("reverse-response:$reply",s.client.execute(ReversePortForwardRequest(RemoteTcpPortSpec(0),LocalTcpPortSpec(42))))}catch(e:Throwable){row("reverse-response:$reply",error(e))}}
  }
  try {Push1(File(tmp,"absent"),"/x").validate()}catch(e:Throwable){row("push-missing-construction",error(e).replace(tmp.path,"<tmp>"))}
  for(path in listOf("é".repeat(1024),"😀".repeat(513)))row("stat-path-utf16:${path.length}",Stat1(path).validate())
  val independent=object:CoroutineScope {override val coroutineContext=Dispatchers.IO+CoroutineExceptionHandler{_,_->}}
  Stub {_,i,o->i.command();o.send("OKAY");i.sync();o.send("DATA".toByteArray()+le(-1))}.use{s->
   try {for(v in s.client.execute(Pull1("/x",File(tmp,"negative"),size=3,coroutineContext=coroutineContext),independent)) {}}catch(e:Throwable){row("approved-p2-negative-data",error(e))}
  }
  Stub {_,i,o->i.command();o.send("OKAY");o.send(le(99))}.use{s->try{s.client.execute(ScreenCaptureRequest(RawImageScreenCaptureAdapter()))}catch(e:Throwable){row("frame-unsupported",error(e))}}
  for(fail in listOf("create","write","commit","abandon")) {
   val count=when(fail){"create"->1;"commit"->4;else->3}
   Stub(count) {n,i,o->
    val cmd=i.command();row("split-command:$fail:$n",hex(cmd.toByteArray()));o.send("OKAY")
    if(cmd.contains("install-write")){row("split-payload:$fail",digest(i.bytes(614)))}
    val response=when {
     cmd.contains("install-create")->if(fail=="create")"Failure create" else "Success [S1]"
     cmd.contains("install-write")->if(fail=="write"||fail=="abandon")"Failure write" else "Success"
     cmd.contains("install-commit")->"Failure commit"
     fail=="abandon"->"Failure abandon"
     else->""
    };o.send(response)
   }.use{s->try{row("split-result:$fail",s.client.execute(InstallSplitPackageRequest(ApkSplitInstallationPackage(listOf(apk)),listOf(Feature.CMD),true)))}catch(e:Throwable){row("split-result:$fail",error(e).replace(apk.path,"<apk>"))}}
  }
  val folder=File(tmp,"folder").apply{mkdirs()};val nested=File(folder,"nested").apply{mkdirs()}
  File(folder,"root.apk").apply{writeText("abc");setLastModified(0x01020304L*1000)}
  File(nested,"inner.apk").apply{writeText("def");setLastModified(0x01020304L*1000)}
  for(empty in listOf(false,true)) {
   val source=if(empty)File(tmp,"empty").apply{mkdirs()}else folder
   Stub(if(empty)3 else 5){n,i,o->
    val cmd=i.command();row("directory-command:$empty:$n",hex(cmd.toByteArray()));o.send("OKAY")
    if(n==0){row("directory-stat:$empty",hex(i.sync()));o.send("STAT".toByteArray()+le(0,0,0))}
    else if(cmd=="sync:"){row("directory-send:$n",hex(i.sync()));val tag=i.bytes(4);val len=Integer.reverseBytes(i.readInt());row("directory-data:$n",hex(tag+le(len)+i.bytes(len)));row("directory-done:$n",hex(i.bytes(8)));o.send("OKAY")}
    else o.send("x0")
   }.use{s->row("directory-result:$empty",s.client.execute(com.malinskiy.adam.request.sync.PushRequest(source,"/dest",emptyList(),coroutineContext=coroutineContext)))}
  }
  for(name in listOf("screencap_1","screencap_2","screencap_1_unaligned","screencap_3")) {
   val b=File(fixture,"$name.bin").readBytes();val version=if(name=="screencap_3")2 else 1
   Stub {_,i,o->i.command();o.send("OKAY");val header=if(version==2)52 else 48;o.send(le(version)+b.copyOfRange(0,header));row("frame-ack:$name",i.read());o.send(b.copyOfRange(header,b.size))}.use{s->
    val image=s.client.execute(ScreenCaptureRequest(BufferedImageScreenCaptureAdapter()));val rgb=image.getRGB(0,0,image.width,image.height,null,0,image.width);val bytes=ByteBuffer.allocate(rgb.size*4).apply{rgb.forEach{putInt(it)}}.array();row("frame-argb-sha256:$name",digest(bytes));row("frame-first:$name",rgb.take(8));row("frame-size:$name","${image.width}x${image.height}")
   }
  }
  for(rawMode in listOf(false,true))for(color in listOf(0,1,2))for(version in listOf(2,16)) {
   val header=if(version==16)le(2,1,1)else le(32,color,4,1,1,0,8,16,8,8,8,24,8)
   val pixel=if(version==16)byteArrayOf(0,0xf8.toByte())else byteArrayOf(90,130.toByte(),170.toByte(),128.toByte())
   Stub {_,i,o->i.command();o.send("OKAY");o.send(le(version)+header);i.read();o.send(pixel)}.use{s->try{val image=if(rawMode)s.client.execute(ScreenCaptureRequest(RawImageScreenCaptureAdapter())).toBufferedImage() else s.client.execute(ScreenCaptureRequest(BufferedImageScreenCaptureAdapter()));row("${if(rawMode)"raw-pixel" else "pixel"}:$version:$color",image.getRGB(0,0))}catch(e:Throwable){row("pixel:$version:$color",error(e))}}
  }
 } finally {tmp.deleteRecursively()}
}

suspend fun fake(root:File,port:Int)=coroutineScope {
 val client=AndroidDebugBridgeClientFactory().apply{this.port=port;host=InetAddress.getLoopbackAddress()}.build()
 val tmp=kotlin.io.path.createTempDirectory("adam-fake-requests").toFile()
 try {
  val apk=File(tmp,"sample-fake.apk").apply{writeBytes(File(root,"fixtures/adam/fixture/sample-fake.apk").readBytes());setLastModified(0x01020304L*1000)}
  for(features in listOf(emptyList(),listOf(Feature.SENDRECV_V2,Feature.STAT_V2,Feature.LS_V2))) {
   val dest=if(features.isEmpty())"/sdcard/v1.apk" else "/sdcard/v2.apk"
   val push=if(features.isEmpty())Push1(apk,dest,coroutineContext=coroutineContext) else com.malinskiy.adam.request.sync.v2.PushFileRequest(apk,dest,features,coroutineContext=coroutineContext)
   for(v in client.execute(push,this@coroutineScope,"emulator-5554")){}
   client.execute(com.malinskiy.adam.request.sync.compat.CompatStatFileRequest(dest,features),"emulator-5554")
   client.execute(com.malinskiy.adam.request.sync.compat.CompatListFileRequest("/sdcard",features),"emulator-5554")
   val local=File(tmp,"pull.apk")
   val pull=if(features.isEmpty())Pull1(dest,local,coroutineContext=coroutineContext) else Pull2(dest,local,features,coroutineContext=coroutineContext)
   for(v in client.execute(pull,this@coroutineScope,"emulator-5554")){}
   row("fake-pull:$dest",digest(local.readBytes()))
  }
  row("fake-stream",client.execute(StreamingPackageInstallRequest(apk,listOf(Feature.ABB_EXEC),true),"emulator-5554"))
  row("fake-split",client.execute(InstallSplitPackageRequest(ApkSplitInstallationPackage(listOf(apk,apk)),listOf(Feature.CMD),true),"emulator-5554"))
  row("fake-reverse",client.execute(ReversePortForwardRequest(RemoteTcpPortSpec(1234),LocalTcpPortSpec(5678)),"emulator-5554"))
  client.execute(RemoveReversePortForwardRequest(RemoteTcpPortSpec(1234)),"emulator-5554")
  val image=client.execute(ScreenCaptureRequest(BufferedImageScreenCaptureAdapter()),"emulator-5554")
  row("fake-frame-pixel",image.getRGB(0,0))
 } finally {client.close();tmp.deleteRecursively()}
}
