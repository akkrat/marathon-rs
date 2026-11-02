package oracle

import java.io.File
import java.math.BigDecimal
import java.time.Instant
import java.time.Duration
import java.util.Locale
import java.util.Random

/** WP 1.2: additional generated boundaries and seeded numeric witnesses. */
object FormattingHardening {
    fun generate(dir: File) {
        val table = JsonlTable("formatting_hardening", "WP 1.2 seeded number formatting and parsing boundaries",
            linkedMapOf("op" to "double | float | fixed | parseDouble | decimal | instant | duration | digit",
                "input" to "raw bits hex or text", "output" to "string, decimal scale, or null for rejected input"))
        fun add(op: String, input: String, action: () -> Any?) {
            val output = try { action() } catch (_: RuntimeException) { null }
            table.add(linkedMapOf("op" to op, "input" to input, "output" to output))
        }
        val random = Random(1200261023L)
        repeat(4096) {
            val bits = random.nextLong()
            val input = "%016x".format(Locale.ROOT, bits)
            val value = java.lang.Double.longBitsToDouble(bits)
            add("double", input) { value.toString() }
            add("fixed", input) { String.format(Locale.ROOT, "%.4f", value) }
            val fbits = random.nextInt()
            add("float", "%08x".format(Locale.ROOT, fbits)) { java.lang.Float.intBitsToFloat(fbits).toString() }
        }
        val exponents = listOf(Long.MIN_VALUE.toString(), "-2147483649", "-2147483648", "-2147483647", "2147483647", "2147483648", "2147483649", Long.MAX_VALUE.toString(), "9223372036854775808", "000000000000000000000001")
        for (exp in exponents) {
            for (m in listOf("0", "1", "-1", "1.0", "0.001")) {
                add("decimal", "${m}e$exp") { BigDecimal("${m}e$exp").scale() }
                add("parseDouble", "${m}e$exp") { "%016x".format(Locale.ROOT, java.lang.Double.doubleToRawLongBits(java.lang.Double.parseDouble("${m}e$exp"))) }
            }
            for (m in listOf("1", "0.0001", "fffffffffffffffff", "0")) {
                val text = "0x${m}p$exp"
                add("parseDouble", text) { "%016x".format(Locale.ROOT, java.lang.Double.doubleToRawLongBits(java.lang.Double.parseDouble(text))) }
            }
        }
        for (text in listOf("-0000-01-01T00:00:00Z", "+0000-01-01T00:00:00Z", "-1000000000-01-01T00:00:00Z", "+1000000000-12-31T23:59:59.999999999Z", "2026-01-01T00:00:00+18:00", "2026-01-01T00:00:00+18:00:01", "2026-01-01T00:00:00.1234567890Z")) {
            add("instant", text) { Instant.parse(text).toString() }
        }
        for (text in listOf("PT-9223372036854775808S", "-PT-9223372036854775808S", "PT9223372036854775807.999999999S", "PT-9223372036854775808.1S", "PT9223372036854775807S1S", "PT+0.1S")) {
            add("duration", text) { Duration.parse(text).toString() }
        }
        for (text in listOf("0x1p-1075", "0x1.0000000000001p-1075", "0x1.fffffffffffff8p1023", "0x1p1024", "-0x0p999999999999999999999", " 1.5d ", "NaNd", "١.٥", "0x１p1")) {
            add("parseDouble", text) { "%016x".format(Locale.ROOT, java.lang.Double.doubleToRawLongBits(java.lang.Double.parseDouble(text))) }
        }
        for (code in listOf(0x30, 0x39, 0x41, 0x61, 0xff21, 0xff19, 0x0661, 0xd835)) {
            for (radix in listOf(0, 1, 2, 10, 36, 37)) {
                add("digit", "$code:$radix") { Character.digit(code.toChar(), radix).takeIf { it >= 0 } }
            }
        }
        table.writeTo(dir)
    }
}
