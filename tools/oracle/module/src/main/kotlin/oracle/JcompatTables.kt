package oracle

import com.fasterxml.jackson.databind.ObjectMapper
import com.google.gson.Gson
import com.google.gson.GsonBuilder
import com.malinskiy.marathon.device.DeviceFeature
import com.malinskiy.marathon.device.DeviceInfo
import com.malinskiy.marathon.device.DevicePoolId
import com.malinskiy.marathon.device.NetworkState
import com.malinskiy.marathon.device.OperatingSystem
import com.malinskiy.marathon.execution.Attachment
import com.malinskiy.marathon.execution.AttachmentType
import com.malinskiy.marathon.execution.TestResult
import com.malinskiy.marathon.execution.TestStatus
import com.malinskiy.marathon.execution.command.parse.MarathonTestParseCommand
import com.malinskiy.marathon.execution.command.parse.ParseCommandResult
import com.malinskiy.marathon.json.FileSerializer
import com.malinskiy.marathon.report.bill.DeviceBill
import com.malinskiy.marathon.report.raw.RawJsonReporter
import com.malinskiy.marathon.test.MetaProperty
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.toTestName
import org.apache.commons.lang3.time.DurationFormatUtils
import org.apache.commons.text.StringSubstitutor
import org.apache.commons.text.lookup.StringLookupFactory
import java.io.File
import java.math.BigInteger
import java.security.MessageDigest
import java.text.SimpleDateFormat
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Date
import java.util.Locale
import java.util.PriorityQueue
import java.util.Random
import java.util.TimeZone

/** The jcompat tables of PLAN.md §4.3, one JSONL file each (`oracle jcompat-tables <dir>`). */
object JcompatTables {
    private const val SEED = 20261012L

    /** The locales a `String.format`/`SimpleDateFormat` call with the default locale is recorded under. */
    private val LOCALES = listOf(Locale.ROOT, Locale.US, Locale.forLanguageTag("ru-RU"), Locale.GERMANY)

    fun generate(dir: File) {
        priorityQueue().writeTo(dir)
        doubleToString().writeTo(dir)
        stringFormat().writeTo(dir)
        instantDuration().writeTo(dir)
        dateFormat().writeTo(dir)
        durationFormatUtils().writeTo(dir)
        stringSubstitutor().writeTo(dir)
        fragmentHash().writeTo(dir)
        gson().writeTo(dir)
        jacksonYaml().writeTo(dir)
        JcompatTables2.generate(dir)
        FormattingHardening.generate(dir)
    }

    private fun hex64(d: Double) = "%016x".format(java.lang.Double.doubleToRawLongBits(d))
    private fun hex32(f: Float) = "%08x".format(java.lang.Float.floatToRawIntBits(f))

