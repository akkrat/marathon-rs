# x25 — final documentation pass

This pass changed documentation only. It corrected statements about limits that the code does not support, and recorded
the open rows an interaction audit of D21–D42 found. This file keeps the evidence for those statements in one place.

## Figures reported by the interaction audit

The audit's logs are not in this repository, so these figures were not reproduced in this pass. The documents that quote
them say what they rest on.

| Claim | Figure | Used in |
|---|---|---|
| D30: time from TERM to the exit of xcodebuild | 5–16 ms on Xcode 27.2 with iOS 18.6, 26.4 and 27.0 simulators; at most 3.06 s with parallel clones; 46 runs; one Mac only | D30 |
| D30: a UI test's application under test after xcodebuild has ended | Stays running for at least 90 s, until the next run | D30 |
| D38: a failed readiness probe on the last device | The run ends after about 6–13 minutes with `remaining tests will not run` | D38 |
| D38: only the probe hangs and needs KILL | About 33 minutes | D38 |
| D38: every command hangs | About 50 minutes | D38 |
| U160: a batch held at D22's gate, quota 0 | 4 of 4 devices on the fake adb lost a batch that never started | U160 |
| U161: tests bounce after SIGINT with `debug: true` | A zero-time loop until exit, with false INCOMPLETE results | U161 |

The 26 to 30 minute figure for D38 (four setups of two boot waits each) is in [x17-findings.md](x17-findings.md).

## Checked against the code in this pass

- D27: `crates/marathon-android/src/device.rs` no longer runs `rm -r $EXTERNAL_STORAGE` or `mkdir` on it; only
  `echo $EXTERNAL_STORAGE` remains.
- D21: the unit is milliseconds (`crates/marathon-adb/src/client/transport.rs:56`), the default is 30 s, and four requests keep
  the limit: `host:version`, a device's features, `getprop` after boot and reverse port-forward removal at dispose
  ([x12-findings.md](x12-findings.md)).
- D31 and D32: `--timeout=300` goes on sends only, through the program `RSYNC4J_RSYNC` names; the worker's command is
  `env NSUnbufferedIO=YES xcrun xcodebuild test-without-building …`.
- D31 chain: `with_retry(30, 10_000)` at `actor.rs:394` makes 30 attempts with 29 waits of 10 s; `with_retry_send(3, 1000)` at
  `apple_application_installer.rs:182` makes 3 sends with 2 waits of 1 s. That is 90 sends, 27,000 s of stall and 350 s of
  waits, 27,350 s or 7.6 hours. At 60 s per send (keep-alive 15 s × 4) it is 90 × 60 s + 350 s, 1.6 hours.
  `shared_transfers.rs:49-60` holds a lock per worker and destination for the whole transfer.
- D41: `BLIPS_PER_DEVICE = 5` (`provider.rs:845`), counted per serial in each adb server's tracker and never reset (`tracker.rs`).
- Quota: `uncompletedTestRetryQuota` defaults to `i32::MAX` (`crates/marathon-config/src/bind.rs:924`), the schema has no run
  deadline, and a device loss is charged to the quota through `on_return_batch` and `handle_uncompleted_tests`
  (`queue.rs:416-431,466`). `maxBatchesWithoutProgress` is an iOS setting and defaults to 0, which is off.
- D36: the 16 ms loop is `source.rs:131-136` for registered devices and `:162` for dynamic ones, and Kotlin has the same loop
  (`AppleSimulatorProvider.kt:215-218,267-272`).
- D39: a no-progress timeout calls `on_batch_completed` (`execution.rs:202-209`, `batch_failure_policy.rs:55-58`), which resets
  the failure count and counts only progress.
- Graphite: write and shutdown errors become `GraphiteException` (`graphite.rs:60-72`), `?` passes them up
  (`progress.rs:356-364`), and an error from `receive` ends the actor loop (`executor/actor.rs:135-160`). Kotlin's `PrintWriter`
  swallowed write errors; only a failed connect threw there.

## Where the notes for this pass disagreed with the code

- A missed D39 failure ends at `testOutputTimeoutMillis` (300 s by default), not at the batch timeout (1,800 s by default).
- The 16 ms is the pause between recovery sweeps, not the delay between attempts. Recovery after a lost connection has no
  pause and no limit.
- A dead link at 60 s per send costs about 1.6 hours per simulator, not 1.5.
- The setup idle timeout of U162 is raised from `fetch_props` (`device.rs:288,423`), not from `device.rs:407`.
- Several line numbers in the notes had moved; the rows quote the lines found.
- C6 is reachable (`count: 0` sharding with `executionStrategy` ALL_SUCCESS gives an empty shard that `reduce`
  throws on), so it is not marked unreachable. A11, A34, A38, A39, A40 and A42 are.
- Connect errors to Graphite, and the lack of a timeout, are the same as Kotlin. Only write and shutdown errors are new.

## Left for the release step

- The first sentence of `README.md` names 0.11.0 as the first public release.
- `CHANGELOG.md` Unreleased becomes the new version's section, with Evidence and Known limits carried forward.

## Real-emulator check of the merged Android fixes

Native release binary from `0d365807`, on a Linux KVM machine, API 36 with two emulators and API 33, emulator 37.1.11, adb 1.0.41, a private adb server on a high port: 41 runs in about 62 minutes.

| Check | Runs | Outcome |
|---|---|---|
| Controls: remote parsing, batch | 4 | Exit 0. 64 tests parsed, no failed attempt |
| `kill-server` during parsing | 9 | Exit 0, 64 of 64, one failed attempt retried after the server returned |
| `kill-server` during install, before parsing | 1 | Exit 0, 64 of 64 |
| adbd killed or `adb reconnect` during parsing | 5 | Exit 0, 64 of 64 |
| A server that can't return, during parsing | 1 | Exit 1 at 60.0 s with `NoDevicesException`, no hang |
| adbd killed or `adb reconnect` mid-test | 4 | Exit 0, all tests passed |
| `kill-server` during a test and between batches | 3 | Exit 0, all tests passed |
| Emulator killed, a second one healthy | 2 | Exit 0, retried on the survivor, disconnected 3.25 s after `offline` |
| Emulator killed, alone | 2 | Exit 1 at 12.5 and 14.4 s |
| `adb reboot`, two emulators | 1 | Exit 0, set up again |
| A stale folder at the file sync destination (U159), remote and local parser | 2 | Exit 0. The folder held only fresh files, at the right path, and a marker elsewhere survived. A control with `am instrument` against the stale folder fails, so the test detects staleness |
| Silent video on API 33, past 30 s; on API 36 | 3 | Exit 0. A 200 s test gave chunks of 180.3 s and 19.9 s, both pulled |

Not checked: a real USB or TCP device and reverse forwards; a partial list reaching the parser, since how far the list had got at each kill can't be seen from outside; D42's failing session code on a real runner; a leftover made by really aborting a run. A file sync push with no `pathRoot` goes to `/data/local/tmp`, not external storage. `FilePushTest#testExternalFolderPresent`, which `docs/wp/x12-findings.md` recorded failing, now passes, which fits U159, though no older build was compared.
