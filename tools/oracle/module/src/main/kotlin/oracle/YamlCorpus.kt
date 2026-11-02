package oracle

import com.fasterxml.jackson.annotation.JsonIgnoreProperties
import com.fasterxml.jackson.core.JsonToken
import com.fasterxml.jackson.core.base.ParserMinimalBase
import com.fasterxml.jackson.dataformat.yaml.YAMLParser
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.FlakinessStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import org.apache.commons.text.StringSubstitutor
import org.apache.commons.text.lookup.StringLookupFactory
import java.io.File
import java.nio.file.Files
import java.time.Duration
import java.time.Instant
import java.util.Base64

// Binding targets of the spike rows. `k` is the only field; the defaults (7, true, "default") make "key absent" and
// "zero value" distinguishable. Other keys are ignored so that rows can define anchors next to `k`; unknown-key rejection is
// covered by the mutation rows, which bind marathon's own types.
@JsonIgnoreProperties(ignoreUnknown = true)
data class TString(val k: String? = "default")
@JsonIgnoreProperties(ignoreUnknown = true)
data class TStringNonNull(val k: String = "default")
@JsonIgnoreProperties(ignoreUnknown = true)
data class TInt(val k: Int = 7)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TLong(val k: Long = 7)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TDouble(val k: Double = 7.0)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TBoolean(val k: Boolean = true)
enum class Color { RED, GREEN, BLUE }
@JsonIgnoreProperties(ignoreUnknown = true)
data class TEnum(val k: Color? = null)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TList(val k: List<String>? = null)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TListNonNull(val k: List<String> = listOf("default"))
@JsonIgnoreProperties(ignoreUnknown = true)
data class TMap(val k: Map<String, String>? = null)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TInstant(val k: Instant? = null)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TDuration(val k: Duration? = null)
@JsonIgnoreProperties(ignoreUnknown = true)
data class TFile(val k: File? = null)

/** The P1 emission sample of docs/research/yaml.md §1.6 (C60b). */
data class EmitSample(val name: String, val count: Int, val word: String, val colon: String, val nothing: String?, val flag: Boolean)

/**
 * The YAML spike corpus (docs/research/yaml.md §5, rows C01–C59) and the mutation families of PLAN.md §6.3, run through
 * marathon's Marathonfile mapper (profile P1) as a token stream and bound to types (`oracle yaml-corpus <file>`).
 */
object YamlCorpus {
    val NOW: Instant = Instant.parse("2026-10-12T00:00:00Z")
    private const val DIR_PLACEHOLDER = "<MARATHONFILE_DIR>"

    internal val simpleTargets: List<Pair<String, Class<*>>> = listOf(
        "String?" to TString::class.java,
        "String" to TStringNonNull::class.java,
        "Int" to TInt::class.java,
        "Long" to TLong::class.java,
        "Double" to TDouble::class.java,
        "Boolean" to TBoolean::class.java,
        "Enum?" to TEnum::class.java,
        "List<String>?" to TList::class.java,
        "List<String>" to TListNonNull::class.java,
        "Map<String,String>?" to TMap::class.java,
        "Instant?" to TInstant::class.java,
        "Duration?" to TDuration::class.java,
        "File?" to TFile::class.java,
    )

    private val strategyTargets: List<Pair<String, Class<*>>> = listOf(
        "SortingStrategyConfiguration" to SortingStrategyConfiguration::class.java,
        "BatchingStrategyConfiguration" to BatchingStrategyConfiguration::class.java,
        "FlakinessStrategyConfiguration" to FlakinessStrategyConfiguration::class.java,
    )

