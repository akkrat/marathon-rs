# macOS vendor JVM witnesses

Run `python3 tools/oracle/macos-probes/run.py` from the repository root. It reads
`e47352f2` without changing it, compiles the four macOS modules, all three upstream
test classes and current shared Apple dependencies to a temporary directory under
`/tmp`, and invokes the eight JUnit methods and recording probes. It uses the
reference installation jars and Gradle cache via `installer-probes/run.py`;
`MARATHON_KT`, `MARATHON_KOTLIN_LIB` and `JAVA_HOME` override those locations.
No remote connection, process worker or real TCC query is used.

Reference toolchain: Android Studio OpenJDK 21.0.10, Kotlin compiler 1.9.24
(`-jvm-target 17`), coroutines-test 1.8.1, JUnit 5.11.0, Kluent 1.73 and Mockito
5.12.0 / mockito-kotlin 5.4.0. Production library versions come from the frozen
reference installation. See the generator's explicit source and jar lists.

`fixtures/oracle/macos/probes.jsonl` is JVM output. Temporary roots become
`$ROOT`, permission SQL's host wall seconds become `<epoch>`, and object identity
hashes become `@<identity>` (D12); no command/status/path normalization is added.
Gson omits null values, including absent workdir. The first eight rows are the
unchanged Kotlin tests; supplemental rows carry command argv, environment,
timeouts, error chains, facts, provider logs/events and lease counts. The inline
row captures result completion before the pull and the final local rename and
remote removal. The disposal row records the exception/cause after session close. Directory and
permission-denied Marathondevices reads record complete JVM exception chains;
the latter requires an unprivileged user and removes permissions only on a
probe-owned temporary file.

`python3 tools/oracle/macos-probes/mark-parity <test-commit>` validates the Rust
names and regenerates all eight assigned rows through the canonical matrix
script. It does not edit CSV lines by hand.
