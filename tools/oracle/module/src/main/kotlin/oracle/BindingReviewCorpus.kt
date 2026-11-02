package oracle

import com.fasterxml.jackson.annotation.JsonTypeInfo
import com.fasterxml.jackson.annotation.JsonSubTypes
import com.fasterxml.jackson.core.JsonParser
import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.JsonDeserializer
import com.fasterxml.jackson.databind.DeserializationContext
import com.fasterxml.jackson.databind.annotation.JsonDeserialize
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.malinskiy.marathon.config.FilteringConfiguration
import com.malinskiy.marathon.config.TestFilterConfiguration
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.vendor.android.TestParserConfiguration
import com.malinskiy.marathon.config.vendor.android.SerialStrategy
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import java.io.File
import java.time.Instant

data class ReviewBean(val k: String = "default")
data class ReviewNested(val bean: ReviewBean)
data class ReviewVendor(val vendorConfiguration: VendorConfiguration)
data class ReviewExecution(val executionStrategy: ExecutionStrategyConfiguration)
data class ReviewDefaults(val primitive: Int = 7, val reference: String = "default", val nullable: Int? = 9)

@JsonTypeInfo(use = JsonTypeInfo.Id.NAME, include = JsonTypeInfo.As.PROPERTY, property = "type")
@JsonSubTypes(JsonSubTypes.Type(value = ReviewTreeLeaf::class, name = "tree"))
abstract class ReviewTreeBase
@JsonDeserialize(using = ReviewTreeDeserializer::class)
data class ReviewTreeLeaf(val properties: List<String>) : ReviewTreeBase()
class ReviewTreeDeserializer : JsonDeserializer<ReviewTreeLeaf>() {
    override fun deserialize(p: JsonParser, ctxt: DeserializationContext): ReviewTreeLeaf {
        val node: JsonNode = p.codec.readTree(p)
        return ReviewTreeLeaf(node.fieldNames().asSequence().map { "$it=${node.get(it).asText()}" }.toList())
    }
}

