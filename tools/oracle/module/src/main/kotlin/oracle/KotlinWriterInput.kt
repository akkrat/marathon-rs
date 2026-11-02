package oracle

import kotlin.reflect.full.memberProperties
import kotlin.reflect.full.primaryConstructor

/** Inputs describe structure before rendering. Explicit overrides stay scalar,
 * so this does not misrepresent secret masking as a generated data-class string. */
fun kotlinWriterInput(value: Any?): Map<String, Any?> {
    fun text(s: String) = mapOf("kind" to "text", "units" to s.map { it.code })
    return when (value) {
        null -> mapOf("kind" to "null")
        is Pair<*, *> -> mapOf("kind" to "tuple", "items" to listOf(kotlinWriterInput(value.first), kotlinWriterInput(value.second)))
        is Triple<*, *, *> -> mapOf("kind" to "tuple", "items" to listOf(kotlinWriterInput(value.first), kotlinWriterInput(value.second), kotlinWriterInput(value.third)))
        is Map<*, *> -> mapOf("kind" to "map", "entries" to value.entries.map { listOf(kotlinWriterInput(it.key), kotlinWriterInput(it.value)) })
        is Collection<*> -> mapOf("kind" to "list", "items" to value.map(::kotlinWriterInput))
        else -> {
            val klass = value::class
            if (klass.isData && value !is com.malinskiy.marathon.config.AnalyticsConfiguration.InfluxDb2Configuration) {
                val properties = klass.memberProperties.associateBy { it.name }
                mapOf("kind" to "data", "name" to klass.simpleName, "fields" to klass.primaryConstructor!!.parameters.map { parameter ->
                    listOf(parameter.name, kotlinWriterInput(properties[parameter.name]!!.getter.call(value)))
                })
            } else text(value.toString().replace(Regex("@[0-9a-fA-F]+"), "@<hash>"))
        }
    }
}
