import oracle.JsonlTable
import oracle.kotlinWriterInput

data class WriterProbe(val text: String?, val nested: Any?)
fun main(args: Array<String>) {
    val dir = java.io.File(args.single())
    oracle.JcompatTables2.generateWriterModels(dir)
    val table = JsonlTable("kotlin_writer_edges", "Kotlin UTF-16 and nested data-class strings", mapOf("input" to "KotlinValue tree", "expected" to "UTF-16 units"))
    for ((i, text) in listOf(null, "", "\uD800", "\uDC00", "\uD800x", "😀", "a\nb, c=(d)", "\u0000\u2028").withIndex()) {
        val value = WriterProbe(text, listOf(mapOf("a" to text, "null" to null), Pair(text, 'x'), Triple(1.0f, 1e7, -0.0)))
        table.add(mapOf("id" to "edge-$i", "input" to kotlinWriterInput(value), "expected" to value.toString().map { it.code }))
    }
    table.writeTo(dir)
}
