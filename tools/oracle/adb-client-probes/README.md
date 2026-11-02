# adb client probes (WP 6.1)

`tools/oracle/adb-client-probes/run > fixtures/oracle/adb-client-probes.jsonl` compiles `AdbClientProbe.kt`
with the Kotlin 1.9.24 compiler from the Gradle cache and runs it on the reference JDK against the jars of the
Kotlin marathon's built CLI distribution (`cli/build/install/marathon/lib`, read-only; `MARATHON_KT` overrides
the checkout): adam 0.5.8, vert.x 4.4.6, netty 4.1.100 and kotlinx-coroutines 1.8.1, exactly as marathon runs
them. Each case talks to a one-connection byte stub inside the probe.

The cases confirm the adam behaviours 6.1 keeps or deliberately changes: the sign-extended v2 exit code, the
empty rejection at EOF during the handshake (and its WARN line), the track-devices EOF error, the exception a
device line without a tab gives (P6 keeps its class and message), the idle timeout
counted in seconds, per-read v1 decoding (D11), the corrupted header of an over-0xFFFF request (P5), what
`close()` does to open streams and new requests, and the connect-refused exception. Review round 2 added a
half-delivered v2 packet, the idle timer on a blocked write, what the server sees at `close()`, `close()` before
the first request, a compat `close` outliving a receive, the refusal's cause, IPv6 refusal and Java's
`InetSocketAddress.toString()`. The idle rows floor the elapsed time to tenths of a second.
`crates/marathon-adb/src/client/oracle_tests.rs` replays every case against the Rust client, except
`inet-socket-address`, which `crates/jcompat/src/net.rs` replays.
