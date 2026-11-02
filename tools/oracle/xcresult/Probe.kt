// Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt
import com.fasterxml.jackson.databind.*
import java.io.File
import kotlin.reflect.full.primaryConstructor

private val plain = ObjectMapper().configure(com.fasterxml.jackson.core.JsonGenerator.Feature.ESCAPE_NON_ASCII, true)
private val pkg = "com.malinskiy.marathon.vendor.ios.xcrun.xcresulttool."
private fun project(value: Any?): Any? = when (value) {
    null, is String, is Number, is Boolean -> value
    is java.util.Date -> value.time
    is List<*> -> value.map(::project)
    else -> value.javaClass.declaredFields.filter { !java.lang.reflect.Modifier.isStatic(it.modifiers) }.associate { field ->
        field.isAccessible = true
        field.name to project(field.get(value))
    }
}
private fun probe(id: String, target: String, input: String, proposal: String? = null) {
    val row = linkedMapOf<String, Any?>("id" to id, "target" to target, "input" to input)
    if (proposal != null) row["proposal"] = proposal
    try {
        val value = AppleJsonMapper.readValue(input, Class.forName(pkg + target))
        row["value"] = project(value)
    } catch (error: Throwable) {
        row["error"] = generateSequence(error) { it.cause }.map { it.javaClass.name }.toList()
        row["path"] = (error as? JsonMappingException)?.path?.map { it.fieldName ?: "[${it.index}]" } ?: emptyList<String>()
        row["message"] = error.message
    }
    println(plain.writeValueAsString(row))
}
fun main(args: Array<String>) {
    println(plain.writeValueAsString(mapOf("schema" to 1, "reference" to "e47352f2", "xcresultSchema" to "3.39", "signature" to "PS0lrtUj8Aw=", "jackson" to "2.17.2", "kotlin" to "1.9.24", "kotlinpoet" to "1.16.0", "jdk" to System.getProperty("java.version"))))
    val schema = plain.readTree(File(args[0], "fixtures/xcresult/schema-3.39.json"))
    val constructors = schema["types"].filter { it["kind"].asText() == "object" }.associate { definition ->
        val name = definition["type"]["name"].asText()
        name to Class.forName(pkg + name).kotlin.primaryConstructor!!.parameters.map { parameter ->
            mapOf("name" to parameter.name, "type" to parameter.type.toString(), "default" to parameter.isOptional)
        }
    }
    plain.writerWithDefaultPrettyPrinter().writeValue(File(args[1]), constructors)
    val cases = listOf("{}", "{\"testsCount\":null}", "{\"testsCount\":{\"_value\":null}}", "{\"testsCount\":{}}", "{\"testsCount\":1}", "{\"testsCount\":{\"_value\":\"42\",\"_type\":{\"_name\":\"Wrong\"}}}")
    cases.forEachIndexed { i, input -> probe("int-shape-$i", "ResultMetrics", input, if (i in 3..4) "P1" else null) }
    listOf("0", "-2147483648", "2147483647", "2147483648", "-2147483649", "1.9", "-1.9", "true", "\" 42 \"", "\"\"", "\"null\"", "\"１２\"", "\"1.5\"", "[]", "{}") .forEachIndexed { i, value -> probe("int-$i", "ResultMetrics", "{\"testsCount\":{\"_value\":$value}}", if (i in listOf(3,4,11)) "P2" else null) }
    listOf("{}", "{\"hash\":null}", "{\"hash\":{\"_value\":null}}", "{\"hash\":{\"_value\":true}}", "{\"hash\":{\"_value\":12}}", "{\"hash\":{\"_value\":1.5}}", "{\"hash\":{\"_value\":\"é😀\\ud800\"}}", "{\"hash\":{\"_value\":[]}}") .forEachIndexed { i, input -> probe("string-$i", "ObjectID", input) }
    listOf("null", "true", "false", "0", "1", "-2", "1.5", "\"true\"", "\"True\"", "\"FALSE\"", "\" yes \"", "\"\"", "\"null\"").forEachIndexed { i, value -> probe("bool-$i", "CodeCoverageInfo", "{\"hasCoverageData\":{\"_value\":$value}}") }
    listOf("null", "1", "1.25", "\"2.5\"", "\"\"", "\"null\"", "\"NaN\"", "\"Infinity\"", "\"-Infinity\"", "true", "\"0x1.0p2\"", "\"１２\"").forEachIndexed { i, value -> probe("double-$i", "ConsoleLogItem", "{\"timestamp\":{\"_value\":$value}}", if (i == 10) "P2" else null) }
    listOf("{}", "{\"name\":null}", "{\"name\":{\"_value\":null}}", "{\"name\":{\"_value\":\"\"}}", "{\"name\":{}}", "{\"name\":false}") .forEachIndexed { i, input -> probe("optional-$i", "ActionAbstractTestSummary", input, if (i >= 4) "P1" else null) }
    listOf("null", "{}", "{\"_values\":null}", "{\"_values\":{}}", "[]", "1", "{\"_values\":[]}", "{\"_values\":[null]}", "{\"_values\":[{},{}]}", "{\"_values\":[1]}", "{\"_values\":[{\"_value\":\"x\"},null,{\"_value\":\"x\"}]}") .forEachIndexed { i, value ->
        probe("object-list-$i", "ActionTestPlanRunSummaries", "{\"summaries\":$value}")
        probe("string-list-$i", "ActionTestActivitySummary", "{\"failureSummaryIDs\":$value}")
        probe("double-list-$i", "ActionTestPerformanceMetricSummary", "{\"measurements\":$value}", if (i in 8..9) "P1" else null)
        probe("nullable-list-$i", "OracleNullableLists", "{\"objects\":$value,\"strings\":null}")
    }
    listOf("-0", "1e7", "1e-8", "1e309").forEachIndexed { i, value -> probe("string-number-$i", "ObjectID", "{\"hash\":{\"_value\":$value}}") }
    probe("double-list-values", "ActionTestPerformanceMetricSummary", "{\"measurements\":{\"_values\":[{\"_value\":\"1.25\"},null,{\"_value\":\"1.25\"}]}}")
    probe("string-list-values", "ActionTestActivitySummary", "{\"failureSummaryIDs\":[{\"_value\":\"x\"},null,{\"_value\":\"x\"}]}")
    probe("nullable-list-missing", "OracleNullableLists", "{}")
    probe("string-list-bare", "ActionTestActivitySummary", "{\"failureSummaryIDs\":[\"x\",null,\"x\"]}", "P1")
    probe("string-list-wrapped-elements", "ActionTestActivitySummary", "{\"failureSummaryIDs\":[{\"_value\":\"x\"}]}")
    val dates = listOf("null", "0", "-1", "1700000000123", "1.5", "\"2023-11-14T22:13:20Z\"", "\"2023-11-14T23:13:20.123456789+0100\"", "\"2023-11-14T23:13:20.1239+01:00\"", "\"1969-12-31T23:59:59.999Z\"", "\"Tue, 14 Nov 2023 22:13:20 GMT\"", "\"2023-02-30T00:00:00Z\"", "\"\"", "\"null\"", "\"2023-11-14\"", "\"1700000000123\"")
    dates.forEachIndexed { i, value -> probe("date-$i", "ActionTestAttachment", "{\"timestamp\":{\"_value\":$value}}", if (i in listOf(9,10,13)) "P3" else null) }
    listOf("null", "{\"_value\":null}", "{\"_value\":\"\"}", "{\"_value\":\"null\"}", "{\"_value\":1}").forEachIndexed { i, value ->
        probe("optional-int-$i", "ActionTestRepetitionPolicySummary", "{\"iteration\":$value}")
        probe("optional-double-$i", "ActionTestMetadata", "{\"duration\":$value}")
    }
    probe("required-object-missing", "ActionsInvocationRecord", "{}")
    probe("required-object-null", "ActionsInvocationRecord", "{\"metrics\":null,\"issues\":{}}")
    probe("required-date-missing", "ActionRecord", "{}")
    probe("unknown-fields", "Reference", "{\"id\":{\"_value\":\"0~id\",\"ignored\":true},\"targetType\":{\"name\":{\"_value\":\"Child\"},\"supertype\":{\"name\":{\"_value\":\"Base\"}}},\"_type\":{\"_name\":\"Other\"},\"unknown\":{\"_value\":false}}")
    probe("base-not-subtype", "ActionTestSummaryIdentifiableObject", "{\"_type\":{\"_name\":\"ActionTestSummary\"},\"identifier\":{\"_value\":\"Class/test\"},\"failureSummaries\":{\"_values\":[{}]}}")
    listOf("null", "{", "{\"testsCount\":", "[]", "true", "{} {}", "{\"testsCount\":{\"_value\":1},\"testsCount\":{\"_value\":2}}").forEachIndexed { i, input -> probe("json-$i", "ResultMetrics", input) }
    // Jackson 2.17 StreamReadConstraints: nesting depth 1000 is accepted, 1001 rejected.
    for (depth in listOf(1000, 1001)) {
        probe("depth-array-$depth", "ResultMetrics", "{\"u\":" + "[".repeat(depth - 1) + "]".repeat(depth - 1) + "}")
    }
    // Only the rejected chain: an accepted one projects 1000 levels deep, beyond the gate's Python JSON check.
    probe("depth-supertype-1001", "TypeDefinition", "{\"supertype\":".repeat(1000) + "{}" + "}".repeat(1000))
    probe("SCRIPTED-root", "ActionsInvocationRecord", File(args[0], "fixtures/xcresult/scripted/root.json").readText())
    probe("SCRIPTED-tests-0", "ActionTestPlanRunSummaries", File(args[0], "fixtures/xcresult/scripted/tests-0.json").readText())
    for (file in listOf("sample_1", "sample_2", "testplanrunsummary_1", "xcode27_failure", "xcode27_testplanrunsummaries")) {
        val target = if (file.contains("summary") || file.contains("summaries")) "ActionTestPlanRunSummaries" else "ActionsInvocationRecord"
        probe("fixture-$file", target, File(args[0], "fixtures/vendor-apple-base/fixtures/xctesttool/$file.json").readText())
    }
}