    fun generate(out: File) {
        val dir = Files.createTempDirectory("oracle-yaml").toFile()
        try {
            val table = JsonlTable(
                out.nameWithoutExtension,
                "YAML corpus for spike 0.3b: docs/research/yaml.md §5 rows (family spike) and the PLAN.md §6.3 mutation " +
                    "families over a base Marathonfile (family mutation), through marathon's Marathonfile mapper (profile P1, " +
                    "time provider fixed at $NOW).",
                linkedMapOf(
                    "id" to "row id (spike: Cnn + letter; emit: C60x; mutation: M-<path>-<mutation>)",
                    "family" to "spike | strategy | emit | mutation",
                    "note" to "what the row checks",
                    "input" to "the YAML text; absent when input_gen is given. Emit rows: the Marathonfile (C60c) or " +
                        "the value serialised, in canonical form",
                    "input_gen" to "for large inputs: the text is the concatenation of each [segment, count] repeated count times",
                    "env" to "mutation rows: the environment for ${'$'}{...} substitution",
                    "tokens" to "{tokens: [...], error: null | {class, message}}; mutation rows other than M-base store " +
                        "{samePrefix, sameSuffix, tokens, error}: the stream is M-base's first samePrefix tokens, then tokens, " +
                        "then M-base's last sameSuffix tokens; a token is [code, text] or [code, text, value], " +
                        "codes { } [ ] F(field) S(string) I(int) D(float) T F0(false) N E(embedded, base64), an optional " +
                        "trailing object holds anchor/tag; texts over 256 chars (error messages: 4000) are replaced by <N chars, sha256 …>",
                    "emit" to "emit rows only: {ok: YAML} written by the Marathonfile mapper (ConfigurationFactory's " +
                        "mapper, the path of ConfigurationFactory.serialize) or {error}",
                    "bind" to "spike/strategy: map target -> {ok: canonical value of k (or of the object)} | {nullDocument: true} | " +
                        "{error: {class, message}}; mutation: {Configuration: {stage, error, diff}} where diff lists " +
                        "[path, base value, value] against row M-base and paths under the Marathonfile directory read $DIR_PLACEHOLDER",
                ),
            )
            spikeRows(table)
            strategyRows(table)
            emitRows(table, dir)
            mutationRows(table, dir)
            @Suppress("UNCHECKED_CAST")
            table.rows.replaceAll { normalize(it, dir) as Map<String, Any?> }
            table.writeTo(out.parentFile.also { it.mkdirs() }).renameTo(out)
        } finally {
            dir.deleteRecursively()
        }
    }

    private val mapper = marathonfileMapper(FixedTimeProvider(NOW), File("/"))

    // -----------------------------------------------------------------------------------------------------------------------------
    // Tokens and binding

    private fun short(s: String, limit: Int = 256): String =
        if (s.length <= limit) s else "<${s.length} chars, sha256 ${sha256(s.toByteArray(Charsets.UTF_8))}>"

    fun tokens(text: String): Map<String, Any?> {
        val out = mutableListOf<List<Any?>>()
        var error: Throwable? = null
        try {
            mapper.createParser(text).use { p ->
                while (true) {
                    val t = p.nextToken() ?: break
                    val tok: MutableList<Any?> = when (t) {
                        JsonToken.START_OBJECT -> mutableListOf("{")
                        JsonToken.END_OBJECT -> mutableListOf("}")
                        JsonToken.START_ARRAY -> mutableListOf("[")
                        JsonToken.END_ARRAY -> mutableListOf("]")
                        JsonToken.FIELD_NAME -> mutableListOf("F", short(p.currentName()))
                        JsonToken.VALUE_STRING -> mutableListOf("S", short(p.text))
                        JsonToken.VALUE_NUMBER_INT -> mutableListOf("I", short(p.text), numberValue(p))
                        JsonToken.VALUE_NUMBER_FLOAT -> mutableListOf("D", short(p.text), numberValue(p))
                        JsonToken.VALUE_TRUE -> mutableListOf("T", p.text)
                        JsonToken.VALUE_FALSE -> mutableListOf("F0", p.text)
                        JsonToken.VALUE_NULL -> mutableListOf("N", p.text)
                        JsonToken.VALUE_EMBEDDED_OBJECT -> mutableListOf("E", Base64.getEncoder().encodeToString(p.binaryValue))
                        else -> mutableListOf(t.name)
                    }
                    val yp = p as YAMLParser
                    val extra = linkedMapOf<String, Any?>()
                    yp.objectId?.let { extra["anchor"] = it.toString() }
                    yp.typeId?.let { extra["tag"] = it.toString() }
                    if (extra.isNotEmpty()) tok.add(extra)
                    out.add(tok)
                }
            }
        } catch (t: Throwable) {
            error = t
        }
        return linkedMapOf("tokens" to out, "error" to error?.let { shortError(it) })
    }

    private fun numberValue(p: com.fasterxml.jackson.core.JsonParser): Any? = try {
        "${p.numberType}:${short(p.numberValue.toString())}"
    } catch (t: Throwable) {
        "error:${t.javaClass.name}"
    }

    private fun shortError(t: Throwable): Map<String, Any?> =
        linkedMapOf("class" to t.javaClass.name, "message" to t.message?.let { short(it, 4000) })

    private fun shorten(v: Any?): Any? = when (v) {
        is String -> short(v)
        is Map<*, *> -> v.entries.associateTo(LinkedHashMap()) { (k, x) -> k to shorten(x) }
        is List<*> -> v.map { shorten(it) }
        else -> v
    }

    fun bind(text: String, target: Class<*>, field: Boolean): Map<String, Any?> = try {
        val v = mapper.readValue(text, target)
        when {
            v == null -> mapOf("nullDocument" to true)
            field -> mapOf("ok" to shorten((Canonical.render(v) as Map<*, *>)["k"]))
            else -> mapOf("ok" to shorten(Canonical.render(v)))
        }
    } catch (t: Throwable) {
        mapOf("error" to shortError(t))
    }

