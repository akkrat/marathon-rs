package oracle

import java.io.File
import org.apache.commons.text.StringSubstitutor
import org.apache.commons.text.lookup.StringLookupFactory

/** Supplementary default-setting boundary probes for WP 1.3. */
object StringSubstitutorEdges {
    fun generate(dir: File) {
        val table = JsonlTable(
            "string_substitutor_edges",
            "commons-text 1.12.0 default StringSubstitutor: prefix/suffix boundaries, Unicode, escaped nesting and cycle diagnostics.",
            linkedMapOf("env" to "lookup map", "input" to "input text", "result" to "{ok} or {error: {class, message}}"),
        )
        val envs = listOf(
            emptyMap(),
            mapOf("V" to "value", "W" to "w", "" to "empty-name", " V " to "spaces", "😀" to "unicode"),
            mapOf("V" to "\$", "W" to "{V}", "A" to "\${V}\${W}"),
            mapOf("V" to "\${W", "W" to "w", "A" to "\${V}}"),
            mapOf("V" to "\${A}", "A" to "\${B}", "B" to "\${A}"),
            mapOf("V" to "\$\${A}", "A" to "\${A}"),
            mapOf("V" to "\${W:-\${A}}", "A" to "a"),
            mapOf("V" to "\${V:-d}", "A" to "\${MISSING}"),
        )
        val inputs = listOf(
            "", "}", "{V}", "\$\${", "\$\${V", "\$\${V}", "\$\${\${V}}", "\$\${V:-\${W}}",
            "\${V}}", "\${{V}}", "\${\${V}:-d}", "\${V:-\$\${W}}", "\${V:-\${W}}",
            "\${V:-\${W:-\${A}}}", "\${V:-a}b}", "\${V}\${W}", "\${V}{W}", "\${V}A}",
            "\${V}\$\${A}", "\$\${W} \${V}", "\${MISSING} \$\${W} \${V}", "\${A} \${V}",
            "\${}", "\${:-d}", "\${ V }", "\${😀}", "😀\$\${W} \${V}😀", "\${V:-😀}",
            "\${V\u0000}", "\${V:-a:-b}", "\${V:-\${A}}\${A}",
        )
        for (env in envs) for (input in inputs) {
            val lookup = StringSubstitutor(StringLookupFactory.INSTANCE.functionStringLookup { key: String -> env[key] })
            val result = try {
                mapOf("ok" to lookup.replace(input))
            } catch (e: Exception) {
                mapOf("error" to mapOf("class" to e.javaClass.name, "message" to e.message))
            }
            table.add(linkedMapOf("env" to env, "input" to input, "result" to result))
        }
        table.writeTo(dir)
    }
}
