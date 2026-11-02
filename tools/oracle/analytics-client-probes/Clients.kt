// Real frozen providers, trackers and query clients; responses are scripted, not service evidence.
import com.malinskiy.marathon.analytics.external.influx.*
import com.malinskiy.marathon.analytics.external.influx2.*
import com.malinskiy.marathon.analytics.external.graphite.*
import com.malinskiy.marathon.analytics.metrics.remote.graphite.QueryableGraphiteClient
import com.malinskiy.marathon.analytics.internal.sub.TestEvent
import com.malinskiy.marathon.config.AnalyticsConfiguration.*
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.test.Test
import com.google.gson.GsonBuilder
import com.sun.net.httpserver.HttpServer
import org.influxdb.BatchOptions
import org.influxdb.dto.Query
import java.net.*
import java.io.*
import java.time.Instant
import java.util.concurrent.*
import okhttp3.OkHttpClient

val gson = GsonBuilder().disableHtmlEscaping().serializeNulls().create()
val exchanges = java.util.Collections.synchronizedList(mutableListOf<Any>())
val observations = mutableListOf<Any>()
val dev = DeviceInfo(OperatingSystem("Fake OS"), "serial: /x", "model", "maker", NetworkState.CONNECTED, emptyList(), true)
fun event(status: TestStatus, escaped: Boolean=false) = TestEvent(Instant.ofEpochMilli(-1001),DevicePoolId("omni"),dev,
    TestResult(Test(if(escaped) "p a,=\\" else "pkg", "Clazz", "method", emptyList()),dev,"batch",status,1000,6000),false)
