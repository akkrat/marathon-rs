package oracle

import java.io.File
import java.math.BigInteger

/**
 * A small JSON writer with fully predictable output, so that the generated tables are byte-stable across runs.
 *
 * Values: null, Boolean, Int/Long/Short/Byte/BigInteger, [JRaw] (a number literal written as is), String, List, Array and
 * Map (written in its iteration order; callers that need sorted keys pass a sorted map).
 *
 * Strings are written as UTF-8 except for `"`, `\`, control characters and unpaired surrogates, which are escaped.
 */
object Json {
    fun compact(value: Any?): String = StringBuilder().also { write(it, value, null, 0) }.toString()

    fun pretty(value: Any?): String = StringBuilder().also { write(it, value, "  ", 0); it.append('\n') }.toString()

    private fun write(sb: StringBuilder, value: Any?, indent: String?, depth: Int) {
        when (value) {
            null -> sb.append("null")
            is Boolean -> sb.append(value.toString())
            is Int, is Long, is Short, is Byte, is BigInteger -> sb.append(value.toString())
            is JRaw -> sb.append(value.text)
            is Double, is Float -> throw IllegalArgumentException("floating point values must be wrapped in JRaw or a string: $value")
            is String -> string(sb, value)
            is Map<*, *> -> container(sb, '{', '}', value.entries.toList(), indent, depth) { e ->
                string(sb, e.key as String)
                sb.append(if (indent != null) ": " else ":")
                write(sb, e.value, indent, depth + 1)
            }
            is Collection<*> -> container(sb, '[', ']', value.toList(), indent, depth) { write(sb, it, indent, depth + 1) }
            is Array<*> -> container(sb, '[', ']', value.toList(), indent, depth) { write(sb, it, indent, depth + 1) }
            else -> throw IllegalArgumentException("not a JSON value: ${value.javaClass.name}")
        }
    }

    private fun <T> container(
        sb: StringBuilder, open: Char, close: Char, items: List<T>, indent: String?, depth: Int, item: (T) -> Unit
    ) {
        sb.append(open)
        if (items.isEmpty()) {
            sb.append(close)
            return
        }
        items.forEachIndexed { i, it ->
            if (i > 0) sb.append(',')
            if (indent != null) {
                sb.append('\n')
                repeat(depth + 1) { sb.append(indent) }
            }
            item(it)
        }
        if (indent != null) {
            sb.append('\n')
            repeat(depth) { sb.append(indent) }
        }
        sb.append(close)
    }

    fun string(sb: StringBuilder, s: String) {
        sb.append('"')
        var i = 0
        while (i < s.length) {
            val c = s[i]
            when {
                c == '"' -> sb.append("\\\"")
                c == '\\' -> sb.append("\\\\")
                c == '\n' -> sb.append("\\n")
                c == '\r' -> sb.append("\\r")
                c == '\t' -> sb.append("\\t")
                c == '\b' -> sb.append("\\b")
                c == '\u000c' -> sb.append("\\f")
                c < ' ' || c == '\u007f' -> sb.append(String.format("\\u%04x", c.code))
                Character.isHighSurrogate(c) && i + 1 < s.length && Character.isLowSurrogate(s[i + 1]) -> {
                    sb.append(c).append(s[i + 1])
                    i++
                }
                Character.isSurrogate(c) -> sb.append(String.format("\\u%04x", c.code))
                else -> sb.append(c)
            }
            i++
        }
        sb.append('"')
    }
}

/** A JSON number literal written verbatim, for example the output of `Double.toString`. */
data class JRaw(val text: String)

/** Renders a double as a JSON number when it is finite (Java spelling, e.g. `1.0E-5`), and as a string otherwise. */
fun jsonDouble(d: Double): Any = if (d.isFinite()) JRaw(d.toString()) else d.toString()

/** A JSONL table: a header line describing the schema, then one object per row. */
class JsonlTable(val name: String, private val description: String, private val schema: Map<String, String>) {
    val rows = mutableListOf<Map<String, Any?>>()

    fun add(row: Map<String, Any?>) {
        rows.add(row)
    }

    fun writeTo(dir: File): File {
        dir.mkdirs()
        val file = File(dir, "$name.jsonl")
        val header = linkedMapOf<String, Any?>(
            "table" to name,
            "description" to description,
            "schema" to schema,
            "rows" to rows.size,
            "jdk" to "${System.getProperty("java.vendor")} ${System.getProperty("java.runtime.version")}",
            "generator" to "tools/oracle/module (run tools/oracle/jcompat-tables or the corpus wrappers to regenerate)",
        )
        file.bufferedWriter().use { w ->
            w.write(Json.compact(header))
            w.write("\n")
            rows.forEach {
                w.write(Json.compact(it))
                w.write("\n")
            }
        }
        System.err.println("wrote ${file.path}: ${rows.size} rows")
        return file
    }
}

/** `class: message` of a throwable and its causes, outermost first. */
fun errorJson(t: Throwable): Map<String, Any?> {
    val causes = mutableListOf<String>()
    var c = t.cause
    val seen = mutableSetOf<Throwable>()
    while (c != null && seen.add(c)) {
        causes.add("${c.javaClass.name}: ${c.message}")
        c = c.cause
    }
    return linkedMapOf("class" to t.javaClass.name, "message" to t.message, "causes" to causes)
}
