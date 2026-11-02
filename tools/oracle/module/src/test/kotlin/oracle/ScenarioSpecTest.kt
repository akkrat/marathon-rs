package oracle

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ObjectNode
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File

class ScenarioSpecTest {
    @TempDir lateinit var dir: File
    private val mapper = ObjectMapper()
    private fun valid() = mapper.readTree("""{"format":"marathon-replay/scenario@1","name":"schema",
        "tests":[{"pkg":"p","clazz":"C","method":"test"}],"devices":[{"serial":"d"}],
        "events":[{"at":0,"event":"connect","device":"d"},{"at":10,"event":"end"}]}""") as ObjectNode
    private fun parse(node: ObjectNode): ScenarioSpec {
        val file = File(dir, "script.json"); file.writeText(node.toString()); return ScenarioSpec.parse(file)
    }
    @Test fun rejects_coercion_duplicates_unknown_fields_and_early_end() {
        assertEquals(2, parse(valid()).events.size)
        val changes: List<(ObjectNode) -> Unit> = listOf(
            { (it["events"][0] as ObjectNode).put("at", 1000.9) },
            { (it["events"][0] as ObjectNode).put("at", "0") },
            { (it["events"][0] as ObjectNode).put("at", -1) },
            { (it["events"][0] as ObjectNode).set<com.fasterxml.jackson.databind.JsonNode>("at", mapper.readTree("9223372036854775808")) },
            { (it["devices"] as com.fasterxml.jackson.databind.node.ArrayNode).add(it["devices"][0].deepCopy<com.fasterxml.jackson.databind.JsonNode>()) },
            { (it["events"][0] as ObjectNode).put("typo", true) },
            { (it["events"][0] as ObjectNode).put("device", "missing") },
            { (it["events"][0] as ObjectNode).apply { put("event", "end"); remove("device") } },
            { (it["devices"][0] as ObjectNode).put("prepareTimeMillis", 1.5) },
            { (it["events"][1] as ObjectNode).put("device", "d") }
        )
        changes.forEach { change -> val node = valid(); change(node); assertThrows(IllegalArgumentException::class.java, { parse(node) }, node.toString()) }
    }
    @Test fun validates_expectation_objects_keys_and_nested_values_before_execution() {
        val invalid = listOf("[]", "27", "null", "true", "{\"typo\":true}",
            "{\"completed\":1}", "{\"value\":\"true\"}", "{\"exception\":false}",
            "{\"rawJson\":[]}", "{\"files\":[]}", "{\"files\":{\"a.txt\":27}}",
            "{\"files\":{\"a.txt\":null}}", "{\"files\":{\"a.txt\":{}}}")
        invalid.forEach { json ->
            val node = valid(); node.set<com.fasterxml.jackson.databind.JsonNode>("expect", mapper.readTree(json))
            assertThrows(IllegalArgumentException::class.java, { parse(node) }, json)
        }
        for (json in listOf("{}", "{\"completed\":true,\"value\":false,\"exception\":\"RuntimeException\",\"rawJson\":\"expected.json\",\"files\":{\"a.txt\":\"text\"}}")) {
            val node = valid(); node.set<com.fasterxml.jackson.databind.JsonNode>("expect", mapper.readTree(json))
            assertNotNull(parse(node).expect)
        }
    }

}