    private fun gen(segments: List<Pair<String, Int>>) = buildString { segments.forEach { (s, n) -> repeat(n) { append(s) } } }

    // -----------------------------------------------------------------------------------------------------------------------------
    // Spike rows (docs/research/yaml.md §5)

    private fun spikeRows(table: JsonlTable) {
        val rows = mutableListOf<Triple<String, String, Any>>() // id, note, text or segments
        fun values(case: String, note: String, vararg vs: String) =
            vs.forEachIndexed { i, v -> rows.add(Triple("$case${'a' + i}", note, if (v.isEmpty()) "k:" else "k: $v")) }
        fun doc(id: String, note: String, text: String) = rows.add(Triple(id, note, text))

        values("C01", "YAML 1.1 bool words", "yes", "Yes", "YES", "no", "No", "NO", "on", "On", "ON", "off", "Off", "OFF", "yES")
        values("C02", "true/false spellings", "true", "True", "TRUE", "false", "False", "FALSE", "tRUE")
        values("C03", "single letters are not booleans", "y", "n", "Y", "N")
        values("C04", "quoted scalars are strings", "'yes'", "\"42\"", "\"null\"", "\"1.5\"", "\"true\"", "'~'")
        values("C05", "integers", "42", "-42", "+42", "0", "-0", "2147483648", "9223372036854775807", "9223372036854775808", "-9223372036854775809")
        values("C06", "underscores in integers", "500_000", "1_2", "-5_0", "42_", "1__0", "0x_1F")
        values("C07", "leading underscore", "_42")
        values("C08", "C-style octal", "017", "-017", "0")
        values("C09", "invalid octal", "08", "09", "018")
        values("C10", "0o octal is not YAML 1.1", "0o17")
        values("C11", "hex and binary", "0x1F", "-0x1F", "0b101", "0X1F", "0xG", "0b102")
        values("C12", "sexagesimal integer", "1:20", "190:20:30", "-1:20")
        values("C13", "floats", "1.5", ".5", "1e3", "5.", "1.5e3", "1.5E+3", "-1.5", "+1.5", "1_000.5", "6.8523015e+5")
        values("C14", "inf and nan", ".inf", "-.Inf", ".nan", ".NaN", "+.inf", ".INF")
        values("C15", "sexagesimal float", "1:20.5")
        values("C16", "punctuation scalars", ".", "+", "_", "-", "=")
        values("C17", "null spellings", "~", "null", "Null", "NULL", "nULL")
        values("C18", "plain empty value", "")
        values("C19", "quoted empty", "''", "\"\"")
        values("C20", "dates and epoch numbers", "2001-12-15", "2001-12-15T02:59:43Z", "2001-12-15T02:59:43.1Z", "1000000", "1000000.5")
        values("C21", "!!str tag", "!!str yes", "!!str 42", "!!str ~")
        values("C22", "!!int tag", "!!int 42", "!!int yes", "!!int \"42\"")
        values("C23", "!!bool tag", "!!bool yes", "!!bool y", "!!bool 1")
        values("C24", "!!float tag", "!!float 1", "!!float x")
        values("C25", "!!binary tag", "!!binary QUJDRA==")
        values("C26", "other tags", "!!timestamp 2001-12-15", "!custom foo", "!<tag:yaml.org,2002:str> 5", "!!null x", "!!map {}")
        doc("C27a", "alias to a scalar", "a: &x 5\nk: *x")
        doc("C27b", "alias in a list", "a: &x 5\nk: [*x, *x]")
        doc("C28", "dangling alias", "k: *dangling")
        doc("C29", "forward alias", "k: *later\na: &later 5")
        doc("C30", "redefined anchor", "a: &x 1\nb: &x 2\nk: *x")
        doc("C31a", "merge key with an alias", "b: &b {c: 1}\nk:\n  <<: *b")
        doc("C31b", "merge key with a mapping", "k:\n  <<: {c: 1}\n  d: 2")
        doc("C32", "alias as a key", "a: &k x\n*k : 1")
        doc("C33a", "duplicate key", "k: 1\nk: 2")
        doc("C33b", "duplicate key, different types", "k: [a]\nk: b")
        doc("C34", "second document", "k: 1\n---\nk: 2")
        doc("C35a", "empty first document", "---\n---\nk: 1")
        doc("C35b", "document marker only", "---\n")
        doc("C36a", "empty input", "")
        doc("C36b", "comment only", "# comment only\n")
        doc("C37", "tab indentation", "a:\n\tb: 1")
        doc("C38", "control character", "k: \u0001")
        doc("C39", "byte order mark", "\uFEFFk: 1")
        rows.add(Triple("C40", "60 aliases", "a: &a x\n" + (1..60).joinToString("\n") { "k$it: *a" } + "\nk: *a"))
        rows.add(Triple("C41", "60-deep flow maps", listOf("k: " to 1, "{a: " to 60, "1" to 1, "}" to 60)))
        rows.add(Triple("C42", "1100-deep flow maps", listOf("k: " to 1, "{a: " to 1100, "1" to 1, "}" to 1100)))
        rows.add(Triple("C43a", "scalar of 3,200,000 code points", listOf("k: " to 1, "a" to 3_200_000)))
        rows.add(Triple("C43b", "scalar just under the 3 MiB code point limit", listOf("k: " to 1, "a" to 3_145_000)))
        doc("C44a", "flow sequence", "k: [1, 2]")
        doc("C44b", "flow mapping", "k: {a: b}")
        doc("C44c", "literal block", "k: |\n  line1\n  line2\n")
        doc("C44d", "folded block", "k: >\n  folded\n  text\n")
        doc("C44e", "double-quoted escapes", "k: \"a\\nA\\t\\u00e9\"")
        doc("C44f", "single-quoted escape", "k: 'it''s'")
        doc("C44g", "document end marker", "k: 1\n...\n")
        doc("C44h", "complex key", "? k\n: 1")
        doc("C44i", "CRLF line ends", "k: 1\r\nj: 2\r\n")
        doc("C44j", "block sequence", "k:\n  - a\n  - 1\n  - yes")
        doc("C44k", "literal block keep/strip", "k: |-\n  x\n\n")
        values("C45", "quoted number", "\"42\"", "\"4_2\"", "\"0x1F\"", "\" 42\"")
        values("C46", "numbers and words into strings", "017", "0x1F", "1.50", "1e3")
        values("C48", "float into int", "1.9", "-1.9", "1e3", "1.5e10")
        values("C50", "enum ordinals and names", "0", "1", "5", "-1", "\"1\"", "RED", "red", "Red", "GREEN ")
        values("C54", "booleans from numbers and strings", "1", "\"1\"", "\"0\"", "\"yes\"", "\"true\"", "\"TRUE\"", "0")
        values("C57", "durations", "\"PT30S\"", "30", "PT30S", "-PT1H", "30.5", "\"30\"", "P1D", "1:00")
        values("C61", "files", "rel/path", "/abs/path", "./x/../y", "~/x", "\"\"")
        values("C47", "null, plain empty and quoted empty into strings", "~", "", "''", "null")
        values("C49", "booleans into numbers", "true", "false", "yes")
        values("C51", "empty string and null into primitives and collections", "\"\"", "null", "''")
        values("C52", "Kotlin nullability: explicit null against nullable, non-null, primitive and collection fields", "null", "~")
        doc("C52z", "Kotlin nullability: key absent, so each default applies", "j: 1")
        values("C55", "long range", "9223372036854775807", "9223372036854775808", "-9223372036854775808", "-9223372036854775809")
        values("C56", "scalar into a collection", "5", "x", "\"\"", "[5]", "{a: 5}")

        for ((id, note, input) in rows) {
            @Suppress("UNCHECKED_CAST")
            val text = if (input is String) input else gen(input as List<Pair<String, Int>>)
            val row = linkedMapOf<String, Any?>("id" to id, "family" to "spike", "note" to note)
            if (input is String) row["input"] = input else row["input_gen"] = (input as List<*>).map { (it as Pair<*, *>).toList() }
            row["tokens"] = tokens(text)
            row["bind"] = simpleTargets.associate { (name, cls) -> name to bind(text, cls, field = true) }.toSortedMap()
            table.add(row)
        }
    }

