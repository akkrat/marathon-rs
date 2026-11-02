# WP 6.2 adam request probes

`run` compiles `AdbRequestProbe.kt` in a temporary directory with the reference JBR 21.0.10 and
Kotlin compiler 1.9.24. It uses the read-only reference distribution's adam 0.5.8, vert.x 4.4.6,
Netty 4.1.100 and coroutines 1.8.1 jars, following the WP 6.1 runner. It opens loopback byte stubs
only; it never starts adb, discovers devices or uses real-host credentials.

```
tools/oracle/adb-request-probes/run > fixtures/oracle/adb-request-probes.jsonl
cargo build -p fake-adb
tools/oracle/adb-request-probes/fake-run > fixtures/oracle/adb-request-fake-results.jsonl
```

The first command also exports the reference JDK's standard sRGB ICC profile to
`fixtures/oracle/adb-request-jdk-srgb.icc`. Its SHA-256 is the first JSONL row. This profile differs
from adam's copied `colorProfiles/sRGB.icc`; replacing it with Little CMS's generated sRGB profile
changes decoded component values. Both are needed for parity.

The rows record exact request bytes (hex, including NUL arguments), transmitted file bytes/hashes,
progress, resulting files, exception class/message/cause, session failure/abandon order,
breadth-first directory push/mkdir commands, optional reverse replies, framebuffer acknowledgement,
whole-image ARGB hashes, profile transitions and protocol-16 pixels. The loopback sync request
observations concatenate tag and path (their LE length is consumed by the stub); DATA and DONE
observations retain their complete framing. Temporary local paths in exceptions become `<tmp>` or
`<apk>`; opaque shell/ABB commands are never normalized.

`fake-run` starts only the repository's scripted `fake-adb` on a fresh ephemeral port, sends the
same production requests using the pinned JVM library, joins that process after SIGTERM, requires
strict rc=0, and saves `fixtures/fake-adb/adb-requests/kotlin-transcript.jsonl`. Rust compares typed
request, sync, shell, framebuffer and acknowledgement fields, omitting only connection identity.
It also reuses the unchanged `android-two-devices` scenario for streamed install.

Approved differences have witnesses rather than rewritten expectations:

- P2: `approved-p2-negative-data` records Kotlin's `IllegalArgumentException` for a negative DATA
  length. Rust returns typed `UnsupportedSyncProtocol` before allocation. Short/oversized records
  and inconsistent geometry likewise fail explicitly instead of recycling stale data or hanging.
- P3: `push-missing-construction` records eager reader construction throwing `FileNotFoundException`
  before validation. Rust checks metadata during async preparation and retains the validation result;
  file-open/read/write failures propagate as I/O errors. File I/O is sequential and close is awaited.
- P4: AWT recycled-object identity and profile-only cache poisoning are omitted. Whole-image hashes,
  repeated captures and raw/buffered sRGB/P3/protocol-16 component values remain required. The old PNG
  fixture differs from the current JDK by one level in some channels; the Rust test allows at most
  one level for that historical PNG while comparing current JVM ARGB hashes exactly.

The source JDK uses 16-bit ICC conversions. Little CMS's optimized transform changes dark-channel
rounding; the port uses `NO_OPTIMIZE` and the exported reference output profile. The original
fixture PNGs and both adam profiles are unchanged. No source/expected values come from the Rust
implementation. All stub threads and client factories are closed/joined before JVM exit.
