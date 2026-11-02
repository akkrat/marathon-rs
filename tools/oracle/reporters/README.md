# WP 3.9c reporter probes

Run from the worktree, using the read-only reference installDist jars:

```sh
python3 tools/oracle/reporters/run \
  /tmp/mrs-rev-2.2-r2-claude/oracle/build/marathon-src/oracle-build/install/oracle/lib
```

The final optional argument selects an output directory (default:
`fixtures/oracle/html-allure-gif`). The Kotlin 1.9.24 compiler comes from the local
Gradle cache; JAVA_HOME defaults to Android Studio's JBR 21.0.10. MARATHON_KT
selects the frozen source, default `~/src/marathon`.
The runner compiles **all 20 assigned source files unchanged** ahead of the
reference distribution on the classpath; it never modifies that checkout or
its dependencies. Compilation and capture/report working files live in a
fresh `/tmp/reporters-oracle-*` directory, removed on completion.

Pinned libraries include Gson 2.11.0, allure-java-commons/model 2.29.0,
allure-kotlin 2.4.0, environment-writer 1.0.0, imgscalr 4.2, Commons Text 1.12.0,
Jackson 2.17.2, and coroutines/test 1.8.1. The complete jar set and SHA-256 hashes
are committed in `jars.sha256`. Allure's shaded Jackson mapper is exercised
through its real writer. No byte-order assumptions come from setter order.

`ReporterProbes.kt` documents the inputs. `probe.json` records path, arithmetic,
exception, capture-count, source timeout, cancellation and log witnesses.
Allure files use explicit UUIDs only at the reflected createTestResult boundary,
then the real schedule/write lifecycle. Raw environment, HTML, timeline, GIF,
input PNG and ImageIO-decoded PNG bytes are retained. `manifest.json` includes
empty directories and hashes of the actual output tree. Empty GIFs contain the
single trailer byte 0x3b; invalid-delay probes leave empty files.

Review round 1 added three probe groups. `resize-ties` is the four-colour 2×4
frame resized to 1×2; `resize-axis-small` lists every 1-row index map for source
and target widths 1–16, and `resize-axis-large` the MD5 of eleven large maps
(realistic screen sizes and pairs only 32-bit fixed point reproduces).
`environment-cases/<n>/environment.xml` and the `environment-case-<n>` rows hold
environment-writer output and the non-indented stderr header lines for control
characters, numeric references, supplementary characters and lone surrogates.
The JVM's stdout is now kept as `stdout.txt` (it was discarded before). It
contains logback's start-up status lines with wall-clock times and staging
paths, then `probe: <id>` markers before every call that can print
environment-writer's `Allure environment data saved.` line.

Regeneration changes the absolute staging root, footer date, and F29 identity
strings in the raw complete configuration environment. The Rust tests normalize
only those relevant typed fields: HTML footer content at the fixed marker;
Allure absolute attachment paths via the recorded root; the `@<hash>` suffix of
F29 identity values when the complete `allure-empty/environment.xml` is compared. Most Allure tests
compare parsed fields under approved P2; the UTF-16 case compares raw bytes
because serde_json rejects unpaired-surrogate JSON that Jackson can emit.
Timeline/HTML Gson projections and templates otherwise compare exact bytes.
Environment uses the full merged Configuration::to_map and is compared with the
complete JVM file (keys, order, values); its small writer witness and the
character cases are byte-exact despite P2 allowing structural comparison.
`check-evidence.py` verifies hashes and the manifest; the negative witnesses run
through the Rust comparators (see the script).

The Rust GIF tests decode original GIFs with `gif`, retaining dimensions,
frame count/order, disposal, alpha and delay. The pinned ImageIO reader has a
tiny-interlaced-image decoding issue, so its generated PNGs are independent raw
evidence, not substituted for the original GIF pixels. Palette differences on
the 1,024-colour gradient are measured and were approved in R1 as documented in
`docs/wp/3.9c-findings.md`. No runtime JVM dependency or JS build is introduced.
