# Pool and Scheduler probes (WP 3.8)

Run `tools/oracle/pool-probes/run [output-directory]` from the repository. This requires
JBR 21.0.10, the reference installDist jars in `tools/oracle/build/marathon-src/cli/build/install/marathon/lib`,
and the Gradle-cached dependencies pinned in `run`. `MARATHON_SOURCE` may select a read-only
checkout of `e47352f2`; by default the project's reference checkout is used.

The compiler outputs go to a temporary directory. `adapt.py` copies the original three
pool test sources and StubDevice there. It retains the assertions, records tracker calls,
and substitutes a virtual dispatcher for BatchAccounting's executor. The CountDownLatch
hold becomes two consecutive sends without yielding. The original DevicePoolActor is
compiled ahead of the reference jar with hooks at receive, sendToQueue and completion;
these hooks only append records. Everything else runs from the reference jars.

`tests.jsonl` contains all 31 tests, including the five characterization device counts.
`scheduler.jsonl` contains 19 additional scenarios: timeouts, polling boundaries, filters,
broadcast/absent tracking, late connections, short-circuit aggregation, lazy job traversal,
C27 and B4. Logs preserve logger, level, message and virtual time. Random batch UUIDs in
log messages are named batch-N in first-observation order.

Regeneration is checked with `cmp` on both files. Rust ports compare source assertions,
tracker outcomes and characterization metrics; Scheduler tests compare result/exception,
time, pools and tracker calls. Full JVM message/log transcripts remain available for
review. They are not asserted as one global Rust ordering: vendor bridge completions and
queue tracker/output awaits introduce independent scheduling boundaries. See
`docs/wp/3.8-findings.md` for the review request concerning that scope.
