package oracle

import java.io.File
import java.math.BigDecimal
import java.time.Duration
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** WP 1.2 review witnesses; expected outcomes come exclusively from the JDK. */
object FormattingRound1 {
    fun generate(dir: File) {
        val table = JsonlTable("formatting_round1", "WP 1.2 Graphite range, supplementary digits and checked time construction",
            linkedMapOf("op" to "graphite | digit | decimal | instantOf | durationOf",
                "input" to "text or colon-separated integers", "output" to "JDK result or null on rejection"))
        fun add(op: String, input: String, action: () -> Any?) {
            val output = try { action() } catch (_: RuntimeException) { null }
            table.add(linkedMapOf("op" to op, "input" to input, "output" to output))
        }
        val graphite = DateTimeFormatter.ofPattern("HH:mm_yyyyMMdd").withZone(ZoneOffset.UTC)
        for (year in listOf(-1000000000, -999999999, -300000000, -10000, -9999, -1, 0, 1, 9999, 10000, 300000000, 999999999, 1000000000)) {
            val y = if (year < 0) "-" + (-year.toLong()).toString().padStart(4, '0') else if (year > 9999) "+$year" else year.toString().padStart(4, '0')
            for (suffix in listOf("-01-01T00:00:00Z", "-12-31T23:59:59.999999999Z")) {
                val instant = Instant.parse(y + suffix)
                for (delta in listOf(-1L, 0L, 1L)) {
                    val value = try { instant.plusSeconds(delta) } catch (_: RuntimeException) { continue }
                    add("graphite", value.toString()) { graphite.format(value) }
                }
            }
        }
        val zeroes = listOf(0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950, 0x1fbf0)
        for (zero in zeroes) {
            for (code in zero - 1..zero + 10) {
                for (radix in listOf(1, 2, 10, 36, 37)) {
                    add("digit", "$code:$radix") { Character.digit(code, radix).takeIf { it >= 0 } }
                }
            }
            val digits = String(Character.toChars(zero)) + String(Character.toChars(zero + 1))
            for (input in listOf(digits, "1.$digits", "1e$digits")) {
                add("decimal", input) { BigDecimal(input).scale() }
            }
        }
        for (input in listOf("٠١", "１.２", "1e١", "𝟎𝟏")) {
            add("decimal", input) { BigDecimal(input).scale() }
        }
        for (seconds in listOf(Long.MIN_VALUE, Instant.MIN.epochSecond, 0L, Instant.MAX.epochSecond, Long.MAX_VALUE)) {
            for (nanos in listOf(Long.MIN_VALUE, -1L, 0L, 999999999L, 1000000000L, Long.MAX_VALUE)) {
                add("instantOf", "$seconds:$nanos") { Instant.ofEpochSecond(seconds, nanos).let { listOf(it.epochSecond, it.nano, it.toString()) } }
                add("durationOf", "$seconds:$nanos") { Duration.ofSeconds(seconds, nanos).let { listOf(it.seconds, it.nano, it.toString()) } }
            }
        }
        table.writeTo(dir)
    }
}