class Peer(val scenario:String, var existing:Boolean=false, val missing:Boolean=false, val duplicate:Boolean=false, val code:Int=200, val override:String?=null, var writeFailures:Int=0, val writeCode:Int=500, val writeError:String="{\"error\":\"temporary\"}", val retryAfter:String?=null, val redirect:Boolean=false, val writeRedirect:Int=0): AutoCloseable {
    val writes = CountDownLatch(1)
    var redirected=false
    val server = HttpServer.create(InetSocketAddress("127.0.0.1",0),0)
    val url get()="http://127.0.0.1:${server.address.port}"
    init {
        server.createContext("/") { e ->
            val body=e.requestBody.readBytes().toString(Charsets.UTF_8)
            val path=e.requestURI.toString()
            val history = scenario.startsWith("history-") || scenario.startsWith("recreate-")
            val case = scenario.substringAfterLast('-')
            val value = mapOf("empty-rate" to 0.0,"empty-time" to 300000.0,"p50" to 5000.0,"p90" to 9000.0,"r50" to 2000.0,"r90" to 4000.0,"s50" to 1.0,"s65" to (5.0/6.0),"s120" to 0.5)[case] ?: 5000.0
            val empty = case=="er" || case=="et"
            val rate = case.startsWith("s") || case=="er"
            val response = override ?: when {
                history && path.startsWith("/render") -> if(empty) "" else "pkg--Clazz--method,0,1,1|${if(rate) value*100 else value}\n"
                history && path.startsWith("/api/v2/query") -> if(empty) "" else "#datatype,string,long,string,double\n#group,false,false,true,false\n#default,_result,,,\n,result,table,testname,_value\n,,0,pkg.Clazz.method,$value\n"
                history && path.contains("SELECT") -> if(empty) "{\"results\":[{}]}" else "{\"results\":[{\"series\":[{\"name\":\"tests\",\"tags\":{\"testname\":\"pkg.Clazz.method\"},\"columns\":[\"time\",\"${if(rate) "mean" else "percentile"}\"],\"values\":[[null,$value]]}]}]}"

                path.startsWith("/api/v2/orgs") -> "{\"orgs\":"+(if(missing) "[]" else if(duplicate) "[{\"id\":\"id1\",\"name\":\"org\"},{\"id\":\"id2\",\"name\":\"org\"}]" else "[{\"id\":\"id1\",\"name\":\"org\"}]")+"}"
                path.startsWith("/api/v2/buckets") && e.requestMethod=="GET" -> if(existing) "{\"buckets\":[{\"id\":\"bucket1\",\"name\":\"bucket\"}]}" else "{\"buckets\":[]}"
                path.startsWith("/api/v2/buckets") -> "{\"id\":\"bucket1\",\"name\":\"bucket\",\"orgID\":\"id1\"}"
                path.startsWith("/api/v2/query") -> "#datatype,string,long,string,double\n#group,false,false,true,false\n#default,_result,,,\n,result,table,testname,_value\n,,0,pkg.Clazz\u0023method,5000.0\n"
                path.startsWith("/render") -> "one\r\n\n \nlast\n"
                (path+body).contains("SHOW+DATABASES") -> "{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\""+(if(existing) "db" else "other")+"\"]]}]}]}"
                path.contains("SELECT") -> "{\"results\":[{\"series\":[{\"name\":\"tests\",\"tags\":{\"testname\":\"pkg.Clazz\u0023method\"},\"columns\":[\"time\",\"percentile\"],\"values\":[[null,5000.0]]}]}]}"
                else -> "{\"results\":[{}]}"
            }
            if(body.startsWith("q=CREATE+DATABASE") || path.startsWith("/api/v2/buckets") && e.requestMethod=="POST") existing=true
            val write=path.startsWith("/write") || path.startsWith("/api/v2/write") || path.startsWith("/final/")
            val redirection=redirect && !redirected && path.startsWith("/api/v2/query")
            val writeRedirection=writeRedirect!=0 && !redirected && write
            if(redirection || writeRedirection) redirected=true
            val status=if(redirection) 302 else if(writeRedirection) writeRedirect else if(write) {if(writeFailures-->0) writeCode else 204} else code
            val responseHeaders=mutableMapOf<String,String>()
            if(redirection) responseHeaders["Location"]="/final"
            if(writeRedirection) responseHeaders["Location"]="/final"+path
            if(retryAfter!=null) responseHeaders["Retry-After"]=retryAfter
            for((key,value) in responseHeaders) e.responseHeaders.set(key,value)
            val actualResponse=if(writeRedirection) "" else if(write && status!=204) writeError else response
            val headers=mapOf("authorization" to e.requestHeaders.getFirst("Authorization"),"content-type" to e.requestHeaders.getFirst("Content-Type"),"accept" to e.requestHeaders.getFirst("Accept"))
            exchanges.add(mapOf("scenario" to scenario,"method" to e.requestMethod,"path" to path,"headers" to headers,"body" to body,"status" to status,"response_headers" to responseHeaders,"response" to if(status==204) "" else actualResponse))
            e.responseHeaders.set("Content-Type", if(path.startsWith("/api/v2/query")) "text/csv" else "application/json")
            if(status==204) e.sendResponseHeaders(204,-1) else { val bytes=actualResponse.toByteArray();e.sendResponseHeaders(status,bytes.size.toLong());e.responseBody.write(bytes) }
            e.close()
            if(status==204) writes.countDown()
        }
        server.start()
    }
    override fun close() { server.stop(0) }
}
fun v1(p:Peer,user:String="",password:String="",debug:Boolean=false) = InfluxDbProvider(InfluxDbConfiguration(p.url,user,password,"db",InfluxDbConfiguration.RetentionPolicyConfiguration.default),debug).createDb()
fun v2(p:Peer,debug:Boolean=false) = InfluxDb2Provider(InfluxDb2Configuration(p.url,"dummy-token","org","bucket"),debug).createDb()
fun attempt(name:String, body:()->Unit) { try { body();observations+=mapOf("scenario" to name,"result" to "ok") } catch(e:Throwable) { observations+=mapOf("scenario" to name,"class" to e.javaClass.name,"message" to e.message,"cause" to e.cause?.javaClass?.name) } }
fun histories() {
    val anchor=Instant.parse("2026-01-01T00:00:00Z")
    val names=listOf("er","et","p50","p90","r50","r90","s50","s65","s120")
    val minutes=listOf(0L,0,120,120,25,35,50,65,120)
    val percentiles=listOf<Double?>(null,90.0,50.0,90.0,50.0,90.0,null,null,null)
    val historyEvents=listOf(1L,10,20,30,40,60,70,80,90,100).mapIndexed {i,m ->
        val e=event(if(i<5) TestStatus.PASSED else TestStatus.FAILURE)
        val at=anchor.minusSeconds(m*60)
        e.copy(instant=at,testResult=e.testResult.copy(startTime=at.toEpochMilli()-(i+1)*1000,endTime=at.toEpochMilli()),`final`=true)
    }
    for(backend in listOf("v1","v2","graphite")) for((index,name) in names.withIndex()) {
        val scenario="history-$backend-$name"
        Peer(scenario).use { p ->
            val source:com.malinskiy.marathon.analytics.metrics.remote.RemoteDataSource
            var carbon:ServerSocket?=null
            var carbonRead:Future<List<String>>?=null
            var executor:ExecutorService?=null
            if(backend=="v1") {
                val db=v1(p);val tracker=InfluxDbTracker(db,"db","rpMarathon",false);historyEvents.forEach{tracker.track(it)};db.flush()
                source=com.malinskiy.marathon.analytics.metrics.remote.influx.InfluxDBDataSource(db,"db","rpMarathon")
            } else if(backend=="v2") {
                val db=v2(p);val tracker=InfluxDb2Tracker(db,false);historyEvents.forEach{tracker.track(it)}
                source=com.malinskiy.marathon.analytics.metrics.remote.influx2.InfluxDB2DataSource(db,"bucket")
            } else {
                carbon=ServerSocket(0,50,InetAddress.getByName("127.0.0.1"));executor=Executors.newSingleThreadExecutor()
                val server=carbon
                carbonRead=executor.submit<List<String>> { (1..10).map { server.accept().use { it.getInputStream().readBytes().toString(Charsets.UTF_8) } } }
                val tracker=GraphiteTracker(BasicGraphiteClient("127.0.0.1",carbon.localPort,"prefix"),false);historyEvents.forEach{tracker.track(it)}
                source=com.malinskiy.marathon.analytics.metrics.remote.graphite.GraphiteDataSource(QueryableGraphiteClient("127.0.0.1",p.server.address.port),"prefix")
            }
            val provider=com.malinskiy.marathon.analytics.external.MetricsProviderImpl(source,0.0,java.time.Duration.ofMinutes(5))
            val limit=anchor.minusSeconds(minutes[index]*60)
            val value=percentiles[index]?.let{provider.executionTime(historyEvents[0].testResult.test,it,limit)} ?: provider.successRate(historyEvents[0].testResult.test,limit)
            observations+=mapOf("scenario" to scenario,"value" to value)
            provider.close()
            if(carbonRead!=null) {carbonRead.get(5,TimeUnit.SECONDS).forEach { exchanges+=mapOf("scenario" to scenario,"protocol" to "carbon","body" to it) };carbon!!.close();executor!!.shutdown()}
        }
    }
}
fun recreation() {
    for(backend in listOf("v1","v2")) Peer("recreate-$backend-p50").use {p ->
        if(backend=="v1") {
            v1(p).close()
            val second=v1(p);val tracker=InfluxDbTracker(second,"db","rpMarathon",false);tracker.track(event(TestStatus.PASSED));second.close()
            val third=v1(p);third.query(Query("SELECT x FROM tests","db"));third.close()
        } else {
            v2(p).close()
            val second=v2(p);val tracker=InfluxDb2Tracker(second,false);tracker.track(event(TestStatus.PASSED));tracker.track(event(TestStatus.PASSED).let{it.copy(testResult=it.testResult.copy(test=it.testResult.test.copy(method="method2")))});second.close()
            val third=v2(p);third.queryApi.query("from(bucket: \"bucket\")");third.close()
        }
    }
}