    // -----------------------------------------------------------------------------------------------------------------------------
    // Strategy rows: timeLimit handling of the hand-written deserializers (C58) and findValue shadowing (C59)

    private fun strategyRows(table: JsonlTable) {
        val limits = listOf(
            "\"2026-01-01T00:00:00Z\"", "\"-PT1H\"", "-PT1H", "1000000", "1000000.5", "PT1H", "\"bogus\"", "[1]", "{a: 1}", "true", "~", "",
        )
        val docs = mutableListOf<Pair<String, String>>()
        for ((i, l) in limits.withIndex()) {
            val tl = if (l.isEmpty()) "timeLimit:" else "timeLimit: $l"
            docs += "C58-sort-$i" to "type: execution-time\npercentile: 80\n$tl"
            docs += "C58-rate-$i" to "type: success-rate\n$tl"
            docs += "C58-flaky-$i" to "type: probability\nminSuccessRate: 0.7\nmaxCount: 3\n$tl"
            docs += "C58-batch-$i" to "type: fixed-size\nsize: 5\n$tl"
        }
        docs += "C58-sort-missing" to "type: execution-time\npercentile: 80"
        docs += "C58-batch-missing" to "type: fixed-size\nsize: 5"
        docs += "C58-flaky-missing" to "type: probability\nminSuccessRate: 0.7\nmaxCount: 3"
        docs += "C58-batch-unknown" to "type: fixed-size\nsize: 5\nzzz: 1"
        docs += "C58-rate-unknown" to "type: success-rate\ntimeLimit: \"2026-01-01T00:00:00Z\"\nzzz: 1"
        docs += "C59-batch-nested-first" to "type: fixed-size\nnested: {timeLimit: PT1H, size: 9}\nsize: 5\ntimeLimit: -PT2H"
        docs += "C59-batch-nested-last" to "type: fixed-size\nsize: 5\ntimeLimit: -PT2H\nnested: {timeLimit: PT1H, size: 9}"
        docs += "C59-batch-nested-only" to "type: fixed-size\nnested: {size: 9, timeLimit: PT1H}"
        docs += "C59-sort-nested-first" to "type: execution-time\nx: {percentile: 1}\npercentile: 80\ntimeLimit: -PT1H"
        docs += "C59-flaky-deep" to "type: probability\na: {b: {minSuccessRate: 0.1}}\nminSuccessRate: 0.7\nmaxCount: 3\ntimeLimit: -PT1H"
        docs += "C59-batch-type-last" to "size: 5\ntype: fixed-size"
        // C53: unknown keys. Most of marathon's types reject them; the hand-written deserializers (influxdb and its
        // retention policy, graphite, execution-time, probability, fixed-size) read only the keys they know.
        val c53 = listOf(
            "C53a" to ("type: influxdb\nurl: u\nuser: u\npassword: p\ndbName: d\nretentionPolicyConfiguration: {name: n, duration: 1d, shardDuration: 1h, replicationFactor: 1, isDefault: true, zzz: 1}\nzzz: 1\nreadOnly: true" to "AnalyticsConfiguration"),
            "C53b" to ("type: graphite\nhost: h\nport: 2003\nzzz: 1\nreadOnly: true" to "AnalyticsConfiguration"),
            "C53c" to ("type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\nzzz: 1" to "AnalyticsConfiguration"),
            "C53d" to ("type: execution-time\npercentile: 80\ntimeLimit: -PT1H\nzzz: 1" to "SortingStrategyConfiguration"),
            "C53e" to ("type: probability\nminSuccessRate: 0.7\nmaxCount: 3\ntimeLimit: -PT1H\nzzz: 1" to "FlakinessStrategyConfiguration"),
            "C53f" to ("type: fixed-size\nsize: 5\nzzz: 1" to "BatchingStrategyConfiguration"),
            "C53g" to ("maxPath: 1\nzzz: 1" to "OutputConfiguration"),
            "C53h" to ("mode: ANY_SUCCESS\nzzz: 1" to "ExecutionStrategyConfiguration"),
            "C53i" to ("type: count\ncount: 2\nzzz: 1" to "ShardingStrategyConfiguration"),
        )
        val typedTargets: Map<String, Class<*>> = strategyTargets.toMap() + mapOf(
            "AnalyticsConfiguration" to com.malinskiy.marathon.config.AnalyticsConfiguration::class.java,
            "OutputConfiguration" to com.malinskiy.marathon.config.OutputConfiguration::class.java,
            "ExecutionStrategyConfiguration" to com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration::class.java,
            "ShardingStrategyConfiguration" to com.malinskiy.marathon.config.strategy.ShardingStrategyConfiguration::class.java,
        )
        for ((id, pair) in c53) {
            val (text, target) = pair
            table.add(
                linkedMapOf(
                    "id" to id, "family" to "strategy", "note" to "unknown keys (C53)", "input" to text,
                    "tokens" to tokens(text),
                    "bind" to mapOf(target to bind(text, typedTargets.getValue(target), field = false)),
                )
            )
        }
        for ((id, text) in docs) {
            table.add(
                linkedMapOf(
                    "id" to id, "family" to "strategy", "note" to "strategy deserializers", "input" to text,
                    "tokens" to tokens(text),
                    "bind" to strategyTargets.filter { it.first.startsWith(target(id)) }
                        .associate { (name, cls) -> name to bind(text, cls, field = false) },
                )
            )
        }
    }

