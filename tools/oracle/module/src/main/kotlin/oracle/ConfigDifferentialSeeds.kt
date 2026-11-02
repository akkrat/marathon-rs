package oracle

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import java.io.File

/** Generation-only YAML to JSON conversion; execution always uses ConfigDump.kt. */
fun configDifferentialSeeds(args: List<String>) {
    require(args.size == 2)
    val root = File(args[0])
    val json = ObjectMapper()
    val yaml = ObjectMapper(YAMLFactory())
    val rows = mutableListOf<Map<String, Any?>>()
    File(root, "fixtures/oracle/config-validation-corpus.jsonl").readLines().drop(1).forEach { line ->
        val row = json.readTree(line)
        val serialized = row["serialized"]
        if (!serialized.isNull) rows += linkedMapOf(
            "id" to row["id"].asText(),
            "source" to "fixtures/oracle/config-validation-corpus.jsonl",
            "value" to yaml.readValue(serialized.asText(), Map::class.java),
        )
    }
    File(args[1]).writeText(json.writerWithDefaultPrettyPrinter().writeValueAsString(rows) + "\n")
}