    private fun attempt(block: () -> Any?): Map<String, Any?> = try {
        mapOf("ok" to block())
    } catch (t: Throwable) {
        mapOf("error" to linkedMapOf("class" to t.javaClass.name, "message" to t.message))
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // PriorityQueue

    /** Equality by [eq] only, as marathon's Test equals by name; [id] tells copies apart; the comparator looks at [key]. */
    private class Item(val id: Int, val key: Int, val eq: Int) {
        override fun equals(other: Any?) = other is Item && other.eq == eq
        override fun hashCode() = eq
        fun json() = listOf(id, key, eq)
    }

    private fun priorityQueue(): JsonlTable {
        val table = JsonlTable(
            "priority_queue",
            "java.util.PriorityQueue(comparator) as QueueActor uses it. Elements are [id, key, eq]: the comparator compares " +
                "key (Integer.compare), equals/hashCode use eq only (marathon's Test equals by name, so a queue holds equal " +
                "copies), id tells copies apart. Each step lists the comparator calls it made and the backing array after it.",
            linkedMapOf(
                "id" to "scenario id",
                "family" to "random | equal-keys | few-keys | duplicates",
                "steps" to "list of {op, args, result, cmp, heap}",
                "steps[].op" to "offer | poll | peek | remove | removeAll | addAll | iterator | clear",
                "steps[].args" to "offer: [item]; addAll: [items]; remove: probe [id, key, eq] (matched by eq); removeAll: probes " +
                    "in a LinkedHashSet; others: []",
                "steps[].result" to "offer/addAll/remove/removeAll: boolean; poll/peek: id or null; iterator: ids in order",
                "steps[].cmp" to "comparator calls during the step as [left id, right id, sign of the result]",
                "steps[].heap" to "ids in the backing array order (= iteration order) after the step",
            ),
        )
        val rnd = Random(SEED)
        val families = listOf("random", "equal-keys", "few-keys", "duplicates")
        for (n in 0 until 240) {
            val family = families[n % families.size]
            var nextId = 0
            val trace = mutableListOf<List<Int>>()
            val queue = PriorityQueue<Item> { a, b ->
                val r = Integer.compare(a.key, b.key)
                trace.add(listOf(a.id, b.id, Integer.signum(r)))
                r
            }
            fun newItem(): Item {
                val key = when (family) {
                    "random" -> rnd.nextInt(1000)
                    "equal-keys" -> 5
                    "few-keys" -> rnd.nextInt(3)
                    else -> rnd.nextInt(6)
                }
                val id = nextId++
                val eq = if (family == "duplicates") rnd.nextInt(5) else id
                return Item(id, key, eq)
            }
            fun probe(): Item {
                val live = queue.toList()
                val eq = if (live.isNotEmpty() && rnd.nextInt(4) != 0) live[rnd.nextInt(live.size)].eq else 1000 + rnd.nextInt(10)
                return Item(-1 - rnd.nextInt(1000), -1, eq)
            }
            val steps = mutableListOf<Map<String, Any?>>()
            val stepCount = 5 + rnd.nextInt(40)
            for (s in 0 until stepCount) {
                trace.clear()
                val roll = rnd.nextInt(100)
                val (op, args, result) = when {
                    roll < 35 -> newItem().let { Triple("offer", listOf(it.json()), queue.offer(it)) }
                    roll < 55 -> Triple("poll", emptyList<Any>(), queue.poll()?.id)
                    roll < 60 -> Triple("peek", emptyList<Any>(), queue.peek()?.id)
                    roll < 70 -> probe().let { Triple("remove", listOf(it.json()), queue.remove(it)) }
                    roll < 78 -> {
                        val probes = LinkedHashSet<Item>().apply { repeat(1 + rnd.nextInt(3)) { add(probe()) } }
                        Triple("removeAll", probes.map { it.json() }, queue.removeAll(probes))
                    }
                    roll < 88 -> {
                        val items = List(rnd.nextInt(6)) { newItem() }
                        Triple("addAll", items.map { it.json() }, queue.addAll(items))
                    }
                    roll < 98 -> Triple("iterator", emptyList<Any>(), queue.iterator().asSequence().map { it.id }.toList())
                    else -> {
                        queue.clear()
                        Triple("clear", emptyList<Any>(), null)
                    }
                }
                steps.add(
                    linkedMapOf(
                        "op" to op,
                        "args" to args,
                        "result" to result,
                        "cmp" to trace.toList(),
                        "heap" to queue.toArray().map { (it as Item).id },
                    )
                )
            }
            table.add(linkedMapOf("id" to "pq-%03d".format(n), "family" to family, "steps" to steps))
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // Double.toString

    private fun doubleToString(): JsonlTable {
        val table = JsonlTable(
            "double_to_string",
            "Double.toString and Float.toString (JDK 19+ shortest-repr algorithm). Kotlin string templates and " +
                "Any.toString() of a Double use the same method.",
            linkedMapOf(
                "kind" to "double | float",
                "bits" to "IEEE 754 raw bits, hex (16 digits for double, 8 for float)",
                "category" to "why the value is in the table",
                "toString" to "the JDK result",
            ),
        )
        val seen = mutableSetOf<String>()
        fun d(category: String, v: Double) {
            if (seen.add("d" + hex64(v))) {
                table.add(linkedMapOf("kind" to "double", "bits" to hex64(v), "category" to category, "toString" to v.toString()))
            }
        }
        fun f(category: String, v: Float) {
            if (seen.add("f" + hex32(v))) {
                table.add(linkedMapOf("kind" to "float", "bits" to hex32(v), "category" to category, "toString" to v.toString()))
            }
        }
        listOf(
            0.0, -0.0, Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, -Double.MIN_VALUE,
            Double.MAX_VALUE, java.lang.Double.MIN_NORMAL, 1.0, -1.0, 0.1, 0.2, 0.3, 0.1 + 0.2, 1.0 / 3, 2.0 / 3, 0.5, 100.0, 80.0,
            0.7, 2.0E-3, 5.0E-324, 4.9E-324, 1.7976931348623157E308, 2.2250738585072014E-308, 2.225073858507201E-308,
            123456789012345678.0, 9007199254740992.0, 9007199254740993.0, 1.0E23, 8.41E21, 2.0E-7, 9.9E-324,
        ).forEach { d("edge", it) }
        for (e in -30..30) {
            val v = "1e$e".toDouble()
            d("power-of-ten", v)
            d("power-of-ten-next-up", Math.nextUp(v))
            d("power-of-ten-next-down", Math.nextDown(v))
        }
        for (e in -60..60) d("power-of-two", Math.scalb(1.0, e))
        // Double.toString switches to computerized scientific notation outside [1e-3, 1e7).
        for (base in listOf(1.0E-3, 1.0E7)) {
            var up = base
            var down = base
            repeat(6) {
                d("threshold", up)
                d("threshold", down)
                up = Math.nextUp(up)
                down = Math.nextDown(down)
            }
            listOf(0.5, 0.9, 0.99, 0.999999, 1.000001, 1.5, 9.99).forEach { d("threshold", base * it) }
        }
        listOf(0.001, 0.0010000000000000002, 9.999999999999998E-4, 9999999.0, 9999999.999999998, 1.0E7, 10000000.5, 1.0E-3)
            .forEach { d("threshold", it) }
        val rnd = Random(SEED)
        repeat(300) { d("random-bits", java.lang.Double.longBitsToDouble(rnd.nextLong())) }
        repeat(150) { d("random-unit", rnd.nextDouble()) }
        repeat(150) { d("random-scaled", rnd.nextDouble() * Math.pow(10.0, (rnd.nextInt(41) - 20).toDouble())) }
        repeat(100) { d("random-ms-ratio", rnd.nextInt(10_000_000) / 1000.0) }
        listOf(0f, -0f, Float.NaN, Float.POSITIVE_INFINITY, Float.MIN_VALUE, Float.MAX_VALUE, 1f, 0.1f, 0.7f, 100f, 1.0E-3f, 1.0E7f, 33.333332f)
            .forEach { f("edge", it) }
        repeat(100) { f("random-bits", java.lang.Float.intBitsToFloat(rnd.nextInt())) }
        repeat(50) { f("random-unit", rnd.nextFloat()) }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // String.format

    private fun stringFormat(): JsonlTable {
        val table = JsonlTable(
            "string_format",
            "String.format (Kotlin String.format/format extension, default locale) for the patterns marathon uses: " +
                "\"%.2f\" and \"%.4f\" (core FileTransferExtensions) and \"%02d%%\" (PoolProgressAccumulator.toPercent). " +
                "Each call is recorded under several default locales.",
            linkedMapOf(
                "pattern" to "format string",
                "locale" to "BCP 47 tag of the locale the format ran under (\"und\" = Locale.ROOT)",
                "arg" to "{double: raw bits hex} or {int: value}",
                "output" to "the formatted string, or {error}",
            ),
        )
        val rnd = Random(SEED)
        val doubles = mutableListOf(
            0.0, -0.0, 0.5, 1.5, 2.5, -0.5, -1.5, 0.125, 0.135, 0.145, 1.005, 2.675, 1.115, 0.00005, 0.00015, 0.000049999,
            1.0E-5, 1.0E20, 1.0E-20, 123456789.987654321, 1.0E7, 9999.995, 9999.994999, Double.NaN, Double.POSITIVE_INFINITY,
            Double.NEGATIVE_INFINITY, Double.MAX_VALUE, Double.MIN_VALUE, 0.1 + 0.2, 1.0 / 3, 2.0 / 3, 1234.5, 1234.56789,
        )
        repeat(60) { doubles.add(rnd.nextInt(5_000_000) / 1000.0 / (1 + rnd.nextInt(9999) / 1000.0)) }
        repeat(20) { doubles.add(rnd.nextDouble() * 1000) }
        val ints = listOf(0, 1, 5, 9, 10, 42, 99, 100, 101, 1000, -1, -5, -10, Int.MAX_VALUE, Int.MIN_VALUE)
        for (locale in LOCALES) {
            for (pattern in listOf("%.2f", "%.4f")) {
                doubles.forEach { v ->
                    table.add(
                        linkedMapOf(
                            "pattern" to pattern, "locale" to locale.toLanguageTag(), "arg" to mapOf("double" to hex64(v)),
                            "output" to String.format(locale, pattern, v),
                        )
                    )
                }
            }
            ints.forEach { v ->
                table.add(
                    linkedMapOf(
                        "pattern" to "%02d%%", "locale" to locale.toLanguageTag(), "arg" to mapOf("int" to v),
                        "output" to String.format(locale, "%02d%%", v),
                    )
                )
            }
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // Instant and Duration

    private fun instantDuration(): JsonlTable {
        val table = JsonlTable(
            "instant_duration",
            "java.time.Instant and Duration toString/parse, including invalid inputs and their exceptions.",
            linkedMapOf(
                "op" to "Instant.toString | Instant.parse | Duration.toString | Duration.parse",
                "seconds" to "toString: epoch seconds (Instant) or seconds (Duration)",
                "nanos" to "toString: nano adjustment, 0..999999999",
                "input" to "parse: the input string",
                "result" to "toString: the string; parse: {ok: {seconds, nanos, toString}} or {error: {class, message}}",
            ),
        )
        val instantSeconds = listOf(
            0L, 1L, -1L, 59L, 1_000_000L, 1_000_000_000L, 1_700_000_000L, 1_760_227_200L, 253_402_300_799L, 253_402_300_800L,
            -62_135_596_800L, -62_135_596_801L, -62_167_219_200L, -62_167_219_201L, Instant.MAX.epochSecond, Instant.MIN.epochSecond,
        )
        val nanos = listOf(0, 1, 10, 1000, 100_000, 1_000_000, 10_000_000, 100_000_000, 123_456_789, 120_000_000, 999_999_999)
        for (s in instantSeconds) for (n in nanos) {
            table.add(linkedMapOf("op" to "Instant.toString", "seconds" to s, "nanos" to n, "result" to Instant.ofEpochSecond(s, n.toLong()).toString()))
        }
        listOf(
            "1970-01-01T00:00:00Z", "2026-10-12T00:00:00Z", "2026-10-12T12:34:56.789Z", "2026-10-12T12:34:56.123456789Z",
            "2026-10-12T12:34:56.1234567891Z", "2026-10-12T12:34:56.Z", "2026-10-12T12:34Z", "2026-10-12T12:34:56+03:00",
            "2026-10-12T12:34:56-00:30", "2026-10-12T12:34:56+0300", "2026-10-12T23:59:60Z", "2026-10-12T24:00:00Z",
            "2026-13-01T00:00:00Z", "2026-02-30T00:00:00Z", "2026-10-12t00:00:00z", "2026-10-12T00:00:00z", "2026-10-12",
            "2026-10-12T00:00:00", "+10000-01-01T00:00:00Z", "10000-01-01T00:00:00Z", "-0001-01-01T00:00:00Z", "0000-01-01T00:00:00Z",
            "1000000", "", " 2026-10-12T00:00:00Z", "2026-10-12T00:00:00Z ", "2026-10-12 00:00:00Z", "-1000000000-01-01T00:00:00Z",
            "+1000000000-12-31T23:59:59.999999999Z", "+1000000001-01-01T00:00:00Z", "2026-W41-1T00:00:00Z", "2026-285T00:00:00Z",
        ).forEach { input ->
            table.add(
                linkedMapOf("op" to "Instant.parse", "input" to input, "result" to attempt {
                    val i = Instant.parse(input)
                    linkedMapOf("seconds" to i.epochSecond, "nanos" to i.nano, "toString" to i.toString())
                })
            )
        }
        val durationSeconds = listOf(
            0L, 1L, 30L, 59L, 60L, 61L, 3599L, 3600L, 3661L, 86_399L, 86_400L, 90_061L, 131_405L, -1L, -30L, -3600L, -86_400L,
            Long.MAX_VALUE, Long.MIN_VALUE, 1_000_000_000_000L,
        )
        for (s in durationSeconds) for (n in listOf(0, 1, 1000, 1_000_000, 500_000_000, 999_999_999)) {
            table.add(linkedMapOf("op" to "Duration.toString", "seconds" to s, "nanos" to n, "result" to Duration.ofSeconds(s, n.toLong()).toString()))
        }
        listOf(
            "PT30S", "PT1H", "PT1M", "P1D", "P1DT12H30M5S", "-PT1H", "-P30D", "PT-1H", "PT0.5S", "PT1.123456789S", "PT1.1234567891S",
            "pt30s", "P2W", "PT", "P", "30", "PT1H30", "+PT1H", "PT1,5S", "PT-0.5S", "-PT-1H", "P1Y", "P1M", "PT1H-30M", "PT9223372036854775807S",
            "PT9223372036854775808S", "PT2562047788015215H", "PT2562047788015216H", "P106751991167300D", "P106751991167301D", "", " PT1S",
            "PT1S ", "PT1.S", "PT.5S", "-P1DT-1H", "PT10M0.000000001S", "PT0S", "-PT0S", "PT-0S",
        ).forEach { input ->
            table.add(
                linkedMapOf("op" to "Duration.parse", "input" to input, "result" to attempt {
                    val d = Duration.parse(input)
                    linkedMapOf("seconds" to d.seconds, "nanos" to d.nano, "toString" to d.toString())
                })
            )
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // SimpleDateFormat and DateTimeFormatter

    private fun dateFormat(): JsonlTable {
        val table = JsonlTable(
            "date_format",
            "The date formats marathon uses, in UTC: SimpleDateFormat(\"HH:mm:ss z, MMM d yyyy\") with the default locale " +
                "(HtmlSummaryReporter, recorded under several locales), SimpleDateFormat(\"yyyy-MM-dd'T'HH:mm:ss\", Locale.US) " +
                "(JUnitReporter) and DateTimeFormatter.ofPattern(\"HH:mm_yyyyMMdd\") with zone UTC (QueryableGraphiteClient, " +
                "default locale).",
            linkedMapOf(
                "formatter" to "SimpleDateFormat | DateTimeFormatter",
                "pattern" to "the pattern",
                "locale" to "BCP 47 tag of the locale",
                "epochMillis" to "the instant formatted",
                "output" to "the formatted string",
            ),
        )
        val millis = listOf(
            0L, 1L, 999L, 1_000_000_000L, 1_000_000_000_000L, 1_700_000_000_123L, 1_760_227_200_000L, 1_760_270_400_000L,
            1_767_225_599_999L, 1_767_225_600_000L, 951_782_400_000L, 4_102_444_800_000L, 253_402_300_799_999L, 253_402_300_800_000L,
            -1L, -86_400_000L, -62_135_596_800_000L, -62_135_596_800_001L,
        ) + (0 until 12).map { 1_735_689_600_000L + it * 2_678_400_000L + it * 3_723_000L }
        val utc = TimeZone.getTimeZone("UTC")
        for (locale in LOCALES) {
            val f = SimpleDateFormat("HH:mm:ss z, MMM d yyyy", locale).apply { timeZone = utc }
            millis.forEach {
                table.add(linkedMapOf("formatter" to "SimpleDateFormat", "pattern" to "HH:mm:ss z, MMM d yyyy", "locale" to locale.toLanguageTag(), "epochMillis" to it, "output" to f.format(Date(it))))
            }
        }
        val junit = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss", Locale.US).apply { timeZone = utc }
        millis.forEach {
            table.add(linkedMapOf("formatter" to "SimpleDateFormat", "pattern" to "yyyy-MM-dd'T'HH:mm:ss", "locale" to Locale.US.toLanguageTag(), "epochMillis" to it, "output" to junit.format(Date(it))))
        }
        for (locale in LOCALES) {
            val f = DateTimeFormatter.ofPattern("HH:mm_yyyyMMdd", locale).withZone(ZoneId.of("UTC"))
            millis.forEach {
                table.add(linkedMapOf("formatter" to "DateTimeFormatter", "pattern" to "HH:mm_yyyyMMdd", "locale" to locale.toLanguageTag(), "epochMillis" to it, "output" to f.format(Instant.ofEpochMilli(it))))
            }
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // DurationFormatUtils

    private fun durationFormatUtils(): JsonlTable {
        val table = JsonlTable(
            "duration_format_utils",
            "commons-lang3 DurationFormatUtils.formatDuration(ms, \"H'H' mm'm' ss's'\") and StdoutReporter.formatDuration, " +
                "which returns \"0s\" for ms <= 0.",
            linkedMapOf(
                "millis" to "input",
                "formatDuration" to "{ok: string} or {error: {class, message}}",
                "stdoutReporter" to "StdoutReporter's wrapper result",
            ),
        )
        val values = listOf(
            0L, 1L, 499L, 500L, 999L, 1000L, 1001L, 1499L, 1500L, 1999L, 59_000L, 59_499L, 59_500L, 59_999L, 60_000L, 61_000L,
            599_999L, 3_599_999L, 3_600_000L, 3_661_000L, 35_999_999L, 36_000_000L, 86_399_999L, 86_400_000L, 90_061_001L,
            359_999_999L, 360_000_000L, 1_000_000_000L, 100_000_000_000L, Long.MAX_VALUE, -1L, -1000L, Long.MIN_VALUE,
        )
        values.forEach { ms ->
            table.add(
                linkedMapOf(
                    "millis" to ms,
                    "formatDuration" to attempt { DurationFormatUtils.formatDuration(ms, "H'H' mm'm' ss's'") },
                    "stdoutReporter" to if (ms > 0) DurationFormatUtils.formatDuration(ms, "H'H' mm'm' ss's'") else "0s",
                )
            )
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // StringSubstitutor

    private fun stringSubstitutor(): JsonlTable {
        val table = JsonlTable(
            "string_substitutor",
            "commons-text 1.12 StringSubstitutor as ConfigurationFactory builds it: StringSubstitutor(environment variable " +
                "lookup), default settings. The environment is the row's env map: environmentVariableStringLookup is " +
                "FunctionStringLookup.on(System::getenv), so the oracle uses a FunctionStringLookup over the map instead.",
            linkedMapOf(
                "env" to "the environment (undefined variables are absent)",
                "input" to "the text substituted",
                "result" to "{ok: string} or {error: {class, message}}",
            ),
        )
        val envs = listOf(
            linkedMapOf("V" to "value"),
            linkedMapOf("V" to ""),
            linkedMapOf<String, String>(),
            linkedMapOf("A" to "\${B}", "B" to "b"),
            linkedMapOf("A" to "\${B}", "B" to "\${A}"),
            linkedMapOf("A" to "\${A}"),
            linkedMapOf("A_x" to "ax", "B" to "x", "A_" to "a_"),
            linkedMapOf("V" to "\$\${W}", "W" to "w"),
            linkedMapOf("V" to "a:-b", "W" to "w"),
            linkedMapOf("V" to "\${W:-d}"),
            linkedMapOf("V:-d" to "odd"),
            linkedMapOf("V" to "multi\nline: \"q\""),
            linkedMapOf("PATH_LIKE" to "/usr/bin:/bin", "V" to "\$"),
        )
        val inputs = listOf(
            "\${V}", "x\${V}y", "\${V:-d}", "\${V:-}", "\${V:-\${W}}", "\${V:-\${W:-e}}", "\$\${V}", "\$\$\${V}", "\$\$\$\${V}",
            "\$V", "\${", "\${V", "\${}", "\${:-d}", "\$", "\$\$", "\${ V }", "\${A}", "\${A_\${B}}", "\${UNDEFINED}",
            "\${UNDEFINED:-default}", "\${UNDEFINED:-}", "\${V}\${V}", "\${V}\${A}", "\$\${UNDEFINED}", "\${V:d}", "\${V-d}",
            "\${V:=d}", "\${V:?err}", "name: \"\${V}\"\noutputDir: \"\${UNDEFINED}\"", "\${PATH_LIKE}", "\${W}", "\${V:-d:-e}",
            "\${\${V}}", "\${V\n}", "\${env:V}",
        )
        for (env in envs) for (input in inputs) {
            val substitutor = StringSubstitutor(StringLookupFactory.INSTANCE.functionStringLookup { key: String -> env[key] })
            table.add(linkedMapOf("env" to env, "input" to input, "result" to attempt { substitutor.replace(input) }))
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // md5 fragmentation hash

    private fun fragmentHash(): JsonlTable {
        val table = JsonlTable(
            "fragment_hash",
            "FragmentationFilter: BigInteger(md5(toTestName() as UTF-8)) is signed; testRemainder = hash.rem(count), plus count " +
                "when negative; a test belongs to fragment `index` when that equals index.",
            linkedMapOf(
                "input" to "the test name (Test.toTestName(): pkg.clazz#method)",
                "md5" to "digest, hex",
                "signed" to "BigInteger(digest).toString()",
                "fragment" to "map count -> the fragment index of the test",
            ),
        )
        val names = mutableListOf(
            "", "#", "com.example.FooTest#testBar", "FooTest#testBar", "com.example.FooTest#testBar[0]", "com.example.FooTest#test bar",
            "пакет.Класс#метод", "com.example.ÜnïcødeTest#emoji😀", "a.b.c.D#e", "com.example.LongTest#" + "x".repeat(300),
        )
        val rnd = Random(SEED)
        // Without k/K: keeps random names from spelling words.
        val alphabet = "abcdefghijlmnopqrstuvwxyzABCDEFGHIJLMNOPQRSTUVWXYZ0123456789_$"
        repeat(240) {
            fun word(n: Int) = (1..n).map { alphabet[rnd.nextInt(alphabet.length)] }.joinToString("")
            val pkg = (1..rnd.nextInt(4)).joinToString(".") { word(1 + rnd.nextInt(8)).lowercase() }
            names.add(Test(pkg, word(1 + rnd.nextInt(20)), word(1 + rnd.nextInt(30)), emptyList()).toTestName())
        }
        val counts = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 16, 100, 1000)
        for (name in names) {
            val digest = MessageDigest.getInstance("MD5").digest(name.toByteArray(Charsets.UTF_8))
            val hash = BigInteger(digest)
            val fragments = linkedMapOf<String, Any?>()
            for (c in counts) {
                val power = BigInteger.valueOf(c.toLong())
                val r = hash % power
                fragments[c.toString()] = (if (r < BigInteger.ZERO) r + power else r).toInt()
            }
            table.add(linkedMapOf("input" to name, "md5" to digest.joinToString("") { "%02x".format(it) }, "signed" to hash.toString(), "fragment" to fragments))
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // Gson

    private fun marathonGson(): Gson = GsonBuilder().registerTypeAdapter(File::class.java, FileSerializer()).create()

    private const val CWD_PLACEHOLDER = "<CWD>"

    /** Replaces the working directory, which relative Files resolve against, by [CWD_PLACEHOLDER]. */
    private fun normaliseCwd(s: String): String {
        val cwd = File(System.getProperty("user.dir"))
        return listOf(cwd.absolutePath, cwd.canonicalPath).distinct().sortedByDescending { it.length }
            .fold(s) { acc, p -> acc.replace(p, CWD_PLACEHOLDER) }
    }

    private fun gson(): JsonlTable {
        val table = JsonlTable(
            "gson",
            "Gson 2.11 as core's Koin module configures it (GsonBuilder().registerTypeAdapter(File, FileSerializer()).create()) " +
                "over representative report models. DeviceInfo carries safeSerialNumber\$delegate (D3: the Rust side omits it).",
            linkedMapOf(
                "id" to "row id",
                "model" to "the Kotlin class serialized",
                "description" to "what the row covers",
                "json" to "{ok: gson.toJson(value)} or {error}. FileSerializer writes File.absolutePath, so a relative File " +
                    "resolves against the generator's working directory; that directory (as user.dir and as its canonical " +
                    "path) reads $CWD_PLACEHOLDER, which makes the table independent of the checkout path",
            ),
        )
        val gson = marathonGson()
        fun device(serial: String, features: Collection<DeviceFeature> = listOf(DeviceFeature.VIDEO, DeviceFeature.SCREENSHOT)) =
            DeviceInfo(OperatingSystem("17.4"), serial, "iPhone 15", "Apple", NetworkState.CONNECTED, features, true)
        val test = Test("com.example", "FooTest", "testBar", listOf(MetaProperty("org.junit.Test"), MetaProperty("Owner", mapOf("value" to "team <a&b>"))))
        val rows = mutableListOf<Triple<String, String, () -> Any?>>()
        rows += Triple("device-lazy-untouched", "DeviceInfo before safeSerialNumber is read") { device("emulator-5554") }
        rows += Triple("device-lazy-read", "DeviceInfo after safeSerialNumber is read") { device("127.0.0.1:5555").also { it.safeSerialNumber } }
        rows += Triple("device-no-features", "empty feature list, unhealthy") { DeviceInfo(OperatingSystem(""), "s", "m", "x", NetworkState.DISCONNECTED, emptyList(), false) }
        rows += Triple("bill", "DeviceBill") { DeviceBill(device("A1B2-C3"), DevicePoolId("omni"), 1_700_000_000_000L, 1_700_000_123_456L, 123_456L) }
        rows += Triple("test-result-passed", "TestResult, passed, no stacktrace") {
            TestResult(test, device("emulator-5554"), "batch-1", TestStatus.PASSED, 1_700_000_000_000L, 1_700_000_001_500L)
        }
        rows += Triple("test-result-failure", "TestResult with a stacktrace needing escapes and attachments") {
            TestResult(
                test, device("emulator-5554"), "batch-2", TestStatus.FAILURE, 0L, 0L,
                "java.lang.AssertionError: expected:<1> but was:<2>\n\tat Foo.bar(Foo.kt:12)\r\n'quoted' = \"double\"   \u0001 é ✓",
                listOf(Attachment(File("/tmp/out/screen.png"), AttachmentType.SCREENSHOT_PNG, "screen"), Attachment(File("relative/log.txt"), AttachmentType.LOG)),
            )
        }
        rows += Triple("raw-test-runs", "RawJsonReporter rows") {
            listOf(
                RawJsonReporter.RawTestRun("com.example", "FooTest", "testBar", listOf(RawJsonReporter.StringMetaProperty("Owner", mapOf("value" to "me"))), "emulator-5554", TestStatus.PASSED, false, true, 1_700_000_000_000L, 1500L),
                RawJsonReporter.RawTestRun("", "Bar", "baz", emptyList(), "s", TestStatus.ASSUMPTION_FAILURE, true, false, 0L, 0L),
            )
        }
        rows += Triple("meta-values", "MetaProperty values of several types; null map values") {
            MetaProperty("m", linkedMapOf("int" to 1, "long" to 3_000_000_000L, "double" to 1.0, "float" to 0.5f, "string" to "s", "null" to null, "list" to listOf(1, "a"), "bool" to true))
        }
        rows += Triple("meta-nan", "a NaN value (Gson rejects it)") { MetaProperty("m", mapOf("nan" to Double.NaN)) }
        rows += Triple("doubles", "Gson's double spelling") { listOf(0.1, 1.0E7, 1.0E-3, 1.0E-4, 123456789012.0, 1.0E21, -0.0, 100.0) }
        rows += Triple("strings", "HTML-escaped and non-ASCII strings") { listOf("<a href='x'>&amp;</a>", "=", "\u007f", "  ", "😀", "\uD800", "tab\tnl\n") }
        rows += Triple("file", "File through FileSerializer (absolute path)") { listOf(File("/abs/path"), File("rel/path")) }
        rows += Triple("test", "Test with meta properties (the parse model)") { test }
        rows += Triple("parse-result", "ParseCommandResult with null flakyTests") { ParseCommandResult(listOf(test), null) }
        rows += Triple("pool-id-map", "map keyed by DevicePoolId (Gson uses toString keys)") { mapOf(DevicePoolId("omni") to 1) }
        for ((id, description, value) in rows) {
            val v = try { value() } catch (t: Throwable) { t }
            table.add(
                linkedMapOf(
                    "id" to id,
                    "model" to if (v is Collection<*>) "List<${v.firstOrNull()?.javaClass?.name}>" else v?.javaClass?.name,
                    "description" to description,
                    "json" to attempt { normaliseCwd(gson.toJson(v)) },
                )
            )
        }
        return table
    }

    // ---------------------------------------------------------------------------------------------------------------------------
    // Jackson YAML emitter of the parse command

    private fun jacksonYaml(): JsonlTable {
        val table = JsonlTable(
            "jackson_yaml_emit",
            "The YAML MarathonTestParseCommand writes for `parse -o` (its own ObjectMapper, taken by reflection): " +
                "ParseCommandResult(tests, flakyTests).",
            linkedMapOf(
                "id" to "row id",
                "description" to "what the row covers",
                "input" to "the ParseCommandResult in canonical form",
                "yaml" to "{ok: mapper.writeValueAsString(result)} or {error}",
            ),
        )
        val command = MarathonTestParseCommand(File("unused"))
        val mapper = MarathonTestParseCommand::class.java.getDeclaredField("mapper").apply { isAccessible = true }.get(command) as ObjectMapper
        fun t(pkg: String, clazz: String, method: String, vararg meta: MetaProperty) = Test(pkg, clazz, method, meta.toList())
        val rows = listOf(
            Triple("empty", "no tests, no flaky tests", ParseCommandResult(emptyList(), null)),
            Triple("empty-flaky", "flaky list present but empty", ParseCommandResult(emptyList(), emptyList())),
            Triple("one", "one test without meta properties", ParseCommandResult(listOf(t("com.example", "FooTest", "testBar")), null)),
            Triple("meta", "meta properties with values of several types", ParseCommandResult(listOf(
                t("com.example", "FooTest", "testBar", MetaProperty("org.junit.Test"), MetaProperty("Owner", linkedMapOf("value" to "me", "n" to 3, "d" to 1.5, "b" to true, "null" to null, "list" to listOf("a", 1)))),
            ), listOf(t("com.example", "FooTest", "testBar")))),
            Triple("quoting", "strings that need quoting or escaping", ParseCommandResult(listOf(
                t("", "yes", "null"), t("~", "123", "1.5"), t("a: b", "#hash", "- dash"), t("'single'", "\"double\"", "back\\slash"),
                t(" lead", "trail ", ""), t("tab\there", "new\nline", "cr\rret"), t("пакет", "Класс", "метод😀"), t("@at", "&amp", "*star"),
                t("!bang", "%pct", "`tick`"), t("true", "False", "0x1F"), t("{brace}", "[bracket]", "a,b"), t("\u0001ctl", " ls", " nbsp"),
            ), null)),
            Triple("long", "a long value (line folding)", ParseCommandResult(listOf(
                t("com.example." + "very.long.package.".repeat(8) + "end", "C", "method with spaces " + "word ".repeat(30)),
            ), null)),
        )
        for ((id, description, value) in rows) {
            table.add(linkedMapOf("id" to id, "description" to description, "input" to Canonical.render(value), "yaml" to attempt { mapper.writeValueAsString(value) }))
        }
        return table
    }
}
