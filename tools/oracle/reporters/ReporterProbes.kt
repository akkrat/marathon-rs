import com.google.gson.Gson
import com.malinskiy.marathon.analytics.internal.sub.*
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.device.screenshot.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.io.*
import com.malinskiy.marathon.report.html.HtmlSummaryReporter
import com.malinskiy.marathon.report.timeline.*
import com.malinskiy.marathon.report.allure.AllureReporter
import com.malinskiy.marathon.test.*
import io.qameta.allure.AllureLifecycle
import io.qameta.allure.FileSystemResultsWriter
import io.qameta.allure.util.ResultsUtils
import kotlinx.coroutines.*
import org.imgscalr.Scalr
import java.io.File
import java.time.Instant
import java.time.Duration
import java.awt.image.BufferedImage
import javax.imageio.ImageIO
import javax.imageio.stream.FileImageOutputStream

val gson = Gson()
fun device(serial: String = "serial #1", features: List<DeviceFeature> = emptyList()) = DeviceInfo(OperatingSystem("23"),serial,"model<&>","unknown",NetworkState.CONNECTED,features,true)
fun config(root: File) = Configuration.Builder(name="Report <&> 名", outputDir=root, vendorConfiguration=VendorConfiguration.StubVendorConfiguration).build()
fun result(method: String, status: TestStatus, start:Long, end:Long, pkg:String="pkg", trace:String?=null, attachments:List<Attachment> = emptyList(), meta:List<MetaProperty> = emptyList(), features:List<DeviceFeature> = emptyList()) = TestResult(Test(pkg,"Clazz",method,meta),device(features=features),"batch",status,start,end,trace,attachments)
fun event(r:TestResult, final:Boolean=true, pool:String="pool") = TestEvent(Instant.EPOCH,DevicePoolId(pool),r.device,r,final)
fun report(tests:List<TestEvent>, pools:List<String> = listOf("pool")) = ExecutionReport(pools.map{DeviceConnectedEvent(Instant.EPOCH,DevicePoolId(it),device())},listOf(DeviceDisconnectedEvent(Instant.EPOCH,DevicePoolId("gone"),device("disconnect-only"))),listOf(DevicePreparingEvent(Instant.ofEpochMilli(10),Instant.ofEpochMilli(30),"prepare-only"),DevicePreparingEvent(Instant.ofEpochMilli(1),Instant.ofEpochMilli(20),"serial #1")),listOf(DeviceProviderPreparingEvent(Instant.EPOCH,Instant.ofEpochMilli(10),"provider-only")),tests)
fun error(block:()->Unit):String? = try { block();null } catch(e:Exception) { e.javaClass.name + ": " + e.message }
fun image(w:Int,h:Int,gradient:Boolean=false):BufferedImage {
    val im=BufferedImage(w,h,BufferedImage.TYPE_INT_ARGB)
    for(y in 0 until h) for(x in 0 until w) im.setRGB(x,y,if(gradient) ((x*255/(w.coerceAtLeast(2)-1)) shl 16) or ((y*255/(h.coerceAtLeast(2)-1)) shl 8) or 0xff000000.toInt() else listOf(0xffff0000.toInt(),0xff00ff00.toInt(),0xff0000ff.toInt(),0xffffffff.toInt())[(x+y*w)%4])
    return im
}
fun main(args:Array<String>) = runBlocking {
    val out=File(args[0]); val tmp=File(args[1]); val root=File(tmp,"output");root.mkdirs()
    val rows=mutableListOf<Map<String,Any?>>()
    fun row(id:String,vararg pairs:Pair<String,Any?>){rows.add(linkedMapOf("id" to id,*pairs))}
    val fm=FileManager(4096,255,root);val cfg=config(root);val html=HtmlSummaryReporter(gson,fm,root,cfg)
    val empty=ExecutionReport(emptyList(),emptyList(),emptyList(),emptyList(),emptyList())
    row("empty-html", "error" to error{html.generate(empty)},"files" to root.walkTopDown().map{it.relativeTo(root).path}.toList())
    val allureEmpty=File(root,"allure-empty")
    println("probe: empty-allure")
    row("empty-allure", "error" to error{AllureReporter(cfg,allureEmpty).generate(empty)},"exists" to allureEmpty.exists())
    val trace="06 V verbose\r\nD debug\nI info\nW warning\nE <&>\nA assert\nα default\n\u0000\u0001\u000b\u000c\u000e\u007f\u0085\u009f\ufffe\uffff\ud800\n"
    row("log", "input" to trace, "html" to html.generateLogcatHtml(trace))
    row("path", "html" to File(root,"/html").path,"timeline" to File(File(root,"/html"),"/timeline").path)
    val tests=TestStatus.values().mapIndexed{i,s->event(result("test$i",s,i.toLong(),(20-i).toLong(),trace=if(i==0)trace else if(i==1)"" else null))} + listOf(event(result("test1",TestStatus.FAILURE,1,50),false),event(result("negative",TestStatus.PASSED,40,3),pool="pool two"),event(result("test1",TestStatus.PASSED,20,30,pkg="other"),pool="pool two"))
    val mixed=report(tests,listOf("pool","pool two"))
    File(out,"timeline.json").writeText(gson.toJson(TimelineSummaryProvider().generate(mixed)))
    File(out,"timeline-empty.json").writeText(gson.toJson(TimelineSummaryProvider().generate(empty)))
    File(out,"index.json").writeText(gson.toJson(with(html){mixed.summary.toHtmlIndex()}))
    File(out,"pool.json").writeText(gson.toJson(with(html){mixed.summary.pools.first().toHtmlPoolSummary()}))
    File(out,"full.json").writeText(gson.toJson(with(html){tests.first().testResult.toHtmlFullTest("pool","batch")}))
    TimelineReporter(TimelineSummaryProvider(),gson,root).generate(mixed);html.generate(mixed)
    File(root,"html").copyRecursively(File(out,"html-mixed"),overwrite=true)
    val artifact=File(root,"artifact # name.bin");artifact.writeText("content")
    val missing=File(root,"missing");val zero=File(root,"zero");zero.writeText("")
    fun prop(name:String,value:Any?)=MetaProperty(name,mapOf("value" to value))
    val meta=listOf(prop("io.qameta.allure.Description","java"),prop("io.qameta.allure.kotlin.Description","kotlin"),prop("io.qameta.allure.Issue","issue"),prop("io.qameta.allure.TmsLink","tms"),prop("io.qameta.allure.kotlin.Issue","ki"),prop("io.qameta.allure.kotlin.TmsLink","kt"),prop("io.qameta.allure.Epic",3),prop("io.qameta.allure.Epic","ignored second"),prop("io.qameta.allure.Feature","feature"),prop("io.qameta.allure.Severity","critical"),prop("io.qameta.allure.kotlin.Owner","owner"))
    val attachments=AttachmentType.values().map{Attachment(artifact,it)}+listOf(Attachment(artifact,AttachmentType.LOG,"named"),Attachment(missing,AttachmentType.LOG),Attachment(zero,AttachmentType.LOG))
    val ar=AllureReporter(cfg,File(root,"allure-results"))
    val create=ar.javaClass.getDeclaredMethod("createTestResult",String::class.java,DeviceInfo::class.java,TestResult::class.java);create.isAccessible=true
    for((i,e) in (tests+event(result("annotations",TestStatus.PASSED,1,2,trace="",meta=meta,attachments=attachments))).withIndex()){
        val uuid="00000000-0000-0000-0000-"+i.toString().padStart(12,'0')
        val r=create.invoke(ar,uuid,e.device,e.testResult) as io.qameta.allure.model.TestResult
        val life=AllureLifecycle(FileSystemResultsWriter(File(out,"allure").toPath()));life.scheduleTestCase(uuid,r);life.writeTestCase(uuid)
    }
    row("history", "input" to "Clazz.test1", "hash" to ResultsUtils.generateMethodSignatureHash("Clazz","test1",emptyList()))
    val env=File(out,"environment");env.mkdirs()
    println("probe: environment")
    com.github.automatedowl.tools.AllureEnvironmentWriter.allureEnvironmentWriter(com.google.common.collect.ImmutableMap.of("first","<&> 名\n\"'","second","two"),env.absolutePath+"/")
    val noenv=File(tmp,"no-env")
    println("probe: environment-missing")
    row("environment-missing", "error" to error{com.github.automatedowl.tools.AllureEnvironmentWriter.allureEnvironmentWriter(com.google.common.collect.ImmutableMap.of("a","b"),noenv.absolutePath+"/")},"exists" to noenv.exists())
    for(delay in listOf(0,19,-10,655360)) {
        val file=File(out,"gif-$delay.gif")
        row("gif-$delay", "error" to error{FileImageOutputStream(file).use{val writer=GifSequenceWriter(it,BufferedImage.TYPE_INT_ARGB,delay,false);writer.writeToSequence(image(2,3));writer.writeToSequence(image(2,3));writer.close()}})
    }
    val zeroGif=File(out,"gif-zero.gif")
    row("gif-zero", "error" to error{FileImageOutputStream(zeroGif).use{GifSequenceWriter(it,BufferedImage.TYPE_INT_ARGB,10,true).close()}},"size" to zeroGif.length())
    for(rotation in Rotation.values()) for((w,h) in listOf(2 to 3,3 to 2,2 to 2)) {
        var n=0
        val source=object:Screenshottable {override val orientation=rotation;override suspend fun getScreenshot(timeout:Duration):BufferedImage? {if(n++==2)throw CancellationException("done");return if(n==1)image(w,h) else image(h,w)}}
        val cap=ScreenCapturer(device(),source,DevicePoolId("pool"),"batch",fm,Duration.ZERO,3,2,Duration.ofMillis(7))
        val t=Test("pkg","Clazz","capture-${rotation.value}-$w-$h",emptyList())
        var caught:String?=null;try{cap.start(t)}catch(e:Exception){caught=e.javaClass.name}
        val file=fm.createFile(FileType.SCREENSHOT,DevicePoolId("pool"),device(),t,"batch")
        file.copyTo(File(out,t.method+".gif"),true)
        row(t.method,"error" to caught,"captures" to cap.capturesTaken)
    }
    for((w,h) in listOf(7 to 11,11 to 7,2 to 3,3 to 2)) {
        val im=image(w,h,true);val resized=if(w<h)Scalr.resize(im,Scalr.Method.SPEED,Scalr.Mode.AUTOMATIC,5,8) else Scalr.resize(im,Scalr.Method.SPEED,Scalr.Mode.AUTOMATIC,8,5)
        ImageIO.write(resized,"png",File(out,"resize-$w-$h.png"));row("resize-$w-$h","width" to resized.width,"height" to resized.height)
    }
    val connected=ExecutionReport(listOf(DeviceConnectedEvent(Instant.EPOCH,DevicePoolId("only"),device())),emptyList(),emptyList(),emptyList(),emptyList())
    File(out,"index-connected-empty.json").writeText(gson.toJson(with(html){connected.summary.toHtmlIndex()}))
    row("empty-average", "error" to error{html.averageDuration(emptyList())})
    row("html-raw-pool", "error" to error{html.generate(report(listOf(event(result("raw",TestStatus.PASSED,0,1),pool="raw #& pool")),listOf("raw #& pool")))},"exists" to File(root,"html/pools/raw #& pool.html").exists())
    val twice=listOf(event(result("overwrite",TestStatus.PASSED,1,2)),event(result("overwrite",TestStatus.FAILURE,3,4)))
    html.generate(report(twice));File(root,"html/pools/pool/serial-#1/pkg.Clazz.overwrite.html").copyTo(File(out,"overwrite.html"),true)
    val fullModels=mutableListOf<Any>()
    for(features in listOf(emptyList(),listOf(DeviceFeature.SCREENSHOT),listOf(DeviceFeature.VIDEO),listOf(DeviceFeature.SCREENSHOT,DeviceFeature.VIDEO))) for(present in listOf(false,true)) {
        val index=fullModels.size;val r=result("artifacts$index",TestStatus.PASSED,0,7,attachments=listOf(Attachment(artifact,AttachmentType.VIDEO),Attachment(missing,AttachmentType.VIDEO),Attachment(artifact,AttachmentType.VIDEO)),features=features)
        if(present){fm.createFile(FileType.SCREENSHOT,DevicePoolId("pool"),r.device,r.test,"batch").writeText("screenshot");fm.createFile(FileType.LOG,DevicePoolId("pool"),r.device,r.test,"batch").writeText("LOG FILE IS NOT STACKTRACE")}
        fullModels.add(with(html){r.toHtmlFullTest("pool","batch")})
    }
    File(out,"html-artifacts.json").writeText(gson.toJson(fullModels))
    fullModels.forEachIndexed{i,m->File(out,"html-artifact-$i.json").writeText(gson.toJson(m))}
    val android=cfg.copy(vendorConfiguration=VendorConfiguration.AndroidConfiguration(androidSdk=null,applicationOutput=null,testApplicationOutput=null,splitApks=null,extraApplicationsOutput=null,outputs=null,profilingConfiguration=com.malinskiy.marathon.config.vendor.android.ProfilingConfiguration(urlTemplate="https://trace.invalid/{}?copy={}")))
    val androidReporter=AllureReporter(android,File(root,"android-allure"));val androidResult=create.invoke(androidReporter,"android",device(),result("profile",TestStatus.PASSED,1,2,attachments=listOf(Attachment(artifact,AttachmentType.PROFILING)))) as io.qameta.allure.model.TestResult
    AllureLifecycle(FileSystemResultsWriter(File(out,"allure").toPath())).apply{scheduleTestCase("android",androidResult);writeTestCase("android")}
    val failureDir=File(tmp,"blocked");failureDir.writeText("file")
    println("probe: allure-write-failure")
    row("allure-write-failure","error" to error{AllureReporter(cfg,failureDir).generate(report(listOf(event(result("x",TestStatus.PASSED,1,2)))))})
    val cases=listOf("timeout","io","null","frame","fatal")
    var atCase=0;val calls=mutableListOf<Long>()
    val script=object:Screenshottable{override val orientation=Rotation.ROTATION_0;override suspend fun getScreenshot(timeout:Duration):BufferedImage?{calls.add(timeout.toMillis());return when(cases[atCase++]){"timeout"->throw java.util.concurrent.TimeoutException("source");"io"->throw java.io.IOException("source");"null"->null;"frame"->image(2,3);else->throw IllegalStateException("fatal")}}}
    val capturer=ScreenCapturer(device(),script,DevicePoolId("pool"),"batch",fm,Duration.ZERO,3,2,Duration.ofMillis(7))
    val capLogger=org.slf4j.LoggerFactory.getLogger("com.malinskiy.marathon.device.screenshot.ScreenCapturer") as ch.qos.logback.classic.Logger
    val appender=ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>();appender.start();capLogger.addAppender(appender)
    var caught:String?=null;try{capturer.start(Test("pkg","Clazz","errors",emptyList()))}catch(e:Exception){caught=e.javaClass.name}
    row("capture-errors","error" to caught,"captures" to capturer.capturesTaken,"timeouts" to calls,"logs" to appender.list.map{listOf(it.loggerName,it.level.toString(),it.formattedMessage,it.throwableProxy?.className)})
    capLogger.detachAppender(appender)
    var nextImage=image(2,3);var reuseCalls=0
    val reuseSource=object:Screenshottable{override val orientation=Rotation.ROTATION_0;override suspend fun getScreenshot(timeout:Duration):BufferedImage?{if(reuseCalls++%2==1)throw CancellationException("done");return nextImage}}
    val reuse=ScreenCapturer(device(),reuseSource,DevicePoolId("pool"),"batch",fm,Duration.ZERO,3,2,Duration.ofMillis(7))
    for(i in 0..1){val t=Test("pkg","Clazz","reuse$i",emptyList());try{reuse.start(t)}catch(e:CancellationException){};fm.createFile(FileType.SCREENSHOT,DevicePoolId("pool"),device(),t,"batch").copyTo(File(out,"reuse$i.gif"),true);nextImage=image(3,2);row("reuse$i","captures" to reuse.capturesTaken)}
    for(name in listOf("gradient","alpha")) {
        val im=if(name=="gradient")image(32,32,true) else BufferedImage(4,1,BufferedImage.TYPE_INT_ARGB).apply{setRGB(0,0,0x00123456);setRGB(1,0,0x80123456.toInt());setRGB(2,0,0xff123456.toInt());setRGB(3,0,0x40abcdef)}
        FileImageOutputStream(File(out,"$name.gif")).use{val w=GifSequenceWriter(it,BufferedImage.TYPE_INT_ARGB,19,true);w.writeToSequence(im);w.close()}
        ImageIO.write(im,"png",File(out,"$name-input.png"))
    }
    // Record decoded ImageIO pixels and native metadata as independent evidence.
    for(file in out.listFiles()!!.filter{it.extension=="gif"&&it.length()>1}){
        val reader=ImageIO.getImageReadersByFormatName("gif").next();ImageIO.createImageInputStream(file).use{stream->reader.input=stream
            for(i in 0 until reader.getNumImages(true)){ImageIO.write(reader.read(i),"png",File(out,file.nameWithoutExtension+"-frame$i.png"))}
        };reader.dispose()
    }
    fun brokenStream()=object:javax.imageio.stream.ImageOutputStreamImpl(){override fun read():Int=-1;override fun read(b:ByteArray,off:Int,len:Int):Int=-1;override fun write(b:Int){throw java.io.IOException("write")};override fun write(b:ByteArray,off:Int,len:Int){throw java.io.IOException("write")}}
    row("gif-write-failure","error" to error{GifSequenceWriter(brokenStream(),BufferedImage.TYPE_INT_ARGB,0,true).writeToSequence(image(2,3))})
    row("gif-close-failure","error" to error{GifSequenceWriter(brokenStream(),BufferedImage.TYPE_INT_ARGB,0,true).close()})
    for(where in listOf("capture","delay")){
        val scheduler=kotlinx.coroutines.test.TestCoroutineScheduler();val dispatcher=kotlinx.coroutines.test.StandardTestDispatcher(scheduler);val scope=CoroutineScope(dispatcher)
        var reads=0;var cleaned=false
        val source=object:Screenshottable{override val orientation=Rotation.ROTATION_0;override suspend fun getScreenshot(timeout:Duration):BufferedImage?{reads++;if(where=="capture")try{awaitCancellation()}finally{cleaned=true};return image(2,3)}}
        val cap=ScreenCapturer(device(),source,DevicePoolId("pool"),"batch",fm,Duration.ofMillis(1000),3,2,Duration.ofMillis(7));val t=Test("pkg","Clazz","cancel-$where",emptyList())
        val job=scope.launch{cap.start(t)};scheduler.runCurrent();job.cancel();scheduler.runCurrent()
        row("cancel-$where","reads" to reads,"captures" to cap.capturesTaken,"completed" to job.isCompleted,"sourceCleanup" to cleaned,"size" to fm.createFile(FileType.SCREENSHOT,DevicePoolId("pool"),device(),t,"batch").length())
    }
    // R1: AWT nearest-neighbour sampling. The four-colour 2x4 witness and exact per-axis
    // index maps through Scalr SPEED (FIT_EXACT only to choose arbitrary 1-row geometry).
    val ties=Scalr.resize(image(2,4),Scalr.Method.SPEED,Scalr.Mode.AUTOMATIC,1,2)
    row("resize-ties","width" to ties.width,"height" to ties.height,"pixels" to (0 until ties.height).flatMap{y->(0 until ties.width).map{x->ties.getRGB(x,y)}})
    fun axis(s:Int,t:Int):List<Int> {
        val im=BufferedImage(s,1,BufferedImage.TYPE_INT_ARGB);for(x in 0 until s)im.setRGB(x,0,0xff000000.toInt() or x)
        val r=Scalr.resize(im,Scalr.Method.SPEED,Scalr.Mode.FIT_EXACT,t,1);return (0 until t).map{r.getRGB(it,0) and 0xffffff}
    }
    row("resize-axis-small","maps" to (1..16).flatMap{s->(1..16).map{t->listOf(s,t,axis(s,t))}})
    val md5={v:List<Int> -> java.security.MessageDigest.getInstance("MD5").digest(v.joinToString(",").toByteArray()).joinToString(""){"%02x".format(it)}}
    row("resize-axis-large","maps" to listOf(61 to 12010,770 to 2718,1080 to 720,1440 to 960,2560 to 1280,3200 to 1080,42787 to 57128,40703 to 56077,50145 to 61292,54245 to 61067,60819 to 65453).map{(s,t)->listOf(s,t,md5(axis(s,t)))})
    // R1: environment-writer character data. stdout markers attribute its success line.
    val envCases=listOf("a\u0001b","\u007f\u0085\u009f\t\ud83d\ude00\r\n\ufffe\uffff<&>\"'","trailing\ud800","x\ud800y","\udc00")
    for((i,value) in envCases.withIndex()){
        val dir=File(out,"environment-cases/$i");dir.mkdirs()
        println("probe: environment-case-$i")
        val err=System.err;val buffer=java.io.ByteArrayOutputStream();System.setErr(java.io.PrintStream(buffer,true,"UTF-8"))
        try{com.github.automatedowl.tools.AllureEnvironmentWriter.allureEnvironmentWriter(com.google.common.collect.ImmutableMap.of("key",value),dir.absolutePath+"/")}finally{System.setErr(err)}
        row("environment-case-$i","value" to value,"stderrHeaders" to buffer.toString("UTF-8").lines().filter{it.isNotEmpty()&&!it[0].isWhitespace()},"size" to File(dir,"environment.xml").length())
    }
    println("probe: environment-cases-end")
    row("rounding","inputs" to listOf(-1.5,-0.5,0.5,4503599627370497.0),"outputs" to listOf(-1.5,-0.5,0.5,4503599627370497.0).map{Math.round(it)})
    // Preserve raw JVM output; comparisons normalize only known UUID/path/footer fields.
    File(out,"probe.json").writeText(gson.toJson(rows))
    root.copyRecursively(File(out,"manifest"),overwrite=true)
    File(out,"runtime-root.txt").writeText(root.absolutePath)
}
