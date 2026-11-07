# X29: a cheaper readiness probe (D46)

The project owner approved this on 2026-11-07. D46 is a new deviation in PLAN §3.11. No configuration field, CLI option or exit code
changed. One command sent to a worker changed: the readiness probe of a simulator on runtime 18 or later.

## What Kotlin does, and what changed

`SimulatorService.kt:111-151` (`isRunning`) sends `xcrun simctl spawn <udid> launchctl dumpstate user/foreground/com.apple.SpringBoard`
from runtime 16 on and `launchctl print system` below 16 or when the version does not parse. It reads the lines holding
`com.apple.springboard.services` and takes the token before it (`A` started, `D` starting). `dumpstate` prints the whole domain,
10 to 20 MB, and the boot wait asks once per poll.

Now, in `crates/marathon-apple/src/bin/xcrun/simctl/service/simulator.rs`:

| Runtime major | Probe |
|---|---|
| not a number, or below 16 | `launchctl print system` (as before) |
| 16 and 17 | `launchctl dumpstate user/foreground/com.apple.SpringBoard` (as before; not measured, so not moved) |
| 18 and later | `launchctl print user/foreground` |

The parser is unchanged. D38 (a failed or timed-out probe is not running) holds for all three.

## Measurements

The earlier measurement, from the project owner: iOS 18.6, 26.4 and 27.0, exit 0, 97 to 121 KB against 11 to 21 MB, the same
`M A`/`M D` line before `com.apple.springboard.services`, and agreement on 658 of 666 paired rounds, the 8 others all reporting ready up to
one poll earlier. Its raw data is not in the repository.

Re-measured here, briefly, on one throwaway simulator per runtime, created and deleted by this work (no other simulator was touched), each
freshly booted and polled six times at one-second intervals with both probes in turn (`xcrun simctl spawn <udid> launchctl …`):

| Runtime | Probe | Exit | Bytes (min–max) | Time per call (ms) | Marker line, rounds 1–6 |
|---|---|---|---|---|---|
| iOS 18.6 | `dumpstate …SpringBoard` | 0 | 9,575,294 – 10,383,500 | 238–381 | D D D D A A |
| iOS 18.6 | `print user/foreground` | 0 | 99,571 – 100,666 | 225–385 | D D D D A A |
| iOS 26.4 | `dumpstate …SpringBoard` | 0 | 3,991,199 – 13,988,287 | 295–458 | D D D D D D |
| iOS 26.4 | `print user/foreground` | 0 | 106,177 – 108,849 | 272–582 | D D D D D D |
| iOS 27.0 | `dumpstate …SpringBoard` | 0 | 3,605,578 – 17,204,196 | 299–676 | D D D D D D |
| iOS 27.0 | `print user/foreground` | 0 | 111,926 – 114,487 | 259–379 | D D D D D D |

In all 18 pairs both probes printed the same `0x… M <state> com.apple.springboard.services` line, with the same state letter in the same round.
On 18.6 both went from `D` to `A` in round 5. The time per call is dominated by `simctl spawn`, so the saving is in bytes, not in
time per call, on a local simulator. On 26.4 and 27.0 six seconds after boot SpringBoard had not yet reached `A`, so the `A` line was
seen on 18.6 only; the earlier measurement covers the others. `dumpstate` grows with uptime (4 to 17 MB within six seconds on 27.0); `print` stays at about 100 KB.

Not re-measured: runtimes 16 and 17 (not installed), the 658 of 666 agreement figure, and the "up to one poll earlier" cases.

## Where the command was matched, and what happened to each

