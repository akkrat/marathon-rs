# Simulator device JVM evidence

Run from this worktree:

```sh
python3 tools/oracle/simulator-device/run.py /tmp/mrs52-kotlin
```

The generator checks out `e47352f2` in its `/tmp` clone and rewrites each instrumented
file from `git show`, so repeated runs do not accumulate patches. The frozen source
checkout is read only. The reference JBR runs the iOS device/startup/context suites
and the base policy/result-bundle suites offline. Capture.kt records logback events;
observational helpers record actual worker argv/results and device state/error
chains. Witness.kt adds setup fallbacks, plist Java shapes, permissions, screenshot
budgets, every pre-run failure class and parallel preparation failure. Only the
direct deadline witness redirects nanoTime to the Kotlin virtual scheduler;
ordinary frozen suites keep their original clocks.

`tests.jsonl` contains 105 successful test invocations. `events.jsonl` retains the
raw observations with the scratch-root path normalized. The extra RuntimeFlags
probe checks coroutine debug/recovery with production JVM defaults and with
Gradle's assertions. The fixtures' hashes are in `fixtures/PROVENANCE.md`.

Rust's simulator_oracle tests compare deterministic command/metadata/error and
poll-count fields. The named parity suites exercise all 69 assigned rows and
partial orders over the real Rust device, runner and artifact queue. Full-run
normalized replay is WP 7.1's responsibility.

After implementation commits, regenerate status rows with:

```sh
python3 tools/oracle/simulator-device/mark-parity HEAD
```

Checkpoint A accepts an extra `A` argument and updates its 29 rows only.