fun faults() {
    for(backend in listOf("v1","v2")) for(mode in listOf("eof","reset","slow")) {
        ServerSocket(0,50,InetAddress.getByName("127.0.0.1")).use {server ->
            val release=CountDownLatch(1)
            val executor=Executors.newSingleThreadExecutor()
            try {
                val accepted=java.util.concurrent.atomic.AtomicInteger()
                val worker=executor.submit { try {
                    // Initial readiness: a generous accept limit, failing the probe rather than hanging under load.
                    server.soTimeout=30000
                    server.accept().use {socket ->
                        accepted.incrementAndGet()
                        socket.getInputStream().read(ByteArray(8192))
                        if(mode=="reset") socket.setSoLinger(true,0)
                        if(mode=="slow") check(release.await(30,TimeUnit.SECONDS))
                    }
                    // Retry detection: a short window in which a transport retry would reconnect.
                    server.soTimeout=500
                    if(mode!="slow") try {server.accept().use{socket -> accepted.incrementAndGet();socket.getInputStream().read(ByteArray(8192));socket.getOutputStream().write("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".toByteArray())}} catch(e:SocketTimeoutException) {}
                } finally {server.close()} }
                val url="http://127.0.0.1:${server.localPort}"
                val http=OkHttpClient.Builder().readTimeout(100,TimeUnit.MILLISECONDS).writeTimeout(100,TimeUnit.MILLISECONDS).connectTimeout(100,TimeUnit.MILLISECONDS)
                if(backend=="v1") {
                    val client=org.influxdb.InfluxDBFactory.connect(url,http)
                    attempt("fault-$backend-$mode"){client.query(Query("SELECT x FROM tests","db"))};client.close()
                } else {
                    val options=com.influxdb.client.InfluxDBClientOptions.builder().url(url).org("org").bucket("bucket").authenticateToken("dummy-token".toCharArray()).okHttpClient(http).build()
                    val client=com.influxdb.client.InfluxDBClientFactory.create(options)
                    attempt("fault-$backend-$mode"){client.queryApi.query("bad")};client.close()
                }
                release.countDown();worker.get(40,TimeUnit.SECONDS)
                observations+=mapOf("scenario" to "fault-$backend-$mode-count","connections" to accepted.get())
            } finally { release.countDown();executor.shutdownNow() }
        }
    }
    ServerSocket(0,50,InetAddress.getByName("127.0.0.1")).use {server ->
        val executor=Executors.newSingleThreadExecutor()
        val worker=executor.submit{server.accept().use{it.setSoLinger(true,0)}}
        attempt("carbon-reset"){BasicGraphiteClient("127.0.0.1",server.localPort,null).send(GraphiteMetric("m".repeat(8*1024*1024),"1",0))}
        worker.get(5,TimeUnit.SECONDS);executor.shutdown()
    }
}