    private fun target(id: String) = when (id.split('-')[1]) {
        "sort", "rate" -> "Sorting"
        "flaky" -> "Flakiness"
        else -> "Batching"
    }

    // -----------------------------------------------------------------------------------------------------------------------------
    // Mutation families (PLAN.md §6.3) over a base Android Marathonfile, bound through ConfigurationFactory

    private sealed class Y {
        data class S(val text: String) : Y()
        data class M(val entries: List<Pair<String, Y>>, val tag: String? = null) : Y()
        data class L(val items: List<Y>) : Y()
    }

    private fun m(vararg e: Pair<String, Y>) = Y.M(e.toList())
    private fun s(t: String) = Y.S(t)

    private val base: Y.M = m(
        "name" to s("&nameAnchor \"sample-app tests\""),
        "outputDir" to s("\"./marathon\""),
        "ignoreFailures" to s("false"),
        "isCodeCoverageEnabled" to s("false"),
        "uncompletedTestRetryQuota" to s("3"),
        "testBatchTimeoutMillis" to s("900000"),
        "testOutputTimeoutMillis" to s("60000"),
        "debug" to s("true"),
        "screenRecordingPolicy" to s("\"ON_ANY\""),
        "deviceInitializationTimeoutMillis" to s("120000"),
        "includeSerialRegexes" to Y.L(listOf(s("\"emulator-.*\""))),
        "analyticsTracking" to s("false"),
        "bugsnagReporting" to s("false"),
        "poolingStrategy" to m("type" to s("\"omni\"")),
        "shardingStrategy" to m("type" to s("\"count\""), "count" to s("1")),
        "sortingStrategy" to m("type" to s("\"success-rate\""), "timeLimit" to s("\"2026-01-01T00:00:00Z\""), "ascending" to s("true")),
        "batchingStrategy" to m("type" to s("\"fixed-size\""), "size" to s("5"), "timeLimit" to s("\"-PT1H\"")),
        "flakinessStrategy" to m("type" to s("\"ignore\"")),
        "retryStrategy" to m("type" to s("\"fixed-quota\""), "totalAllowedRetryQuota" to s("100"), "retryPerTestQuota" to s("2")),
        "filteringConfiguration" to m(
            "allowlist" to Y.L(listOf(m("type" to s("\"simple-class-name\""), "regex" to s("\".*Test\"")))),
        ),
        "executionStrategy" to m("mode" to s("\"ANY_SUCCESS\""), "fast" to s("false")),
        "outputConfiguration" to m("maxPath" to s("1024")),
        "vendorConfiguration" to m(
            "type" to s("\"Android\""),
            "androidSdk" to s("\"/opt/android-sdk\""),
            "applicationApk" to s("\"app.apk\""),
            "testApplicationApk" to s("\"test.apk\""),
            "autoGrantPermission" to s("true"),
            "instrumentationArgs" to m("debug" to s("\"false\"")),
            "serialStrategy" to s("\"AUTOMATIC\""),
            "waitForDevicesTimeoutMillis" to s("15000"),
            "timeoutConfiguration" to m("shell" to s("\"PT20S\"")),
        ),
    )

