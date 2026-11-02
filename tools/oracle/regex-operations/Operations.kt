// Kotlin operation probes only; pattern flavour is deliberately shared with Rust.
fun json(value: Any?): String = when (value) {
    null -> "null"
    is String -> "\"" + value.flatMap { c -> when(c) {
        '\\' -> "\\\\".toList(); '"' -> "\\\"".toList()
        else -> if (c.code < 32) "\\u%04x".format(c.code).toList() else listOf(c)
    } }.joinToString("") + "\""
    is Map<*, *> -> value.entries.joinToString(",", "{", "}") { json(it.key) + ":" + json(it.value) }
    is Iterable<*> -> value.joinToString(",", "[", "]") { json(it) }
    else -> value.toString()
}
fun main() {
    val rows = mutableListOf<Map<String, Any?>>()
    fun probe(pattern: String, input: String, op: String, limit: Int = 0, replacement: String = "-") {
        val regex = Regex(pattern)
        val expected = try {
            when(op) {
                "split" -> regex.split(input, limit)
                "replace" -> regex.replace(input, replacement)
                else -> regex.findAll(input).map { mapOf("range" to listOf(it.range.first, it.range.last + 1), "groups" to it.groups.map { it?.value }) }.toList()
            }
        } catch (e: Exception) { mapOf("error" to e.javaClass.simpleName) }
        rows.add(mapOf("pattern" to pattern, "input" to input, "op" to op, "limit" to limit, "replacement" to replacement, "expected" to expected))
    }
    for (pattern in listOf("", "x*", "a*")) for (input in listOf("", "x", "ab")) {
        probe(pattern, input, "findAll")
        probe(pattern, input, "replace")
        for (limit in listOf(-1, 0, 1, 2, 3)) probe(pattern, input, "split", limit)
    }
    for (replacement in listOf("\${word}:\$2:\$0:\$12:\\$\\\\", "\$", "\$3", "\${missing}", "\\", "\$word", "\$\$")) {
        for (input in listOf("a ab", "z")) probe("(?<word>a)(b)?", input, "replace", replacement = replacement)
    }
    println(json(mapOf("table" to "regex-operations", "rows" to rows.size, "generator" to "tools/oracle/regex-operations/run", "jdk" to System.getProperty("java.version"))))
    rows.forEach { println(json(it)) }
}