fun main(args:Array<String>) {
    val logAppender=ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent>().apply{start()}
    (org.slf4j.LoggerFactory.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME) as ch.qos.logback.classic.Logger).addAppender(logAppender)
    for(at in listOf(Instant.MIN,Instant.MAX,Instant.ofEpochSecond(-1,999999999),Instant.ofEpochSecond(0,999999))) observations+=mapOf("scenario" to "v2-instant","instant" to at.toString(),"line" to com.influxdb.client.write.Point("tests").time(at,com.influxdb.client.domain.WritePrecision.MS).addField("duration",1.0).toLineProtocol())

    faults()
    val nf=java.text.NumberFormat.getInstance(java.util.Locale.ENGLISH).apply{maximumFractionDigits=340;minimumFractionDigits=1;isGroupingUsed=false}
    for(duration in listOf(0L,-1L,1L,999L,9999999L,10000000L,86400000L,123456789012L,999999999999L,1000000000000L,-1000000000000L,(1L shl 53)-1,1L shl 53,-(1L shl 53),Long.MAX_VALUE,Long.MIN_VALUE)) observations+=mapOf("scenario" to "duration-format","value" to duration,"text" to nf.format(duration.toDouble()))
    for(url in listOf("host","ftp://host","http:\\host","http://user:pass@host","http://host?q=1","http://host#fragment")) attempt("url-$url"){com.influxdb.client.InfluxDBClientOptions.builder().url(url).build()}
    histories()
    recreation()

    val defaults=BatchOptions.DEFAULTS
    observations+=mapOf("scenario" to "defaults","actions" to defaults.actions,"flushMs" to defaults.flushDuration,"jitterMs" to defaults.jitterDuration,"buffer" to defaults.bufferLimit,"precision" to defaults.precision.toString(),"drop" to defaults.isDropActionsOnQueueExhaustion)
    val http=OkHttpClient.Builder().connectTimeout(60,TimeUnit.SECONDS).readTimeout(60,TimeUnit.SECONDS).writeTimeout(60,TimeUnit.SECONDS).build()
    observations+=mapOf("scenario" to "http","connectMs" to http.connectTimeoutMillis,"readMs" to http.readTimeoutMillis,"writeMs" to http.writeTimeoutMillis,"callMs" to http.callTimeoutMillis,"retry" to http.retryOnConnectionFailure,"redirects" to http.followRedirects)
    Peer("v1-config-escaping").use {p ->
        val c=InfluxDbConfiguration(p.url,"u é😀","päss","db a+é",InfluxDbConfiguration.RetentionPolicyConfiguration("rp a+é","30d","30m",2,true))
        val db=InfluxDbProvider(c).createDb();InfluxDbTracker(db,c.dbName,c.retentionPolicyConfiguration.name,false).track(event(TestStatus.PASSED));db.query(Query("SELECT x",c.dbName));db.close()
    }
    Peer("v2-config-escaping").use {p ->
        val c=InfluxDb2Configuration(p.url,"dummy-token","org","bucket a+é")
        val db=InfluxDb2Provider(c).createDb();InfluxDb2Tracker(db,false).track(event(TestStatus.PASSED));db.close()
    }
    for ((name,user,password,debug) in listOf(listOf("v1-fresh","","","false"),listOf("v1-auth","user","pass","true"),listOf("v1-partial","user","","false"),listOf("v1-existing","","","false"))) {
        Peer(name,existing=name=="v1-existing").use { p ->
            val db=v1(p,user,password,debug.toBoolean());val tracker=InfluxDbTracker(db,"db","rpMarathon",false)
            TestStatus.values().forEach { tracker.track(event(it,true)) }
            InfluxDbTracker(db,"db","rpMarathon",true).track(event(TestStatus.PASSED))
            tracker.close();attempt(name+"-close-again") { tracker.close() }
        }
    }
    for((name,code,error) in listOf(Triple("v1-retry",500,"temporary"),Triple("v1-permanent",400,"field type conflict"))) Peer(name,writeFailures=2,writeCode=code,writeError="{\"error\":\"$error\"}").use {p ->
        val db=v1(p);val tracker=InfluxDbTracker(db,"db","rpMarathon",false);tracker.track(event(TestStatus.PASSED));db.flush();db.flush();db.close()
    }
    for((name,code,retryAfter) in listOf(Triple("v2-http408",408,null),Triple("v2-http503",503,null),Triple("v2-http503-retry",503,"0"))) Peer(name,writeFailures=1,writeCode=code,writeError="{\"code\":\"temporary\",\"message\":\"temporary\"}",retryAfter=retryAfter).use {p ->val db=v2(p);InfluxDb2Tracker(db,false).track(event(TestStatus.PASSED));db.close()}
    for((name,code) in listOf("v2-write-307" to 307,"v2-write-308" to 308)) Peer(name,writeRedirect=code).use {p -> val db=v2(p);InfluxDb2Tracker(db,false).track(event(TestStatus.PASSED));db.close()}
    Peer("v1-write-307",writeRedirect=307).use {p -> val db=v1(p);InfluxDbTracker(db,"db","rpMarathon",false).track(event(TestStatus.PASSED));db.flush();db.close()}
    Peer("v2-empty-token").use {p -> attempt("v2-empty-token"){val db=InfluxDb2Provider(InfluxDb2Configuration(p.url,"","org","bucket")).createDb();InfluxDb2Tracker(db,false).track(event(TestStatus.PASSED));db.close()}}
    for((name,error) in listOf("v1-error-database" to "database not found","v1-error-field" to "field type conflict","v1-error-auth" to "authorization failed","v1-error-other" to "bad")) Peer(name,code=400,override="{\"error\":\"$error\"}").use {p -> val db=org.influxdb.InfluxDBFactory.connect(p.url);attempt(name){db.query(Query("SELECT x FROM tests","db"))};db.close()}
    for((name,csv) in listOf("v2-csv-long" to "#datatype,string,long,string,long\n#group,false,false,true,false\n#default,_result,,,\n,result,table,testname,_value\n,,0,test,1.5\n","v2-csv-double" to "#datatype,string,long,string,double\n#group,false,false,true,false\n#default,_result,,,\n,result,table,testname,_value\n,,0,test,xyz\n")) Peer(name,override=csv).use {p -> val db=com.influxdb.client.InfluxDBClientFactory.create(p.url,"dummy-token".toCharArray(),"org","bucket");attempt(name){db.queryApi.query("bad")};db.close()}
    Peer("v2-redirect",redirect=true).use {p ->val db=v2(p);attempt("v2-redirect"){db.queryApi.query("bad")};db.close()}
    Peer("v1-timer").use { p -> val db=v1(p);InfluxDbTracker(db,"db","rpMarathon",false).track(event(TestStatus.PASSED));check(p.writes.await(5,TimeUnit.SECONDS));db.close() }
    Peer("v1-threshold").use { p -> val db=v1(p);val tracker=InfluxDbTracker(db,"db","rpMarathon",false);repeat(1000){tracker.track(event(TestStatus.PASSED))};check(p.writes.await(5,TimeUnit.SECONDS));db.close() }
    for(name in listOf("v2-fresh","v2-existing","v2-duplicate","v2-missing")) {
        Peer(name,existing=name=="v2-existing",missing=name=="v2-missing",duplicate=name=="v2-duplicate").use { p -> attempt(name) {
            val db=v2(p,name=="v2-duplicate");val tracker=InfluxDb2Tracker(db,false)
            TestStatus.values().forEach { tracker.track(event(it,true)) }
            InfluxDb2Tracker(db,true).track(event(TestStatus.PASSED));tracker.close();tracker.close()
        } }
    }
    Peer("v1-query",existing=true).use { p -> val db=v1(p);observations+=mapOf("scenario" to "v1-query","value" to db.query(Query("SELECT x FROM tests","db")));db.close() }
    Peer("v2-query",existing=true).use { p -> val db=v2(p);attempt("v2-query") { observations+=mapOf("scenario" to "v2-records","values" to db.queryApi.query("from(bucket: \"bucket\")").flatMap{it.records}.map{it.values}) };db.close() }
    for(code in listOf(400,401,429,500)) {
        Peer("v2-error-$code",code=code,override="{\"code\":\"invalid\",\"message\":\"bad\"}").use { p ->
            val db=com.influxdb.client.InfluxDBClientFactory.create(p.url,"dummy-token".toCharArray(),"org","bucket")
            attempt("v2-error-$code") { db.queryApi.query("bad") }; db.close()
        }
    }
    Peer("v2-invalid-json",override="invalid").use { p -> attempt("v2-invalid-json"){v2(p).close()} }
    Peer("v2-invalid-csv",override="invalid").use { p -> val db=com.influxdb.client.InfluxDBClientFactory.create(p.url,"dummy-token".toCharArray(),"org","bucket");attempt("v2-invalid-csv"){db.queryApi.query("bad")};db.close() }
    Peer("graphite-http",code=500).use { p -> observations+=mapOf("scenario" to "graphite-http","lines" to QueryableGraphiteClient("127.0.0.1",p.server.address.port).query("a b+*",Instant.parse("2026-01-01T00:00:00Z"))) }
    val sender=object:DataSender { override fun sendData(host:String,port:Int,callback:(PrintWriter)->Unit) { val out=StringWriter();callback(PrintWriter(out));observations+=mapOf("scenario" to "graphite-payload","body" to out.toString()) } }
    val graphite=BasicGraphiteClient("host",2003,"a.b",sender);val gt=GraphiteTracker(graphite,false);TestStatus.values().forEach{gt.track(event(it,true))};graphite.send(emptyList())
    val failing=object:DataSender { override fun sendData(host:String,port:Int,callback:(PrintWriter)->Unit) { val w=PrintWriter(object:Writer(){override fun write(c:CharArray,o:Int,n:Int){throw IOException("reset")};override fun flush(){};override fun close(){}});callback(w);observations+=mapOf("scenario" to "printwriter-suppression","error" to w.checkError()) } }
    attempt("carbon-suppression") { BasicGraphiteClient("host",2003,null,failing).send(GraphiteMetric("metric","1",-1001)) }
    attempt("carbon-refused") { BasicGraphiteClient("127.0.0.1",1,null).send(GraphiteMetric("metric","1",0)) }
    val refused=com.influxdb.client.InfluxDBClientFactory.create("http://127.0.0.1:1","dummy-token".toCharArray(),"org","bucket")
    attempt("v2-refused-query"){refused.queryApi.query("bad")};attempt("v2-refused-tracker"){InfluxDb2Tracker(refused,false).track(event(TestStatus.PASSED))};refused.close()
    observations+=mapOf("scenario" to "java-form","input" to "a b+*~é","encoded" to java.net.URLEncoder.encode("a b+*~é","UTF-8"))
    for(at in listOf(Instant.MIN,Instant.MAX,Instant.ofEpochMilli(-1001),Instant.ofEpochMilli(Long.MIN_VALUE),Instant.ofEpochMilli(Long.MAX_VALUE))) attempt("epoch-$at"){observations+=mapOf("scenario" to "epoch-millis","instant" to at.toString(),"millis" to at.toEpochMilli())}
    observations+=mapOf("scenario" to "custom-logs","records" to logAppender.list.filter{it.loggerName.endsWith("InfluxDb2Provider") || it.loggerName.endsWith("InfluxDb2Tracker")}.map{mapOf("logger" to it.loggerName,"level" to it.level.toString(),"message" to it.formattedMessage,"cause" to it.throwableProxy?.className)})
    val out=File(args[0]);File(out,"exchanges.jsonl").writeText(exchanges.joinToString("\n",postfix="\n"){gson.toJson(it)})
    File(out,"observations.jsonl").writeText(observations.joinToString("\n",postfix="\n"){gson.toJson(if(it is Map<*,*> && (it["scenario"] as? String)?.startsWith("fault-")==true) it.mapValues{(key,value)->if(key=="message" && value is String) value.replace(Regex("http://127[.]0[.]0[.]1:[0-9]+/"),"http://127.0.0.1:<port>/") else value} else it)})
}
