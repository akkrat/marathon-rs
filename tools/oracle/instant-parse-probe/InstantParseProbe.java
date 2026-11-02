import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * WP 2.5 review round 1: `Instant.parse` failures, with the `DateTimeParseException` message (its `at index N`) and
 * the cause chain, for the hidden `config-dump --now` argument. Inputs are hand-picked edges plus every truncation,
 * deletion, insertion and single-character replacement of three valid instants.
 *
 * Run: java tools/oracle/instant-parse-probe/InstantParseProbe.java > fixtures/oracle/jcompat/instant_parse_errors.jsonl
 */
public class InstantParseProbe {
    static final String[] BASES = {
        "2026-01-01T00:00:00Z",
        "+12026-10-12T23:59:60.123+01:00:30",
        "-0001-12-31T24:00:00.5-18:00",
    };
    static final String EDITS = "x09-+:.ZTzt 5";
    static final String[] EDGES = {
        "bogus", "bad", "", "2026-01-01T00:00:00+19:00", "2026-01-01T00:00:00+23:59", "2026-01-01T00:00:00+24:00",
        "2026-01-01T00:00:00+59:00", "2026-01-01T00:00:00+60:00", "2026-01-01T00:00:00+01:60",
        "2026-01-01T00:00:00+01:00:60", "2026-01-01T00:00:00+01:00:", "2026-01-01T00:00:00+01:00:5",
        "2026-01-01T00:00:00+18:00:01", "2026-01-01T00:00:00-18:00", "2026-01-01T00:00:00+1:00",
        "+1000000000-12-31T23:59:59.999999999Z", "+1000000001-01-01T00:00:00Z", "-1000000000-01-01T00:00:00Z",
        "-1000000001-01-01T00:00:00Z", "+12345678901-01-01T00:00:00Z", "12345678901-01-01T00:00:00Z",
        "2026-01-01T00:00:00.1234567891Z", "2026-01-01T00:00:00,5Z", "2026-02-29T00:00:00Z", "2024-02-29T00:00:00Z",
        "2026-01-01T24:00:01Z", "2026-01-01T23:59:60.5Z", "2026-01-01T00:00:00Ž", "٢026-01-01T00:00:00Z",
        "x1234567890123456789012345678901234567890123456789012345678901234567890",
        "2026-01-01T00:00:00Zx1234567890123456789012345678901234567890123456789012345678901234567890",
        "2026-01-01T00:00:00.123456789Zé",
    };

    public static void main(String[] args) {
        Set<String> inputs = new LinkedHashSet<>(List.of(EDGES));
        for (String base : BASES) {
            for (int i = 0; i <= base.length(); i++) {
                inputs.add(base.substring(0, i));
                if (i < base.length()) inputs.add(base.substring(0, i) + base.substring(i + 1));
                for (char c : EDITS.toCharArray()) {
                    inputs.add(base.substring(0, i) + c + base.substring(i));
                    if (i < base.length()) inputs.add(base.substring(0, i) + c + base.substring(i + 1));
                }
            }
        }
        System.out.println("{\"table\":\"instant_parse_errors\",\"description\":\"Instant.parse: the result, or the "
            + "exception message and cause chain\",\"rows\":" + inputs.size() + ",\"jdk\":" + json(System.getProperty("java.vendor")
            + " " + System.getProperty("java.runtime.version")) + ",\"generator\":\"tools/oracle/instant-parse-probe/InstantParseProbe.java\"}");
        for (String input : inputs) {
            StringBuilder row = new StringBuilder("{\"input\":").append(json(input));
            try {
                String ok = Instant.parse(input).toString();
                row.append(",\"ok\":").append(json(ok));
            } catch (RuntimeException e) {
                List<String> chain = new ArrayList<>();
                for (Throwable t = e; t != null; t = t.getCause()) {
                    chain.add("{\"class\":" + json(t.getClass().getName()) + ",\"message\":" + json(t.getMessage()) + "}");
                }
                row.append(",\"error\":[").append(String.join(",", chain)).append("]");
            }
            System.out.println(row.append("}"));
        }
    }

    static String json(String s) {
        if (s == null) return "null";
        StringBuilder b = new StringBuilder("\"");
        for (char c : s.toCharArray()) {
            if (c == '"' || c == '\\') b.append('\\').append(c);
            else if (c < 0x20 || c > 0x7e) b.append(String.format("\\u%04x", (int) c));
            else b.append(c);
        }
        return b.append('"').toString();
    }
}
