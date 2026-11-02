// WP 3.3: execute the pinned filters and allure-test-filter 2.29.0, never a reimplementation.
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.config.TestFilterConfiguration as C
import com.malinskiy.marathon.execution.TestFilter
import com.malinskiy.marathon.execution.filter.*
import com.malinskiy.marathon.extension.toTestFilter
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.MetaProperty
import com.linkedin.dex.parser.DecodedValue.*
import io.qameta.allure.testfilter.*
import ch.qos.logback.classic.Logger
import ch.qos.logback.core.read.ListAppender
import ch.qos.logback.classic.spi.ILoggingEvent
import org.slf4j.LoggerFactory
import java.io.File
import java.nio.file.Files
import java.util.Optional

val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
val logs = ListAppender<ILoggingEvent>().apply { start() }
fun capture() { (LoggerFactory.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME) as Logger).addAppender(logs) }
fun logRows(root: File? = null) = logs.list.map { e -> mapOf("logger" to e.loggerName, "level" to e.level.toString(), "message" to e.formattedMessage.replace(root?.absolutePath ?: "<never>", "<tmp>"), "cause" to e.throwableProxy?.className) }.also { logs.list.clear() }
fun error(e: Throwable): Map<String,Any?> = mapOf("class" to e.javaClass.name, "message" to e.message, "cause" to e.cause?.let(::error))
fun output(block: () -> Any?): Any? = try { block() } catch (e: Throwable) { mapOf("error" to error(e)) }
fun names(tests: List<Test>) = tests.map { listOf(it.pkg, it.clazz, it.method) }
fun meta(tests: List<Test>) = tests.map { it.metaProperties.map { it.name } }
fun t(pkg: String="pkg", clazz: String="Class", method: String="method", annotations: List<MetaProperty> = emptyList()) = Test(pkg,clazz,method,annotations)
fun a(name: String, value: Any? = "yes") = MetaProperty(name, mapOf("value" to value))
fun spec(type: String, regex: String?=null, values: List<String>?=null, enabled: Boolean=true, subpackages: Boolean=false) = linkedMapOf<String,Any?>("type" to type,"regex" to regex,"values" to values,"enabled" to enabled,"subpackages" to subpackages)
fun config(s: Map<String,Any?>): C {
 val r=(s["regex"] as String?)?.toRegex(); val v=s["values"] as List<String>?; val enabled=s["enabled"] as Boolean
 return when(s["type"]) {
 "class" -> C.SimpleClassnameFilterConfiguration(r,v,enabled=enabled)
 "simple" -> C.SimpleTestnameFilterConfiguration(r,v,enabled=enabled)
 "fqclass" -> C.FullyQualifiedClassnameFilterConfiguration(r,v,enabled=enabled)
 "fqtest" -> C.FullyQualifiedTestnameFilterConfiguration(r,v,enabled=enabled)
 "method" -> C.TestMethodFilterConfiguration(r,v,enabled=enabled)
 "package" -> C.TestPackageFilterConfiguration(r,v,subpackages=s["subpackages"] as Boolean,enabled=enabled)
 "annotation" -> C.AnnotationFilterConfiguration(r,v,enabled=enabled)
 else -> error("bad spec")
 }
}
fun main(args: Array<String>) {
 capture()
 if(args[0]=="--supply") {
  var plan: TestPlan? = null
  val supplied=output { plan=FileTestPlanSupplier().supply().orElse(null); plan?.javaClass?.simpleName }
  val selected=output { (plan as? TestPlanV1_0)?.isSelected("7","pkg.Class.method") }
  println("RESULT:"+gson.toJson(mapOf("supplied" to supplied,"selected" to selected,"logs" to logRows(File(".").absoluteFile.parentFile))))
  return
 }
 val dir=File(args[0]).apply { mkdirs() }
 val header=mapOf("schema" to "filters-v1", "source" to "e47352f2", "java" to System.getProperty("java.version"), "allure" to "2.29.0", "normalization" to "temporary root=<tmp>; DecodedNull suffix=<identity>; no stack frames")
 var normalRoot: String? = null
 fun write(name: String, rows: List<Any?>) { File(dir,"$name.jsonl").writeText((listOf(header)+rows).joinToString("\n") { gson.toJson(it).replace(normalRoot ?: "<never>", "<tmp>") }+"\n") }
 val tests=listOf(t("pkg","Class","one",listOf(a("A"))),t("pkg.sub","Other","two",listOf(a("B"))),t("","Class","one"),t(" \t","Class","three"),t("pkg2","Class","one"),t("pkg","Class","one",listOf(a("B"))))
 val specs=listOf(spec("class","Class"),spec("simple",values=listOf("Class#one")),spec("fqclass",values=listOf("pkg.Class","Class")),spec("fqtest",".*#one"),spec("method","one"),spec("package",values=listOf("pkg"),subpackages=true),spec("package",values=listOf("pkg")),spec("annotation","A"),spec("annotation",values=listOf("B")),spec("annotation","A",listOf("B")),spec("class","Class",listOf("Other")),spec("class",values=emptyList()),spec("annotation"))
 val leafRows=mutableListOf<Any?>()
 for(s in specs) for(enabled in listOf(true,false)) {
  val actual=s.toMutableMap().apply { put("enabled",enabled) };val filter=config(actual).toTestFilter()
  leafRows+=mapOf("config" to actual,"input" to names(tests),"annotations" to meta(tests),"filter" to names(filter.filter(tests)),"filterNot" to names(filter.filterNot(tests)))
 }
 for(count in listOf(0,1,3,-3)) for(index in listOf(-1,0,1,2)) {
  val f=C.FragmentationFilterConfiguration(index,count).toTestFilter()
  leafRows+=mapOf("fragment" to listOf(index,count),"input" to names(tests),"filter" to output { names(f.filter(tests)) },"filterNot" to output { names(f.filterNot(tests)) },"empty" to output { names(f.filter(emptyList())) })
 }
 val d6="^((?!Abstract).)*Test$"
 val d6Filter=C.SimpleClassnameFilterConfiguration(d6.toRegex()).toTestFilter()
 leafRows+=mapOf("d6" to d6,"input" to listOf("SimpleTest","AbstractTest","SomeClass"),"filter" to d6Filter.filter(listOf(t(clazz="SimpleTest"),t(clazz="AbstractTest"),t(clazz="SomeClass"))).map { it.clazz })
 write("leaves",leafRows)
 val compositions=mutableListOf<Any?>()
 for(op in C.CompositionFilterConfiguration.OPERATION.values()) for(n in 0..3) {
  val trace=mutableListOf<Any?>()
  val children=(0 until n).map { i -> object:TestFilter {
   override fun filter(input: List<Test>): List<Test> { trace+=mapOf("child" to i,"input" to names(input));return input.filter { if(i==0) it.metaProperties.any { it.name=="B" } else if(i==1) it.pkg=="pkg" else it.method=="three" } }
   override fun filterNot(input: List<Test>) = error("unused")
  }}
  val f=CompositionFilter(children,op,true)
  val selected=f.filter(tests);val filterTrace=trace.toList();trace.clear()
  val excluded=f.filterNot(tests)
  compositions+=mapOf("op" to op.toString(),"children" to n,"input" to names(tests),"annotations" to meta(tests),"filter" to names(selected),"filterMeta" to meta(selected),"trace" to filterTrace,"filterNot" to names(excluded),"filterNotMeta" to meta(excluded),"notTrace" to trace)
 }
 val nested=C.CompositionFilterConfiguration(listOf(C.SimpleClassnameFilterConfiguration("Other".toRegex()),C.CompositionFilterConfiguration(listOf(C.TestMethodFilterConfiguration("three".toRegex())),C.CompositionFilterConfiguration.OPERATION.SUBTRACT)),C.CompositionFilterConfiguration.OPERATION.UNION).toTestFilter()
 compositions+=mapOf("nested" to true,"filter" to names(nested.filter(tests)))
 write("composition",compositions)
 val wrappers:List<Pair<String,Any?>> = listOf("string" to "a)b(value=x)","null" to null,"byte" to DecodedByte(-1),"short" to DecodedShort(-2),"int" to DecodedInt(-3),"long" to DecodedLong(-4),"float" to DecodedFloat(-0.0f),"double" to DecodedDouble(Double.NaN),"char" to DecodedChar(')'),"boolean" to DecodedBoolean(true),"decodedString" to DecodedString("a)b"),"type" to DecodedType("Ltype;"),"enum" to DecodedEnum("ENUM"),"decodedNull" to DecodedNull,"nested" to DecodedArrayValue(arrayOf(DecodedString("a)b"),DecodedNull)),"empty" to DecodedArrayValue(emptyArray()))
 val identity=Integer.toHexString(System.identityHashCode(DecodedNull))
 val annotationRows=wrappers.map { (kind,value) ->
  val rendered=value.toString();val extracted=rendered.substringAfter("(value=").substringBefore(")")
  val f=C.AnnotationDataFilterConfiguration("A".toRegex(),Regex.escape(extracted).toRegex()).toTestFilter()
  val test=t(annotations=listOf(a("A",arrayOf(value))))
  mapOf("kind" to kind,"rendered" to rendered.replace(identity,"<identity>"),"extracted" to extracted.replace(identity,"<identity>"),"selected" to f.filter(listOf(test)).size)
 }.toMutableList<Any?>()
 for((kind,value) in listOf("scalar" to "a)b", "list" to listOf("a","b"), "map" to linkedMapOf("x" to 1,"y" to null))) {
  val f=C.AnnotationDataFilterConfiguration("A".toRegex(),Regex.escape(value.toString()).toRegex()).toTestFilter()
  annotationRows+=mapOf("kind" to kind,"rendered" to value.toString(),"selected" to f.filter(listOf(t(annotations=listOf(a("A",value))))).size)
 }
 val convert=Class.forName("com.malinskiy.marathon.android.DexTestParserKt").getDeclaredMethod("toMetaProperty",com.linkedin.dex.parser.TestAnnotation::class.java).apply { isAccessible=true }
 for((kind,value) in wrappers.filter { it.second is com.linkedin.dex.parser.DecodedValue }) {
  val annotation=com.linkedin.dex.parser.TestAnnotation("A",mapOf("value" to DecodedArrayValue(arrayOf(value as com.linkedin.dex.parser.DecodedValue))),false)
  val property=convert.invoke(null,annotation) as MetaProperty
  val rendered=(property.values["value"] as Array<*>).single().toString()
  val extracted=rendered.substringAfter("(value=").substringBefore(")")
  val f=C.AnnotationDataFilterConfiguration("A".toRegex(),Regex.escape(extracted).toRegex()).toTestFilter()
  annotationRows+=mapOf("producer" to kind,"extracted" to extracted.replace(identity,"<identity>"),"selected" to f.filter(listOf(t(annotations=listOf(property)))).size)
 }
 val renderedElements=mutableListOf<String>()
 fun counted(text:String)=object { override fun toString():String { renderedElements+=text;return text } }
 val eager=C.AnnotationDataFilterConfiguration("A".toRegex(),"match".toRegex()).toTestFilter()
 val eagerTest=t(annotations=listOf(a("A",arrayOf(counted("match"),counted("later")))))
 val eagerSelected=eager.filter(listOf(eagerTest)).size
 annotationRows+=mapOf("eagerRendering" to renderedElements,"selected" to eagerSelected)
 write("annotation",annotationRows)
 val tmp=Files.createTempDirectory("filter-probes-").toFile()
 normalRoot=tmp.absolutePath
 try {
  val file=File(tmp,"values")
  val contents=listOf(" #comment\nClass#method\nx#literal\nx #comment\n\nx\tx#literal\n", "\uFEFFClass\rClass\nOther\r\nClass\n", "x\u00A0#literal\n\u00A0\u001c\n", "x #comment\u2028after\ny #comment\u2028\nz #comment\u0085", "\t\n\u2007#comment\n")
  val fileRows=mutableListOf<Any?>()
  for(content in contents) { file.writeText(content);fileRows+=mapOf("bytes" to file.readBytes().map { it.toInt() and 255 },"values" to FilterValuesFileReader().readValues(file)) }
  file.writeBytes(byteArrayOf(0xed.toByte(),0xa0.toByte(),0x80.toByte(),10,0xf0.toByte(),0x80.toByte(),0x80.toByte(),0x80.toByte()))
  fileRows+=mapOf("bytes" to file.readBytes().map { it.toInt() and 255 },"values" to FilterValuesFileReader().readValues(file))
  file.delete()
  val f=C.SimpleClassnameFilterConfiguration(file=file).toTestFilter()
  val cacheSteps=mutableListOf<Any?>()
  cacheSteps+=mapOf("step" to "empty","result" to names(f.filter(emptyList())),"logs" to logRows(tmp))
  cacheSteps+=mapOf("step" to "missing","result" to names(f.filter(tests)),"logs" to logRows(tmp))
  file.writeText("Class")
  cacheSteps+=mapOf("step" to "created","result" to names(f.filterNot(tests)),"logs" to logRows(tmp))
  fileRows+=mapOf("cache" to cacheSteps)
  file.delete();file.mkdir()
  val retry=C.SimpleClassnameFilterConfiguration(file=file).toTestFilter()
  fileRows+=mapOf("retryError" to output { names(retry.filter(tests)) })
  file.delete();file.writeText("Class")
  fileRows+=mapOf("retrySuccess" to names(retry.filter(tests)))
  fileRows+=mapOf("disabled" to names(C.SimpleClassnameFilterConfiguration(file=File(tmp,"missing"),enabled=false).toTestFilter().filter(tests)),"logs" to logRows(tmp))
  fileRows+=mapOf("explicitEmpty" to names(C.SimpleClassnameFilterConfiguration(values=emptyList(),file=File(tmp,"missing")).toTestFilter().filter(tests)),"logs" to logRows(tmp))
  val mutation=C.SimpleClassnameFilterConfiguration(file=file).toTestFilter()
  val before=mutation.filter(tests);file.writeText("Other");val after=mutation.filterNot(tests)
  fileRows+=mapOf("mutationBefore" to names(before),"mutationAfter" to names(after))
  val invalidFile=File(tmp,"bad\u0000path")
  fileRows+=mapOf("invalidPath" to "bad\u0000path","values" to FilterValuesFileReader().readValues(invalidFile),"logs" to logRows(tmp))
  write("files",fileRows)
  val allRows=mutableListOf<Any?>()
  val jsons=listOf("{\"version\":\"1.0\",\"tests\":[{\"id\":\"7\"}]}","{\"version\":\"1.0\",\"tests\":[{\"id\":\"pkg.Class.method\"}]}","{\"version\":\"1.0\",\"tests\":[{\"selector\":\"7\"}]}","{\"version\":\"1.0\",\"tests\":[]}","{\"version\":\"1.0\"}","{\"version\":\"1.0\",\"tests\":null}","{\"version\":\"1.0\",\"tests\":[null]}","{\"version\":\"1.0\",\"tests\":[{\"id\":7,\"selector\":true}]}","{\"version\":\"1.0\",\"tests\":[{\"id\":null,\"selector\":null,\"extra\":5}]}","{\"version\":\"1.0\",\"tests\":{}}","{\"version\":\"1\",\"tests\":[]}","{\"version\":1.0,\"tests\":[]}","{\"tests\":[]}","{}","null","[]","1","bad")
  for(json in jsons) {
   File(tmp,"plan.json").writeText(json)
   val cmd=listOf(System.getProperty("java.home")+"/bin/java","-cp",System.getProperty("java.class.path"),"FilterProbesKt","--supply")
   fun supply(env:Map<String,String>): Any? {
    val p=ProcessBuilder(cmd).directory(tmp).redirectErrorStream(true);p.environment().remove("ALLURE_TESTPLAN_PATH");p.environment().remove("AS_TESTPLAN_PATH");p.environment().putAll(env)
    val process=p.start();val text=process.inputStream.bufferedReader().readText();check(process.waitFor()==0)
    return gson.fromJson(text.lineSequence().first { it.startsWith("RESULT:") }.removePrefix("RESULT:"), Any::class.java)
   }
   allRows+=mapOf("json" to json,"env" to mapOf("ALLURE_TESTPLAN_PATH" to "plan.json"),"output" to supply(mapOf("ALLURE_TESTPLAN_PATH" to "plan.json")))
   if(json==jsons.first()) for(env in listOf(emptyMap(),mapOf("AS_TESTPLAN_PATH" to "plan.json"),mapOf("ALLURE_TESTPLAN_PATH" to "", "AS_TESTPLAN_PATH" to "plan.json"),mapOf("ALLURE_TESTPLAN_PATH" to "missing","AS_TESTPLAN_PATH" to "plan.json"),mapOf("ALLURE_TESTPLAN_PATH" to " ","AS_TESTPLAN_PATH" to "plan.json"),mapOf("ALLURE_TESTPLAN_PATH" to ".","AS_TESTPLAN_PATH" to "plan.json"))) allRows+=mapOf("json" to json,"env" to env,"output" to supply(env))
  }
  val path=FileTestPlanSupplier::class.java.getDeclaredMethod("tryGetPath",String::class.java).apply { isAccessible=true }
  path.invoke(FileTestPlanSupplier(),"bad\u0000path")
  allRows+=mapOf("invalidPathLogs" to logRows())
  for(enabled in listOf(true,false)) for(planKind in listOf("absent","unknown","v1")) {
   var calls=0
   val plan:TestPlan?=when(planKind) { "absent"->null;"unknown"->TestPlanUnknown();else->TestPlanV1_0().setTests(listOf(TestPlanV1_0.TestCase().setId("7"),TestPlanV1_0.TestCase().setSelector("pkg.Class.two"),TestPlanV1_0.TestCase().setId(""))) }
   val f=AllureTestFilter(C.AllureFilterConfiguration(enabled),TestPlanSupplier { calls++; Optional.ofNullable(plan) })
   val inputs=listOf(t(method="one",annotations=listOf(a("io.qameta.allure.AllureId","7"))),t(method="two"),t(method="three",annotations=listOf(a("io.qameta.allure.AllureId",7),a("io.qameta.allure.kotlin.AllureId","7"))),t(method="four",annotations=listOf(a("io.qameta.allure.kotlin.AllureId",""))))
   val empty=f.filter(emptyList());val allow=f.filter(inputs);val block=f.filterNot(inputs)
   allRows+=mapOf("wrapper" to planKind,"enabled" to enabled,"empty" to names(empty),"filter" to names(allow),"filterNot" to names(block),"calls" to calls)
  }
  for(op in C.CompositionFilterConfiguration.OPERATION.values()) for(n in 0..2) {
   var calls=0
   val children=(0 until n).map { AllureTestFilter(C.AllureFilterConfiguration(),TestPlanSupplier { calls++;Optional.empty() }) }
   val f=CompositionFilter(children,op,true)
   allRows+=mapOf("compositionOp" to op.toString(),"children" to n,"filter" to names(f.filter(emptyList())),"calls" to calls)
  }
  var attempts=0
  val retryPlan=AllureTestFilter(C.AllureFilterConfiguration(),TestPlanSupplier { attempts++;if(attempts==1) throw IllegalStateException("retry");Optional.empty() })
  allRows+=mapOf("supplierRetry" to listOf(output { names(retryPlan.filter(emptyList())) },output { names(retryPlan.filterNot(tests)) }),"calls" to attempts)
  write("allure",allRows)
 } finally { tmp.deleteRecursively() }
}
