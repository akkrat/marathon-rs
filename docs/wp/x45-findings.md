# X45: a worker frozen while remote parsing sends to it

> Superseded by [D54](../deviations.md#d54--an-apple-send-stops-when-its-call-is-cancelled-and-any-transfer-when-its-devices-connection-is-lost)
> (WP x46): a send now stops on its call's cancellation, and any transfer on its device's lost connection. The tests below were
> renamed and assert the new outcome; see the [x46 findings](x46-findings.md). This record is otherwise as measured.

A measurement, not a product change. The question: with the default settings, does a worker that freezes while remote
parsing sends to it make D53 move the parse to a healthy second worker within `deviceInitializationTimeoutMillis` (180 s)?

**Answer: no.** Keep-alive notices the frozen worker at about 75 s, but that only ends the borrowed simulator's own command
connection. The parse's send runs in rsync over its own ssh, which notices nothing, and nothing ties the lost simulator to
the parse that borrowed it. The first attempt ends only when D31 stops the send, 305 s after it started. By then the 180 s
scope has expired. D53 does pick the healthy worker for the second attempt, but that attempt runs under the expired scope
and fails at once. The run exits 1 with `Timed out waiting for a temporary device for remote test parsing` at about 311 s,
not at 180 s. With keep-alive off it fails the same way and exits at about 346 s. D53's text is not changed (see "What the
docs say" below).

## Setup

`crates/marathon-cli/tests/ios_stalled_send.rs`, two `#[ignore]`d real-clock tests on the built binary:
`a_worker_frozen_while_parsing_with_default_keep_alive_x45` and `a_worker_frozen_while_parsing_with_keep_alive_off_x45`.
Run by hand with `caffeinate -i cargo test -p marathon-cli --test ios_stalled_send a_worker_frozen_while_parsing -- --ignored --nocapture`.

- **Two fake iOS workers**, each with one simulator and one passing test, as in the other tests of that file. The Marathonfile
  sets neither `deviceInitializationTimeoutMillis` nor `keepAliveInterval`, so they keep their defaults: 180 s, and 15 s × 4.
  The second test sets `keepAliveInterval: PT0S`.
- **The frozen worker is a process of its own**, so that SIGSTOP stops it and not the test. It is the test binary itself,
  started on the helper `frozen_worker_process`, which does nothing unless the test sets `MARATHON_X45_FROZEN_ROOT`.
- **The freeze.** As in x26, a stand-in `RSYNC4J_RSYNC` sends `kill -STOP` to that process when the first rsync to the frozen
  worker starts, and then runs the real rsync. The worker's kernel still accepts connections, but nothing answers them.
  The stand-in logs when each rsync starts and ends.
- **Forcing the first borrow.** The binary has no hook for choosing a device: its live hooks pick at random. Remote parsing's
  borrow picks among the simulators set up at that moment, though, and waits only if there are none. So the healthy worker
  gets the fake worker's `late_open` fault: it answers the first session of every connection 5 s late, and its simulator is
  set up 10 s after the start. Each run has only the frozen simulator set up when the parse borrows, at about 0.4 s. Every
  run took the frozen worker first, as the tests assert. The delay also slows each send to the healthy worker by 5 s.

## Timelines

Times are seconds after `Starting marathon` in the run's log. The test's own clock starts about 1 s earlier, before the
process starts, and the exit times are from that clock.

**Default keep-alive** (run 3; run 1 is the same to within 0.3 s and exited 1 at 311.3 s):

| Time | What happened |
|---|---|
| 0.27 | The frozen worker's simulator is set up |
| 0.42 | The parse finds the test binary and borrows that simulator, the only one set up. The test bundle's send starts, and the worker is frozen as it does |
| 10.28 | The healthy worker's simulator is set up; `Providing 2 devices` |
| 75.24 | `Did not receive any keep-alive response for 60 seconds`: the simulator's own SSH connection dies, and the provider starts re-provisioning it (D44): three connects at 30 s each, then the worker retry at 205 s and three more. None of it touches the parse |
| 180 | `deviceInitializationTimeoutMillis` expires. Nothing is logged and the send goes on |
| 305.56 | `send error: no progress in 300 s sending to 127.0.0.1:/tmp/marathon/shared/marathon.xctest, rsync stopped: rsync(…): error: poll: timeout`. The attempt ends with `LeavePool` (U163), and D53 marks the frozen simulator as tried |
| 305.56 | The second attempt borrows the healthy simulator. The frozen one is not borrowable anyway while it is lost. Both `create remote directory` calls fail at once with `Timed out waiting for 180000 ms`, logged and ignored as in Kotlin |
| 310.64 | The test bundle reaches the healthy worker (5.1 s, the `late_open` delay), and the attempt then stops on the cancellation |
| 310.65 | `NoDevicesException: Timed out waiting for a temporary device for remote test parsing`. Exit 1 at 311.6 s on the test's clock |

**Keep-alive off** (run 3, run in parallel with the above):

| Time | What happened |
|---|---|
| 0.42 to 305.56 | The same, with no keep-alive message and no re-provisioning. Nothing notices the frozen worker until D31's limit |
| 305.56 to 310.64 | The same second attempt on the healthy worker |
| 310.26 | The healthy simulator's connection ends with `Broken transport; encountered EOF`, because the fake worker drops a connection that has been idle for 300 s (keep-alive normally prevents this). It is an artefact of the fake worker, and it did not change the result |
| 310.65 | The same `NoDevicesException` |
| 345.3 | Disposing the frozen simulator: `No reply to opening chan#12 in 30 s` on its frozen connection, which nothing has closed. Exit 1 at 346.3 s on the test's clock |

Sends, both cases: the version probe, one send to the frozen worker (the parse's bundle, stopped by D31), and one to the
healthy worker (the second attempt's bundle). There were no other sends, because no device actor ever started.

**Run 2 was discarded for timing.** Every process in it paused at once for about 39 s (a 30 s retry logged 69 s after the
previous one), which matches the Mac sleeping. The wall clock went on while libtest's monotonic clock did not: the run
measured 385 s with keep-alive off, against 346 s for the whole test. The order of events was the same as in run 3. Run 3
ran under `caffeinate`. Run 1 did not, but it had no such gap: the 30 s retries in its log are 30 s apart.

## Why the first attempt does not end before 180 s

By reading, at efebbb62:

1. **Core waits for the vendor call.** `extract_remotely` (`crates/marathon-core/src/marathon.rs:624`) runs
   `with_retry(3, 0)` inside `with_timeout_or_null`. Each attempt calls `parser.extract` through `call_vendor`
   (`crates/marathon-core/src/executor/bridge.rs:33`), whose join is protected: on cancellation it signals the vendor task and
   waits for it to return. So the 180 s limit can only end an attempt that is watching its cancellation signal.
2. **The send does not watch it.** `NmTestParser::parse_tests` (`crates/marathon-apple/src/nm_test_parser.rs:54`) calls
   `push_shared`, then `SharedTransfers::send`, then the simulator's `push_folder`
   (`crates/marathon-ios/src/apple_simulator_device/worker.rs:236`), which ignores its `VendorContext`. That calls
   `FileBridge::send` (`crates/marathon-apple/src/cmd/mod.rs:197`), which has no cancellation parameter. In
   `RsyncFileBridge::send` (`crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs:837`), only rsync's exit or
   D31's stall decision ends it.
3. **The lost simulator does not end the parse.** Keep-alive ends the simulator's command connection. The provider then
   re-provisions the simulator (`crates/marathon-ios/src/device/simulator_provider/recovery.rs:569`) and stops offering it
   (`borrowable`, `simulator_provider.rs:1052`). Neither has any link to a parse that already borrowed it. A device actor's
   prepare is cancelled when its device is lost (x26's 75 s), but during the parse there are no device actors yet.
4. **rsync's own ssh hangs.** `--rsh` is `ssh -o 'StrictHostKeyChecking no' -F /dev/null -i … -l … -p …`
   (`rsync_file_bridge.rs:250-269`): no `ConnectTimeout` and no `ServerAliveInterval`, and `-F /dev/null` keeps out any user
   configuration. The worker stopped before ssh connected, so the kernel put the connection in the listen backlog, and ssh
   waits for the server's banner indefinitely. Only openrsync's `--timeout=300` ends it, as `poll: timeout`. A worker that
   freezes mid-transfer, with the session already up, would also have no `ServerAlive`, and TCP keep-alive is off or takes
   two hours. That case was not run.
5. **The second attempt is Kotlin's too.** Kotlin's `withRetry` rethrows a `CancellationException`, but it waits with
   `delay(0)`, which returns at once without checking for cancellation. So after a failure that is not a cancellation, the
   next attempt starts even when the scope has expired, and its borrow does not suspend while a device is available. The
   port's `with_retry` does the same.

**Kotlin, by reading.** `RsyncFileBridge.kt:40-66` blocks in `monitor(rsync.builder())` inside a suspend function, and
`withTimeoutOrNull` (`Marathon.kt:131`) cannot interrupt it. Kotlin has no rsync limit either (A3), so in Kotlin a worker
frozen during the parse's send blocks the parse with no limit, whatever the keep-alive. The port ends it at 305 s because of
D31. This is recorded as A104 in `docs/known-kotlin-bugs.md`.

## The smallest change that would cover it

Not made; any of these would be a new deviation. The commands sent to workers would not change.

1. **Let a transfer be cancelled.** `FileBridge::send` and `receive` take a cancellation signal. When it fires, the
   `RsyncFileBridge` stops the transfer with D31's existing stop (TERM, then KILL 5 s later, to the processes the transfer
   started) and returns. With only this, using the parse's context, the 180 s limit would hold: the run would fail at about
   185 s instead of 311 s. D53 would still not help.
2. **End the attempt when its device is lost.** The iOS `push_folder` gives the bridge a signal that fires when the
   simulator's connection is lost as well as on the context's cancellation. A loss returns an ordinary failure rather than
   `Cancelled`, so `borrow_and_extract` marks the simulator as tried. The next attempt then borrows the healthy simulator at
   about 75 to 80 s, well within 180 s. This needs (1) to stop the abandoned rsync. By reading, the iOS provider does not
   join the bridge's retained tasks when it terminates, so without (1) a stale rsync would not delay the run's end. It would
   still hold the frozen worker's send lock until its own 300 s.

Two alternatives that do not cover it:

- `-o ServerAliveInterval` on `--rsh`. ssh starts those messages only once the session is up, so it would not end the hang
  measured here, which is on the banner. It would change `--rsh` on every transfer.
- `ConnectTimeout` would end the banner wait, but x26 rejected it for the reasons given there.

The workaround without a code change is unchanged: raise `deviceInitializationTimeoutMillis` above about 320 s. Then the
second attempt runs on the healthy worker within the scope, as x44's run with the limit at 1,200 s showed (passed at
296.5 s).

## What the docs say

- D53's **Limit** in `docs/deviations.md` says that with the default 180 s, a stall during the first attempt "still ends the
  parse with that timeout before a second attempt can start". The measurement shows that the timeout cannot end an attempt
  that is in a send: the run fails at about 311 s, not 180 s. A second attempt does start, on the healthy worker, and fails
  on the expired scope. The outcome D53 states, the run failing with the timeout's error, is right. The timing and the
  "before a second attempt" are not.
- x26's table row "Stall during remote parsing" says the 180 s limit "ends it before any 300 s can". That is wrong in the
  same way.
- D31's "While parsing … all of it stays within `deviceInitializationTimeoutMillis`" is wrong too: a stalled send ends the
  parse 305 s after it started, whatever the limit.

The brief says to change no deviation text when the answer is "not covered", so those three are left for the owner to
decide. A ledger row (A104) records the Kotlin behaviour behind them.

## What was verified, and how

- Both new tests ran by hand three times (run 1 default only; runs 2 and 3 both), all passing, on this Mac with openrsync.
  Run 2's timings are discarded, as above.
- `cargo fmt --check`, `cargo clippy -p marathon-cli --all-targets -- -D warnings`: clean.
- `cargo test -p marathon-cli --test ios_stalled_send`: the two tests that are not ignored pass. The refactor moved the
  Marathonfile into a helper, `files`, and `worker` into `worker_with`. The other ignored tests in the file were not run
  again; the text they write is the same as before.
- Not run: `scripts/check.sh`, the remote check, the workspace suite, Linux, or rsync 3. No product code changed.

## What was not verified

- A worker frozen mid-transfer, with the session up (point 4 above, by reading).
- A real packet-dropping link instead of SIGSTOP.
- Android and macOS: by reading, Android's parse runs instrumentation over adb rather than rsync, and macOS uses the same
  rsync bridge, so macOS should behave like iOS. Neither was measured.
- The random first borrow without `late_open`. In a real run, the parse borrows the simulator that is set up first.

## Incidental bugs

None in the product. The three documentation statements above are inaccurate and are left to the owner.
