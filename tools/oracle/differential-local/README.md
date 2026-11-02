# Actual local differential probe

`tools/oracle/build.sh reference` verifies and builds this checkout's disposable
clone at `e47352f2 + 0001`. `run NEW_DIRECTORY` compiles `Probe.kt` against those
installed jars with cached Kotlin 1.9.24 and executes the actual local executor
and file bridge under JBR. The reference tree is never modified. No Xcode or
real device is involved. The shell exits 7 intentionally; the probe must exit 0.

The paired/capture entrypoint is `tools/differential/run`; it isolates HOME and
TMPDIR, pins locale/timezone, retains process logs, hashes every installed jar
and records the entire binary-copy tree. `MRS_GRADLE_CACHE` lets compilation
read the existing dependency cache while the JVM's HOME is isolated. This is
only a harness path, never a product configuration key.