/** Review witnesses also exercise real hierarchies, independently of the Rust test schemas. */
object BindingReviewCorpus {
    fun generate(out: File) {
        val p1 = marathonfileMapper(FixedTimeProvider(Instant.EPOCH), File("/"))
        val p2 = ObjectMapper(YAMLFactory()).registerModule(KotlinModule.Builder().build())
        val rows = mutableListOf<Map<String, Any?>>()
        fun row(target: String, input: String, action: () -> Any?) {
            val result = try { mapOf("ok" to Canonical.render(action())) }
            catch (t: Throwable) { mapOf("error" to generateSequence(t) { it.cause }.map { it.javaClass.name }.toList()) }
            rows.add(mapOf("target" to target, "input" to input, "result" to result))
        }
        for (scalar in listOf("null", "true", "false", "0", "1", "-7", "2147483648", "4294967296", "9223372036854775808", "18446744073709551616", "1.5", "-2.5", "1e309", "''", "'  '", "abc", "'2.5'", "'2147483648'", "'9223372036854775808'", "'1e309'", "'true'", "'TRUE'", "' false '", "'١'", "!!binary YWJj", "[]", "{}", "[1]", "{k: 2}")) {
            row("Accessors", "k: $scalar") {
                val n = p1.readTree("k: $scalar").get("k")
                mapOf("text" to n.asText(), "int" to n.asInt(), "long" to n.asLong(), "double" to n.asDouble(), "boolean" to n.asBoolean())
            }
        }
        for (input in listOf("outer: {k: 7}\nk: 99", "outer: {k: 7}", "k: 1\nk: 2", "k: null", "[ {k: 7} ]", "abc")) {
            row("Lookup", input) {
                val n = p1.readTree(input)
                mapOf("get" to n.get("k")?.asText(), "find" to n.findValue("k")?.asText())
            }
        }
        for (input in listOf("{}", "primitive: null", "reference: null", "nullable: null", "primitive: '12'", "nullable: '12'")) {
            row("Defaults", input) { p1.readValue(input, ReviewDefaults::class.java) }
        }
        for ((target, type, input) in listOf(
            Triple("Nested", ReviewNested::class.java, "bean: null"),
            Triple("Nested", ReviewNested::class.java, "bean: {}"),
            Triple("VendorField", ReviewVendor::class.java, "vendorConfiguration: null"),
            Triple("ExecutionField", ReviewExecution::class.java, "executionStrategy: null")
        )) row(target, input) { p1.readValue(input, type) }
        for (input in listOf("allowlist: [null]", "allowlist: null", "allowlist: [{type: method, type: allure}]", "allowlist: [{type: allure, type: method}]", "allowlist: [{regex: abc, type: method, type: allure}]", "allowlist: [{type: null}]", "allowlist: [{type: []}]", "allowlist: [{type: {}}]", "allowlist: [abc]")) {
            row("Filtering", input) { p1.readValue(input, FilteringConfiguration::class.java) }
        }
        for ((target, type, ids) in listOf(
            Triple("Vendor", VendorConfiguration::class.java, listOf("stub", "empty")),
            Triple("Parser", TestParserConfiguration::class.java, listOf("local", "remote"))
        )) for (input in listOf("{type: ${ids[0]}, type: ${ids[1]}}", "{type: ${ids[1]}, type: ${ids[0]}}", "{type: null}", "{type: []}", "{type: {}}", "abc", "null", "{type: ${ids[0]}}", "{type: null, type: ${ids[0]}}", "{type: [], type: ${ids[0]}}", "{type: ${ids[0]}, type: null}", "{type: ${ids[0]}, type: []}")) {
            row(target, input) { p1.readValue(input, type) }
        }
        for (scalar in listOf("null", "17", "1.5", "true", "false", "!!binary YWJj", "'abc'", "[]", "{}")) {
            row("Regex", "k: $scalar") { p1.readValue("k: $scalar", RegexField::class.java) }
        }
        for (input in listOf("''", "'  '", "' relative/key '", "' /a/../b '", "17", "true", "\"a\\u0000b\"", "null")) {
            row("P2File", input) { p2.readValue(input, File::class.java)?.path }
        }
        for ((target, type) in listOf("SerialStrategy" to SerialStrategy::class.java,
            "AndroidRecorder" to com.malinskiy.marathon.config.vendor.android.RecorderType::class.java,
            "IosRecorder" to com.malinskiy.marathon.config.vendor.apple.ios.RecorderType::class.java)) {
            for (input in listOf("bogus", "1", "true", "null", "''", "[]", "{}", if (target == "SerialStrategy") "automatic" else "video")) {
                row(target, input) { p1.readValue(input, type) }
            }
        }
        for (input in listOf("{type: fixed-size, size: true}", "{type: fixed-size, size: bad}", "{type: fixed-size, size: '2.5'}", "{type: fixed-size, size: {}}", "{type: fixed-size, size: 1, percentile: abc}", "{type: fixed-size, size: 1, percentile: zzz}")) {
            row("Batching", input) { p1.readValue(input, BatchingStrategyConfiguration::class.java) }
        }
        for (input in listOf("{type: tree, k: x}", "{k: x, type: tree}", "{type: tree, type: later, k: x}", "{type: null, type: tree, k: x}", "{nested: {type: retained}, type: tree}")) {
            row("SubtypeTree", input) { p1.readValue(input, ReviewTreeBase::class.java) }
        }
        // One list plus 499 map/list pairs and the final map = Jackson's maximum depth, 1000.
        row("DeepFilters", "499") {
            val filter = "{type: composition, op: UNION, filters: [".repeat(499) + "{type: allure}" + "]}".repeat(499)
            p1.readValue("[$filter]", p1.typeFactory.constructCollectionType(List::class.java, TestFilterConfiguration::class.java)) as List<*>
            499
        }
        out.writeText((listOf(mapOf("description" to "WP 2.1 review round 1 JVM witnesses, Jackson 2.17.2")) + rows).joinToString("\n") { Json.compact(it) } + "\n")
    }
}
