@file:OptIn(kotlin.time.ExperimentalTime::class)

package oracle.hooks

import com.google.gson.JsonParser
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.*
import org.junit.jupiter.api.Test

class TapeTest {
    private fun tape(extra: String = "") = Tape.parse(JsonParser.parseString("""{"format":"marathon-replay/tape@1"$extra}""").asJsonObject, "test")

    @Test fun defaults_are_real_and_inactive_hooks_delegate() {
        assertEquals(Tape.ClockMode.REAL, tape().wall.mode)
        assertFalse(OracleHooks.active)
        assertEquals("example.Type@abcdef", OracleHooks.identities("unused", "example.Type@abcdef"))
        assertEquals(42, OracleHooks.runBlocking("unused") { 42 })
    }

    @Test fun separate_clocks_and_per_site_tapes() {
        val t = tape(""", "clocks":{"wall":{"mode":"frozen","start":"2020-01-01T00:00:00Z"},"monotonic":{"mode":"step","startNanos":7,"stepNanos":3},"scheduler":{"mode":"virtual"}},"reads":{"x":[1,2]},"draws":{"x":[-2147483648,2147483647],"y":["serial"]},"ids":{"z":{"values":["one","two"]}}""")
        assertEquals(1577836800000, t.wall.start)
        assertEquals(7, t.monotonic.start)
        assertEquals(listOf(1L, 2L), t.reads["x"])
        assertEquals("serial", t.draws["y"]!!.single().asString)
        assertEquals(listOf("one", "two"), t.ids["z"]!!.values)
    }

    @Test fun rejects_coercion_typos_overflow_and_incompatible_modes() {
        listOf(
            """, "typo":{}""",
            """, "reads":{"x":["1"]}""",
            """, "reads":{"x":[1.5]}""",
            """, "reads":{"x":[9223372036854775808]}""",
            """, "draws":{"x":[true]}""",
            """, "draws":{"x":[2147483648]}""",
            """, "ids":{"x":{"values":[1]}}""",
            """, "ids":{"x":{"values":[],"pattern":"%d"}}""",
            """, "ids":{"x":{"pattern":"%s"}}""",
            """, "clocks":{"wall":{"mode":"scheduler","start":"2020-01-01T00:00:00Z"}}""",
            """, "clocks":{"scheduler":{"mode":"typo"}}"""
        ).forEach { input -> assertThrows(OracleTapeException::class.java, { tape(input) }, input) }
    }

    @Test fun barriers_release_in_script_order_and_detect_extra_events() = runTest {
        val gate = Barriers(listOf("site:a", "site:b"))
        val seen = mutableListOf<String>()
        val b = async { gate.event("site", "b") { seen.add("b") } }
        val a = async { gate.event("site", "a") { seen.add("a") } }
        a.await(); b.await()
        assertEquals(listOf("a", "b"), seen)
        assertTrue(gate.remaining().isEmpty())
        try { gate.event("site", "c") {}; fail<Unit>("accepted an unlisted event") } catch (_: IllegalStateException) {}
    }
}