    private fun emit(node: Y.M): String = StringBuilder().also { emitMap(it, node, 0) }.toString()

    private fun emitMap(sb: StringBuilder, node: Y.M, indent: Int, firstPrefix: String? = null) {
        node.entries.forEachIndexed { i, (k, v) ->
            if (i == 0 && firstPrefix != null) sb.append(firstPrefix) else sb.append(" ".repeat(indent))
            sb.append(k).append(':')
            emitValue(sb, v, indent)
        }
    }

    private fun emitValue(sb: StringBuilder, v: Y, indent: Int) {
        when (v) {
            is Y.S -> sb.append(if (v.text.isEmpty()) "\n" else " ${v.text}\n")
            is Y.M -> {
                if (v.tag != null) sb.append(' ').append(v.tag)
                if (v.entries.isEmpty()) {
                    sb.append(" {}\n")
                } else {
                    sb.append('\n')
                    emitMap(sb, v, indent + 2)
                }
            }
            is Y.L -> {
                sb.append('\n')
                v.items.forEach { item ->
                    when (item) {
                        is Y.S -> sb.append(" ".repeat(indent + 2)).append("- ").append(item.text).append('\n')
                        is Y.M -> emitMap(sb, item, indent + 4, " ".repeat(indent + 2) + "- ")
                        is Y.L -> throw IllegalArgumentException("nested lists are not used")
                    }
                }
            }
        }
    }

    /** Key paths of every mapping entry, with list indices as numbers. */
    private fun paths(node: Y, prefix: List<Any>, out: MutableList<List<Any>>) {
        when (node) {
            is Y.M -> node.entries.forEach { (k, v) -> out.add(prefix + k); paths(v, prefix + k, out) }
            is Y.L -> node.items.forEachIndexed { i, v -> paths(v, prefix + i, out) }
            is Y.S -> Unit
        }
    }

    private fun at(node: Y, path: List<Any>): Y = path.fold(node) { n, seg ->
        when (n) {
            is Y.M -> n.entries.first { it.first == seg }.second
            is Y.L -> n.items[seg as Int]
            is Y.S -> throw IllegalArgumentException("no path $path")
        }
    }

