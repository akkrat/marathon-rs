# WP 3.5 probes

Run from this worktree after `tools/oracle/build.sh reference`:

```sh
tools/oracle/progress-probes/run /tmp/mrs-3.5-oracle
```

The probe compiles against the real `e47352f2` reference distribution with the
cached Kotlin 1.9.24 compiler and Android Studio JBR 21.0.10. It does not replace
or edit a Kotlin implementation. State seeding uses reflection solely to cover
all five states under every mode/fast combination, including unreachable cells.
Only log timestamps/thread names are omitted; logger names, levels, messages,
tracker payloads and relative diagnostic/tracker ordering are retained.

The four outputs are described in `fixtures/PROVENANCE.md`. Rust replays the
entire pool and run tables in `progress_oracle.rs` and `run_results.rs`; extra
Rust tests cover backwards clocks, labelled default-time reads, mutable count
recalculation, original registration keys/name collisions, FIFO output despite
reversed vendor polls, shared-console encoding and swallowed stdout errors.

P2 deliberately removes the overwritten TemporalTestResult wall read in
`testStarted`. This reference probe uses explicit Timer values and records Timer
reads only; a separate Rust test pins the approved wall-label consumption.
P3 makes multi-test aggregate diagnostics insertion ordered in Rust; this corpus
uses one registered name, so no HashMap-order normalization is hidden here.
