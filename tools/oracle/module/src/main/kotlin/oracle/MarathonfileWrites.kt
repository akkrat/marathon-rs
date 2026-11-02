package oracle

import com.fasterxml.jackson.databind.JsonNode
import org.apache.commons.text.StringSubstitutor
import org.apache.commons.text.lookup.StringLookupFactory
import java.io.File
import java.time.Instant
import kotlin.system.exitProcess

/**
 * `marathonfile-writes <Marathonfile> --env-file F`: the decoded values of the Marathonfile keys that make marathon write
 * files (outputDir; application and testApplication, whose .ipa/.zip archives are extracted next to them), for
 * run-isolated.sh. The text goes through the same substitution and the same YAML mapper as ConfigurationFactory.parse, so
 * quoting, flow and block styles, aliases and duplicate keys decode as marathon decodes them. Keys are found at any depth.
 *
 * Prints {"marathonfileDir": ..., "values": [{"key", "value" | "nonScalar": true}], "error": null | {...}}; when
 * substitution or YAML parsing fails, marathon fails at the same point before writing anything.
 */
fun marathonfileWrites(args: List<String>) {
    val file = args.firstOrNull()?.let { File(it) } ?: usage()
    val envIndex = args.indexOf("--env-file")
    val env = if (envIndex >= 0) readEnvFile(File(args.getOrNull(envIndex + 1) ?: usage())) else emptyMap()
    val dir = file.canonicalFile.parentFile
    val values = mutableListOf<Map<String, Any?>>()
    var error: Throwable? = null
    try {
        val text = StringSubstitutor(StringLookupFactory.INSTANCE.functionStringLookup { k: String -> env[k] }).replace(file.readText())
        val tree: JsonNode? = marathonfileMapper(FixedTimeProvider(Instant.EPOCH), dir).readTree(text)
        for (key in listOf("outputDir", "application", "testApplication")) {
            tree?.findValues(key)?.forEach { v ->
                when {
                    v.isNull -> Unit
                    v.isValueNode -> values.add(linkedMapOf("key" to key, "value" to v.asText()))
                    else -> values.add(linkedMapOf("key" to key, "nonScalar" to true))
                }
            }
        }
    } catch (t: Throwable) {
        error = t
    }
    print(Json.pretty(linkedMapOf("marathonfileDir" to dir.path, "values" to values, "error" to error?.let { errorJson(it) })))
}

private fun usage(): Nothing {
    System.err.println("usage: marathonfile-writes <Marathonfile> [--env-file F]")
    exitProcess(2)
}