    /** Rewrites the entries of the mapping that holds the last key of [path]. */
    private fun edit(node: Y, path: List<Any>, f: (MutableList<Pair<String, Y>>, Int) -> Unit): Y {
        if (path.size == 1) {
            node as Y.M
            val entries = node.entries.toMutableList()
            f(entries, entries.indexOfFirst { it.first == path[0] })
            return node.copy(entries = entries)
        }
        return when (node) {
            is Y.M -> node.copy(entries = node.entries.map { (k, v) -> if (k == path[0]) k to edit(v, path.drop(1), f) else k to v })
            is Y.L -> Y.L(node.items.mapIndexed { i, v -> if (i == path[0]) edit(v, path.drop(1), f) else v })
            is Y.S -> throw IllegalArgumentException("no path $path")
        }
    }

    private fun mutations(value: Y, key: String): List<Pair<String, (MutableList<Pair<String, Y>>, Int) -> Unit>> {
        val list = mutableListOf<Pair<String, (MutableList<Pair<String, Y>>, Int) -> Unit>>()
        fun set(v: Y): (MutableList<Pair<String, Y>>, Int) -> Unit = { e, i -> e[i] = e[i].first to v }
        list += "drop" to { e, i -> e.removeAt(i) }
        if (key.first().isLetter()) {
            val flipped = (if (key.first().isUpperCase()) key.first().lowercaseChar() else key.first().uppercaseChar()) + key.drop(1)
            list += "misspell" to { e, i -> e[i] = flipped to e[i].second }
        }
        list += "unknown-sibling" to { e, i -> e.add(i + 1, "oracleUnknown" to s("1")) }
        list += "duplicate-before" to { e, i -> e.add(i, e[i].first to s("\"oracle-dup\"")) }
        list += "duplicate-after" to { e, i -> e.add(i + 1, e[i].first to s("\"oracle-dup\"")) }
        list += "alias" to set(s("*nameAnchor"))
        list += "null" to set(s("null"))
        list += "empty" to set(s(""))
        when (value) {
            is Y.S -> {
                val text = value.text.removePrefix("&nameAnchor ")
                val plain = text.removeSurrounding("\"")
                list += "mistype-list" to set(Y.L(listOf(s(text))))
                list += "mistype-map" to set(m("k" to s(text)))
                list += (if (text.startsWith("\"")) "unquote" else "quote") to set(s(if (text.startsWith("\"")) plain else "\"$text\""))
                list += "single-quote" to set(s("'${plain.replace("'", "''")}'"))
                for (lit in listOf("yes", "0x10", "1_000", ".inf", "1:20")) list += "yaml11-$lit" to set(s(lit))
                list += "subst-defined" to set(s("\${ORACLE_V}"))
                list += "subst-default" to set(s("\${ORACLE_UNSET:-$text}"))
                list += "subst-undefined" to set(s("\${ORACLE_UNSET}"))
                list += "tag-str" to set(s("!!str $text"))
                list += "tag-custom" to set(s("!oracle $text"))
                list += "anchor" to set(s("&oracleAnchor $text"))
            }
            is Y.M -> {
                list += "mistype-scalar" to set(s("x"))
                list += "mistype-list" to set(Y.L(listOf(s("x"))))
                list += "empty-map" to set(Y.M(emptyList()))
                list += "tag-custom" to set(value.copy(tag = "!oracle"))
            }
            is Y.L -> {
                list += "mistype-scalar" to set(s("x"))
                list += "empty-list" to set(s("[]"))
            }
        }
        return list
    }

    /** C60: the P1 emission sample of docs/research/yaml.md §1.6 and a full Configuration, through ConfigurationFactory.serialize's mapper. */
    private fun emitRows(table: JsonlTable, dir: File) {
        val sample = linkedMapOf("name" to "hello", "count" to 42, "word" to "yes", "colon" to "a:b", "nothing" to null, "flag" to true)
        fun emitted(block: () -> String): Map<String, Any?> = try {
            mapOf("ok" to block())
        } catch (t: Throwable) {
            mapOf("error" to shortError(t))
        }
        table.add(
            linkedMapOf(
                "id" to "C60a", "family" to "emit", "note" to "research note §1.6 sample as a LinkedHashMap",
                "input" to Canonical.render(sample), "emit" to emitted { mapper.writeValueAsString(sample) },
            )
        )
        val pojo = EmitSample("hello", 42, "yes", "a:b", null, true)
        table.add(
            linkedMapOf(
                "id" to "C60b", "family" to "emit", "note" to "research note §1.6 sample as a Kotlin data class",
                "input" to Canonical.render(pojo), "emit" to emitted { mapper.writeValueAsString(pojo) },
            )
        )
        File(dir, "app.apk").writeText("apk")
        File(dir, "test.apk").writeText("apk")
        val file = File(dir, "Marathonfile")
        val text = emit(base)
        file.writeText(text)
        val factory = com.malinskiy.marathon.config.serialization.ConfigurationFactory(
            marathonfileDir = dir.canonicalFile, mapper = marathonfileMapper(FixedTimeProvider(NOW), dir.canonicalFile),
            analyticsTracking = true, bugsnagReporting = true,
        )
        table.add(
            linkedMapOf(
                "id" to "C60c", "family" to "emit", "note" to "ConfigurationFactory.serialize of the M-base configuration",
                "input" to text, "emit" to emitted { factory.serialize(factory.parse(file)) },
            )
        )
    }

    private fun mutationRows(table: JsonlTable, dir: File) {
        File(dir, "app.apk").writeText("apk")
        File(dir, "test.apk").writeText("apk")
        val baseText = emit(base)
        val baseResult = bindConfiguration(dir, baseText, emptyMap())
        val baseConfig = baseResult["config"]
        @Suppress("UNCHECKED_CAST")
        val baseTokens = tokens(baseText)["tokens"] as List<Any?>
        table.add(
            linkedMapOf(
                "id" to "M-base", "family" to "mutation", "note" to "the base Marathonfile; its directory holds app.apk and test.apk",
                "input" to baseText, "env" to emptyMap<String, String>(), "tokens" to tokens(baseText),
                "bind" to mapOf("Configuration" to linkedMapOf("stage" to baseResult["stage"], "error" to baseResult["error"], "config" to baseConfig)),
            )
        )
        val all = mutableListOf<List<Any>>().also { paths(base, emptyList(), it) }
        for (path in all) {
            val key = path.last() as String
            val value = at(base, path)
            for ((name, f) in mutations(value, key)) {
                val text = emit(edit(base, path, f) as Y.M)
                val env = if (name == "subst-defined") mapOf("ORACLE_V" to (value as Y.S).text.removePrefix("&nameAnchor ")) else emptyMap()
                val r = bindConfiguration(dir, text, env)
                table.add(
                    linkedMapOf(
                        "id" to "M-${path.joinToString(".")}-$name", "family" to "mutation", "note" to name, "input" to text, "env" to env,
                        "tokens" to tokenDelta(baseTokens, tokens(text)),
                        "bind" to mapOf("Configuration" to linkedMapOf("stage" to r["stage"], "error" to r["error"], "diff" to r["config"]?.let { diff(baseConfig, it) })),
                    )
                )
            }
        }
    }

    /**
     * Mutation rows store their tokens relative to M-base: the stream is base[0, samePrefix) + tokens +
     * base[size - sameSuffix, size).
     */
    private fun tokenDelta(base: List<Any?>, result: Map<String, Any?>): Map<String, Any?> {
        @Suppress("UNCHECKED_CAST")
        val t = result["tokens"] as List<Any?>
        var p = 0
        while (p < base.size && p < t.size && base[p] == t[p]) p++
        var q = 0
        while (q < base.size - p && q < t.size - p && base[base.size - 1 - q] == t[t.size - 1 - q]) q++
        return linkedMapOf("samePrefix" to p, "sameSuffix" to q, "tokens" to t.subList(p, t.size - q), "error" to result["error"])
    }

    private fun bindConfiguration(dir: File, text: String, env: Map<String, String>): Map<String, Any?> {
        val file = File(dir, "Marathonfile")
        file.writeText(text)
        val substitutor = StringSubstitutor(StringLookupFactory.INSTANCE.functionStringLookup { k: String -> env[k] })
        val outcome = parseAndValidate(file, NOW, substitutor)
        return linkedMapOf(
            "stage" to outcome.stage,
            "error" to outcome.error?.let { errorJson(it) },
            "config" to outcome.config?.let { Canonical.render(it) },
        )
    }

    /** [path, base value, value] for every leaf that differs; containers that change shape are reported whole. */
    private fun diff(a: Any?, b: Any?, path: String = "", out: MutableList<List<Any?>> = mutableListOf()): List<List<Any?>> {
        when {
            a is Map<*, *> && b is Map<*, *> -> (a.keys + b.keys).map { it as String }.toSortedSet().forEach {
                diff(a[it], b[it], if (path.isEmpty()) it else "$path.$it", out)
            }
            a is List<*> && b is List<*> && a.size == b.size -> a.indices.forEach { diff(a[it], b[it], "$path[$it]", out) }
            a != b -> out.add(listOf(path, a, b))
        }
        return out
    }

    private fun normalize(v: Any?, dir: File): Any? {
        val forms = listOf(dir.canonicalPath, dir.absolutePath).distinct().sortedByDescending { it.length }
        fun n(x: Any?): Any? = when (x) {
            is String -> forms.fold(x) { s, f -> s.replace(f, DIR_PLACEHOLDER) }
            is Map<*, *> -> x.entries.associateTo(LinkedHashMap()) { (k, e) -> k as String to n(e) }
            is List<*> -> x.map { n(it) }
            else -> x
        }
        return n(v)
    }
}