| Place | Change |
|---|---|
| `crates/marathon-apple/src/bin/xcrun/simctl/service/simulator.rs` | The choice above |
| `tools/fake-worker/src/apple.rs` | The fake answers `print user/foreground` like `dumpstate` (marker when booted, exit 1 with `SpringBoard not running` otherwise) |
| `fixtures/fake-worker/apple/command-catalogue.json` | **Unchanged.** Its one form (`springboard-16plus`) is still the form of runtimes 16 and 17, and `generated_catalogue_matches_the_ssh_executor_and_production_wrappers` now calls `is_running` with `17.5` for it. The scenarios' `semantic.json` count forms, so `tools/fake-worker/apple_evidence.py` counts the new wire as that form (`without_d46_probe`), as it does for D32's prefix |
| `tools/differential/vendor_transport.py` | `with_d46_probe`, applied to a JVM record or a native record pinned before D46 (the same records that D31 and D32 rewrite), only for a probe whose simulator's recorded `SIMULATOR_RUNTIME_VERSION` reply parses to 18 or more; refuses a pre-D46 record that already holds the new command. The lane rule accepts both forms |
| `tools/differential/test_evidence.py` | `test_d46_*`: per-runtime table, the narrowness cases, the refusal, and a live native copy of the committed `ios-smoke` record compared with the JVM golden, with the kept-`dumpstate` case failing |
| `crates/marathon-apple/tests/apple_bin_oracle.rs` | Four `d46_*` tests: runtimes 18 and later (`18`, `18.6`, `26.4`, `27.0`, `+18.1`, `99.9`) against the JVM `sim.running/16.0/*` rows with only the argv replaced; 16 and 17 unchanged; unparsable versions (`""`, `x`, `-1.0`, `2147483648.0`, `１８.0`, `18x`) on `print system`; the allowance changes nothing but the argv. The D38 allowance is unchanged |
| `crates/marathon-ios/tests/simulator_prepare.rs` | The readiness stubs match any `launchctl` probe; a new test pins the probe a runtime 18.6 device sends |
| `crates/marathon-test-support/tests/world_executor.rs` | A new test: the fake answers both forms alike, in process and over SSH |
| `fixtures/oracle/apple-bin/probes.jsonl`, `fixtures/oracle/simulator-device/events.jsonl` | **Untouched.** The first has no row above runtime 16, so Kotlin's argv for 18 is read from its `sim.running/16.0` rows, which hold for every runtime from 16 (the code has one branch). The second holds `dumpstate` at runtime 18.6 and no test compares its probe argv: the setup comparison stops before the first probe |
| `tools/differential/registry.json` | Regenerated with `make_registry.py` |

## Comparator allowance and what it hides

`with_d46_probe` rewrites an `exec` whose wire is exactly `xcrun simctl spawn <udid> launchctl dumpstate user/foreground/com.apple.SpringBoard`,
to exactly `xcrun simctl spawn <udid> launchctl print user/foreground`, when that `<udid>` has a `xcrun simctl getenv <udid> SIMULATOR_RUNTIME_VERSION`
session in the same record whose stdout reads an ASCII integer (one optional plus) of 18 or more before the first dot. It touches no other
field. Negative tests: runtimes 16 and 17, a missing reply, `x.1`, a non-ASCII digit, a negative number and `1x8`; another simulator's
version; another key (`SIMULATOR_RUNTIME_BUILD_VERSION`); `dumpstate` with an extra argument or without the service path; `print system`;
`print user/foreground/com.apple.SpringBoard`; the command inside `sh -c` or after `env`; and a native run that keeps `dumpstate`
on 27.1, which still differs from the JVM record.

## What was run

`cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-test-support -p fake-worker --all-targets -- -D warnings`;
`cargo test` for `marathon-apple`, `marathon-ios`, `marathon-test-support`, `fake-worker`, `marathon-macos` and `marathon-cli`
(`--no-fail-fast`; its `differential_vendors` test, the native vendor differential through the real binary and the fake worker, passes after
the registry was regenerated); `tools/fake-worker/test_apple_evidence.py`, `tools/differential/test_evidence.py`, `tools/differential/run validate`
and `make_registry.py --check`. With the probe choice put back to `dumpstate` for 18 and later, the new Rust tests that pin it fail; with `with_d46_probe`
switched off, the new Python tests fail. Not run, as the brief asks: `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`.

## Not verified

- Runtimes 16 and 17, so they keep the old probe. The decision to move them is for a measurement on them.
- A real run through a worker with a command filter. Nothing here checks what a filter does with the new command.
- The earlier 666-round comparison, and the up-to-one-poll-earlier cases.
- That `print user/foreground` is as reliable as `dumpstate` on an overloaded host. It is a smaller request of the same daemon.

## Incidental bugs

- **`dumpstate` may be cut at 20 MiB (inference).** One of 356 outputs in the earlier measurement was exactly 20,971,520 bytes, a round
  number that looks like a limit of the host's capture and not of `launchctl`. A cut output could drop the marker line and read as not
  running. This still matters on runtimes 16 and 17, which keep `dumpstate`, and no longer on 18 and later. Not checked, since no
  runtime below 18 is installed here, and left alone.
- **`boot_failure` still keeps the whole probe output in memory** to read one line (x17's follow-up). With the smaller probe on 18 and later
  that is 100 KB per poll; on 16 and 17 it is as before.
