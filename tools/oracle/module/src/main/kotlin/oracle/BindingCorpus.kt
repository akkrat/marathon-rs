package oracle

import java.io.File
import java.time.Instant

// Explicit Kotlin creator witnesses; the earlier YAML corpora also use data classes.
data class RequiredString(val k: String)
data class RequiredList(val k: List<String>)
data class RequiredMap(val k: Map<String, String>)
data class RequiredInt(val k: Int)
data class RegexField(val k: Regex?)

object BindingCorpus {
    fun generate(out: File) {
        val mapper = marathonfileMapper(FixedTimeProvider(Instant.parse("2026-10-12T00:00:00Z")), File("/"))
        val targets = listOf("RequiredString" to RequiredString::class.java, "RequiredList" to RequiredList::class.java,
            "RequiredMap" to RequiredMap::class.java, "RequiredInt" to RequiredInt::class.java, "Regex" to RegexField::class.java)
        val rows = mutableListOf<Map<String, Any?>>()
        for (input in listOf("{}", "k: null", "k: []", "k: {}", "k: '[abc'", "k: '[a-z]+'")) {
            for ((name, type) in targets) {
                val result = try { mapOf("ok" to Canonical.render(mapper.readValue(input, type))) }
                catch (t: Throwable) {
                    val chain = generateSequence(t) { it.cause }.map { it.javaClass.name }.toList()
                    mapOf("error" to chain)
                }
                rows.add(mapOf("input" to input, "target" to name, "result" to result))
            }
        }
        for (input in listOf("k: 017", "k: null\nk: x", "k: 0.99999999999999999", "k: {a: 017, a: yes}")) {
            for ((name, type) in listOf("TreeString" to TString::class.java, "TreeDouble" to TDouble::class.java, "TreeMap" to TMap::class.java)) {
                val result = try { mapOf("ok" to Canonical.render(mapper.treeToValue(mapper.readTree(input), type))) }
                catch (t: Throwable) { mapOf("error" to generateSequence(t) { it.cause }.map { it.javaClass.name }.toList()) }
                rows.add(mapOf("input" to input, "target" to name, "result" to result))
            }
        }
        out.writeText((listOf(mapOf("description" to "WP 2.1 Kotlin creators and invalid Regex class chain")) + rows).joinToString("\n") { Json.compact(it) } + "\n")
    }
}
