package probe;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import org.junit.Test;

// Placeholders for the malformed-string probe (WP 6.4 R1). javac and d8 only
// write well-formed MUTF-8, so mutate_dex.py patches the trailing '_' runs in
// place with the byte sequences it lists (same length, no 0x00); the oracle
// then records what `String(bytes)` makes of them, in values and test names.
@Retention(RetentionPolicy.RUNTIME)
@interface Bytes {
    String[] value();
    String single();
}

public class Utf8ProbeTest {
    @Test
    @Bytes(single = "s:______", value = {
        "u00:______", "u01:___", "u02:___", "u03:__", "u04:___", "u05:___", "u06:__",
        "u07:____", "u08:___", "u09:____", "u10:_", "u11:__", "u12:___", "u13:___",
        "u14:____", "u15:____", "u16:____", "u17:_", "u18:_", "u19:__", "u20:___",
        "u21:___", "u22:____", "u23:__", "u24:__", "u25:___", "u26:_", "u27:___",
        "u28:__", "u29:_", "u30:__", "u31:__", "u32:__", "u33:_",
    })
    public void utf8Nul__() {
    }

    @Test
    public void utf8Pair______() {
    }
}
