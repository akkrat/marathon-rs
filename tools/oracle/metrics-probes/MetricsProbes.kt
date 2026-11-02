// WP 3.4a: real e47352f2 classes, scripted sources and intercepted HTTP (no network).
import com.malinskiy.marathon.analytics.external.*
import com.malinskiy.marathon.analytics.metrics.remote.*
import com.malinskiy.marathon.analytics.metrics.remote.graphite.*
import com.malinskiy.marathon.analytics.metrics.remote.influx.InfluxDBDataSource
import com.malinskiy.marathon.analytics.metrics.remote.influx2.InfluxDB2DataSource
import com.malinskiy.marathon.config.*
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.log.MarathonLogging
import com.malinskiy.marathon.execution.*
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.toSafeTestName
import java.time.*
import java.io.File
import java.lang.reflect.Proxy
import org.slf4j.LoggerFactory
import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import com.google.gson.GsonBuilder
import org.influxdb.InfluxDB
import org.influxdb.dto.Query
import org.influxdb.dto.QueryResult
import com.influxdb.client.InfluxDBClient
import com.influxdb.client.QueryApi
import okhttp3.*
import okhttp3.ResponseBody.Companion.toResponseBody

val gson = GsonBuilder().disableHtmlEscaping().serializeNulls().create()
val rows = mutableListOf<Any>()
val appender = ListAppender<ILoggingEvent>()
fun logs(): List<List<String>> = appender.list.map { listOf(it.loggerName, it.level.toString(), it.formattedMessage.replace(Regex(" in -?[0-9]+ ms$"), " in <ms> ms")) }.also { appender.list.clear() }
fun bits(d: Double) = java.lang.Long.toUnsignedString(java.lang.Double.doubleToLongBits(d),16).padStart(16,'0')
fun test(i: Int) = Test("pkg$i","clazz$i","method$i",emptyList())
val device = DeviceInfo(OperatingSystem("Fake OS"),"serial","model","maker",NetworkState.CONNECTED,emptyList(),true)
val limit = Instant.parse("2026-01-01T00:00:00Z")
fun result(i: Int, duration: Long, status: String) = TestResult(test(i),device,"batch",TestStatus.valueOf(status),1000,1000+duration)
class Source(val mode: String): RemoteDataSource {
    val calls = mutableListOf<List<String>>()
    var attempt = 0
    fun called(kind: String,p: Double?,limit: Instant) {
        calls += listOf(kind,p?.toString() ?: "",limit.toString())
        attempt++
        if (mode=="fail" || mode=="retry" && attempt<=2) throw RuntimeException("unreadable")
    }
    override fun requestAllSuccessRates(limit: Instant): List<SuccessRate> {
        called("rate",null,limit)
        return if(mode=="duplicate") listOf(SuccessRate(test(0).toSafeTestName(),0.1),SuccessRate(test(0).toSafeTestName(),0.9)) else emptyList()
    }
    override fun requestAllExecutionTimes(percentile: Double,limit: Instant): List<ExecutionTime> {
        called("time",percentile,limit)
        return when(mode) {
            "duplicate" -> listOf(ExecutionTime(test(0).toSafeTestName(),100.5),ExecutionTime(test(0).toSafeTestName(),7000.0))
            "history" -> listOf(ExecutionTime(test(0).toSafeTestName(),7000.0))
            else -> emptyList()
        }
    }
    override fun close() {}
}
data class Op(val op: String,val test: Int=0,val p: String="80.0",val duration: Long=0,val status:String="PASSED",val tests:List<Int> = emptyList(),val limit:String="2026-01-01T00:00:00Z")
fun scenario(name:String,mode:String,min:Int?,ops:List<Op>) {
    MarathonLogging.debug=true
    val source=Source(mode);val provider=MetricsProviderImpl(source,0.0,Duration.ofMillis(300000),min);logs()
    val steps=ops.map { op ->
        val value=when(op.op) {
            "time" -> bits(provider.executionTime(test(op.test),op.p.toDouble(),Instant.parse(op.limit)))
            "rate" -> bits(provider.successRate(test(op.test),Instant.parse(op.limit)))
            "observe" -> {provider.observeExecutionTimes(listOf(result(op.test,op.duration,op.status)));null}
            "coverage" -> {provider.reportExecutionTimeCoverage(op.tests.map(::test),op.p.toDouble(),Instant.parse(op.limit),"sorting");null}
            else -> error(op.op)
        }
        mapOf("input" to op,"bits" to value,"logs" to logs(),"calls" to source.calls.toList()).also {source.calls.clear()}
    }
    rows+=mapOf("name" to name,"mode" to mode,"min" to min,"steps" to steps)
}
fun <T> proxy(type:Class<T>, block:(java.lang.reflect.Method,Array<out Any?>)->Any?):T = type.cast(Proxy.newProxyInstance(type.classLoader,arrayOf(type)){_,m,a -> block(m,a?:emptyArray())})
fun queryRows(): List<Any> {
    val out=mutableListOf<Any>()
    for (p in listOf(0.0,50.0,90.0,99.9)) for (instant in listOf("2026-01-01T00:00:00Z","2026-01-01T00:00:00.123Z","2026-01-01T00:00:00.123456Z","2026-01-01T00:00:00.123456789Z")) {
        for (rp in listOf("rpMarathon","a\"b")) {
            val client=proxy(InfluxDB::class.java){m,a -> if(m.name=="query") {val q=a[0] as Query;out+=mapOf("backend" to "influx","p" to p.toString(),"limit" to instant,"policy" to rp,"command" to q.command,"database" to q.database); QueryResult().apply{results=emptyList()}} else null }
            val source=InfluxDBDataSource(client,"db",rp);source.requestAllSuccessRates(Instant.parse(instant));source.requestAllExecutionTimes(p,Instant.parse(instant))
        }
        for (bucket in listOf("bucket","a\"b")) {
            val query=proxy(QueryApi::class.java){m,a -> if(m.name=="query") {out+=mapOf("backend" to "flux","p" to p.toString(),"limit" to instant,"bucket" to bucket,"command" to a[0]);emptyList<Any>()} else null }
            val client=proxy(InfluxDBClient::class.java){m,_ -> if(m.name=="getQueryApi") query else null }
            val source=InfluxDB2DataSource(client,bucket);source.requestAllSuccessRates(Instant.parse(instant));source.requestAllExecutionTimes(p,Instant.parse(instant))
        }
    }
    for(prefix in listOf(null,"","a","a.b","é.雪","a b\t.c")) for(line in listOf("pkg--clazz--method,1,2,3|50","pkg--clazz--method,1,2,3|None","pkg--clazz--method,1,2,3|50,None","no-pipe","pkg--clazz--method,1,2,3|NaN","pkg--clazz--method,1,2,3|  ","pkg--clazz--method,1,2,3| bad ")) for(time in listOf(false,true)) {
        var url="";var target=""
        val client=QueryableGraphiteClient("localhost")
        val http=OkHttpClient.Builder().addInterceptor {chain -> url=chain.request().url.toString();target=chain.request().url.queryParameter("target")!!;Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("OK").body(line.toResponseBody()).build() }.build()
        client.javaClass.getDeclaredField("okHttpClient").apply{isAccessible=true}.set(client,http)
        val source=GraphiteDataSource(client,prefix)
        var error:String?=null;var errorMessage:String?=null;var value:String?=null;var name:String?=null
        try { if(time) { val r=source.requestAllExecutionTimes(99.9,limit).single();value=bits(r.percentile);name=r.testName } else {val r=source.requestAllSuccessRates(limit).single();value=bits(r.mean);name=r.testName} } catch(e:Exception){error=e.javaClass.name;errorMessage=e.message}
        out+=mapOf("backend" to "graphite","prefix" to prefix,"line" to line,"time" to time,"target" to target,"url" to url,"bits" to value,"name" to name,"error" to error,"errorMessage" to errorMessage)
    }
    return out
}
fun influxErrorRows(): List<Any> = listOf(false,true).flatMap { statement ->
    listOf(false,true).map { time ->
        MarathonLogging.debug=true
        val text="server rejected query"
        var calls=0
        val reply=QueryResult().apply {
            if(statement) results=listOf(QueryResult.Result().apply {error=text}) else error=text
        }
        val client=proxy(InfluxDB::class.java){m,_ -> if(m.name=="query") {calls++;reply} else null}
        val provider=MetricsProviderImpl(InfluxDBDataSource(client,"db","rp"),0.0,Duration.ofMillis(300000))
        logs()
        val value=if(time) provider.executionTime(test(0),80.0,limit) else provider.successRate(test(0),limit)
        val causes=appender.list.mapNotNull {e -> e.throwableProxy?.let {
            mapOf("logger" to e.loggerName,"level" to e.level.toString(),"class" to it.className,"message" to it.message)
        }}
        mapOf("statement" to statement,"time" to time,"error" to text,"bits" to bits(value),"calls" to calls,"causes" to causes,"logs" to logs())
    }
}
fun main(args:Array<String>) {
    val root=LoggerFactory.getLogger(Logger.ROOT_LOGGER_NAME) as Logger
    root.detachAndStopAllAppenders();appender.start();root.addAppender(appender)
    for(mode in listOf("empty","fail","retry","duplicate")) scenario("cache-$mode",mode,null,listOf(Op("time"),Op("time"),Op("rate"),Op("rate"),Op("time",test=1),Op("time",p="NaN"),Op("time",p="NaN"),Op("time",p="0.0"),Op("time",p="-0.0"),Op("rate",test=1),Op("time",limit="2026-01-02T00:00:00Z")))
    for(min in listOf(null,-1,0,1,2,10)) for(mode in listOf("empty","history")) scenario("observed-$mode-$min",mode,min,
        listOf(Op("time",test=2),Op("observe",duration=90000),Op("time"),Op("time",test=2),Op("observe",test=1,duration=20000,status="FAILURE"),Op("time",test=2),Op("observe",duration=10000),Op("observe",duration=40000),Op("observe",test=3,duration=-1),Op("observe",test=4,duration=1,status="IGNORED"),Op("observe",test=5,duration=1,status="INCOMPLETE"),Op("observe",test=6,duration=1,status="ASSUMPTION_FAILURE")) + listOf("0.0","100.0","120.0","-1.0","NaN","50.0").flatMap{listOf(Op("time",p=it),Op("time",test=2,p=it))})
    for(min in listOf(null,2)) for(tests in listOf(emptyList(),listOf(0),listOf(0,1),listOf(0,1,2,0))) scenario("coverage-$min-$tests","history",min,listOf(Op("coverage",tests=tests),Op("time",test=3)))
    for(order in listOf(listOf(0,1,2),listOf(1,2,0))) scenario("average-order-$order","empty",3,
        order.map {Op("observe",test=it,duration=if(it==0)9007199254740992L else 1L)} + listOf(Op("time",test=6),Op("observe",test=3,duration=Long.MAX_VALUE),Op("time",test=3)))
    val dir=File(args.single());dir.mkdirs();File(dir,"provider.jsonl").writeText(rows.joinToString("\n",postfix="\n"){gson.toJson(it)})
    File(dir,"queries.jsonl").writeText(queryRows().joinToString("\n",postfix="\n"){gson.toJson(it)})
    val levels=listOf(false,true).map { debug ->
        MarathonLogging.debug=true
        val provider=MetricsProviderImpl(Source("empty"),0.0,Duration.ZERO)
        val noop=NoOpMetricsProvider()
        val config=Configuration.Builder("",File("")).apply {vendorConfiguration=VendorConfiguration.StubVendorConfiguration;analyticsConfiguration=AnalyticsConfiguration.InfluxDbConfiguration("host","user","password","db",AnalyticsConfiguration.InfluxDbConfiguration.RetentionPolicyConfiguration.default)}.build()
        val factory=MetricsProviderFactory(config)
        factory.create()
        MarathonLogging.debug=debug
        logs()
        listOf("MetricsProviderImpl","NoOpMetricsProvider","MetricsProviderFactory").forEach {
            val logger=LoggerFactory.getLogger(it) as Logger
            logger.debug("probe DEBUG");logger.warn("probe WARN")
        }
        provider.executionTime(test(0),80.0,limit);provider.successRate(test(0),limit);noop.reportExecutionTimeCoverage(listOf(test(0)),80.0,limit,"sorting")
        mapOf("debug" to debug,"events" to logs(),"levels" to listOf("MetricsProviderImpl","NoOpMetricsProvider","MetricsProviderFactory").associateWith{(LoggerFactory.getLogger(it) as Logger).level.toString()})
    }
    File(dir,"loggers.jsonl").writeText(levels.joinToString("\n",postfix="\n"){gson.toJson(it)})
    File(dir,"influx-errors.jsonl").writeText(influxErrorRows().joinToString("\n",postfix="\n"){gson.toJson(it)})
}
