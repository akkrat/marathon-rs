package oracle.hooks

import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import java.io.File
import java.time.Instant

/** Strict input shared with the future Rust replay seams; see docs/design/replay-tapes.md. */
class Tape(
    val wall: ClockSpec,
    val monotonic: ClockSpec,
    val scheduler: SchedulerMode,
    val reads: Map<String, List<Long>>,
    val draws: Map<String, List<JsonElement>>,
    val ids: Map<String, IdSpec>,
    val barriers: List<String>,
) {
    enum class ClockMode { REAL, FROZEN, SCHEDULER, STEP }
    enum class SchedulerMode { REAL, VIRTUAL }
    class ClockSpec(val mode: ClockMode, val start: Long, val step: Long)
    class IdSpec(val values: List<String>?, val pattern: String?, val start: Long)

    companion object {
        const val FORMAT = "marathon-replay/tape@1"
        fun read(file: File): Tape = parse(JsonParser.parseString(file.readText()).asJsonObject, file.path)

        fun parse(json: JsonObject, where: String): Tape {
            fun fail(message: String): Nothing = throw OracleTapeException("$where: $message")
            fun JsonObject.only(vararg keys: String) {
                keySet().firstOrNull { it !in keys }?.let { fail("unknown key $it") }
            }
            fun string(value: JsonElement): String {
                if (!value.isJsonPrimitive || !value.asJsonPrimitive.isString) fail("expected string: $value")
                return value.asString
            }
            fun integer(value: JsonElement): Long {
                if (!value.isJsonPrimitive || !value.asJsonPrimitive.isNumber || !value.toString().matches(Regex("-?(0|[1-9][0-9]*)"))) {
                    fail("expected integer: $value")
                }
                return value.toString().toLongOrNull() ?: fail("integer outside signed 64-bit range: $value")
            }
            fun obj(parent: JsonObject, key: String): JsonObject {
                val value = parent.get(key) ?: return JsonObject()
                if (!value.isJsonObject) fail("$key must be an object")
                return value.asJsonObject
            }
            fun array(value: JsonElement): List<JsonElement> {
                if (!value.isJsonArray) fail("expected array: $value")
                return value.asJsonArray.toList()
            }
            json.only("format", "clocks", "reads", "draws", "ids", "barriers")
            if (json.get("format")?.let(::string) != FORMAT) fail("format must be $FORMAT")
            val clocks = obj(json, "clocks")
            clocks.only("wall", "monotonic", "scheduler")
            fun clock(name: String, startKey: String, stepKey: String, parseStart: (JsonElement) -> Long): ClockSpec {
                if (!clocks.has(name)) return ClockSpec(ClockMode.REAL, 0, 0)
                val spec = obj(clocks, name)
                spec.only("mode", startKey, stepKey)
                val mode = when (spec.get("mode")?.let(::string)) {
                    "real" -> ClockMode.REAL
                    "frozen" -> ClockMode.FROZEN
                    "scheduler" -> ClockMode.SCHEDULER
                    "step" -> ClockMode.STEP
                    else -> fail("invalid clocks.$name.mode")
                }
                val start = spec.get(startKey)?.let(parseStart)
                if (mode != ClockMode.REAL && start == null) fail("clocks.$name.$startKey is required")
                val step = spec.get(stepKey)?.let(::integer)
                if ((mode == ClockMode.STEP) != (step != null)) fail("$stepKey goes only with step mode")
                if (step != null && step < 0) fail("$stepKey must be nonnegative")
                return ClockSpec(mode, start ?: 0, step ?: 0)
            }
            val wall = clock("wall", "start", "stepMillis") { Instant.parse(string(it)).toEpochMilli() }
            val monotonic = clock("monotonic", "startNanos", "stepNanos", ::integer)
            val scheduler = if (!clocks.has("scheduler")) SchedulerMode.REAL else {
                val spec = obj(clocks, "scheduler")
                spec.only("mode")
                when (spec.get("mode")?.let(::string)) {
                    "real" -> SchedulerMode.REAL
                    "virtual" -> SchedulerMode.VIRTUAL
                    else -> fail("invalid clocks.scheduler.mode")
                }
            }
            if ((wall.mode == ClockMode.SCHEDULER || monotonic.mode == ClockMode.SCHEDULER) && scheduler != SchedulerMode.VIRTUAL) {
                fail("scheduler-derived clocks require virtual scheduler mode")
            }
            val reads = obj(json, "reads").entrySet().associate { (label, values) -> label to array(values).map(::integer) }
            val draws = obj(json, "draws").entrySet().associate { (label, values) ->
                label to array(values).onEach {
                    if (it.isJsonPrimitive && it.asJsonPrimitive.isString) string(it)
                    else if (integer(it) !in Int.MIN_VALUE..Int.MAX_VALUE) fail("draws.$label: outside signed 32-bit range")
                }
            }
            val ids = obj(json, "ids").entrySet().associate { (label, value) ->
                if (!value.isJsonObject) fail("ids.$label must be an object")
                val spec = value.asJsonObject
                spec.only("values", "pattern", "start")
                val values = spec.get("values")?.let { array(it).map(::string) }
                val pattern = spec.get("pattern")?.let(::string)
                if ((values == null) == (pattern == null)) fail("ids.$label: give either values or pattern")
                if (values != null && spec.has("start")) fail("ids.$label: start goes with pattern")
                if (pattern != null && !pattern.matches(Regex("[^%]*%0?[0-9]*[dx][^%]*"))) {
                    fail("ids.$label: pattern must contain one integer %d or %x conversion")
                }
                label to IdSpec(values, pattern, spec.get("start")?.let(::integer) ?: 1)
            }
            return Tape(wall, monotonic, scheduler, reads, draws, ids,
                json.get("barriers")?.let { array(it).map(::string) } ?: emptyList())
        }
    }
}

class OracleTapeException(message: String) : IllegalStateException(message)
