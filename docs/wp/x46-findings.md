# X46: an Apple send stops when its call is cancelled, and any transfer when its device's connection is lost (D54)

x45 measured that a worker frozen while remote parsing sends the test bundle to it is not caught within
`deviceInitializationTimeoutMillis`: keep-alive drops the simulator's own connection at about 75 s but the parse doesn't
notice, the 180 s limit cannot end the send, D31 ends it at 305 s, and the run fails at about 311 s (A104). D54 makes every
iOS and macOS send take its call's cancellation, and every transfer, send or receive, the loss of its device's own
connection. Approved 2026-11-08 by the project owner. Receives took the cancellation too until review round 1 (see the end).

**Result.** With the default keep-alive the frozen worker's attempt fails at about 75 s and D53's next attempt parses on the
healthy worker: the run exits 0 at about 91 s. With keep-alive off the 180 s limit ends the parse at 180 s, and the run exits
1 with `Timed out waiting for a temporary device for remote test parsing` at about 211 s, after disposing the frozen
simulator has waited 30 s on its connection. Before D54 these were exit 1 at 311.6 s and 346.3 s.

## Design

Nothing reaches a worker differently: the rsync and ssh command lines and the commands sent to workers are unchanged, and
`--rsh` gets no `ConnectTimeout` or `ServerAliveInterval` (x26 rejected both; a frozen worker hangs ssh on its banner, where
`ServerAliveInterval` never starts).

- **`TransferStop`** (`crates/marathon-apple/src/cmd/mod.rs:219`): a `CancellationSignal`, and optionally a future that is
  ready with a `Failure` once the device's connection is lost. `now()` (`:261`) says whether a stop has already come,
  without waiting; `stopped()` (`:277`) waits for one. A cancellation comes first if both have. `TransferStopped::into_error`
  (`:235`) is what a send returns for it.
- **`FileBridge::send_with` / `receive_with`** (`cmd/mod.rs`, in the trait): `send`/`receive` with a stop. The default
  ignores the stop and calls the plain method, which is right for the local copy bridge (`JvmFileBridge`) and for test
  fakes. `RsyncFileBridge` implements them, and its plain `send`/`receive` call them with `TransferStop::never()`.
- **`CommandExecutor::connection_lost`** (`cmd/mod.rs:132`): a future ready with the cause once the connection is lost,
  never for one that cannot be lost (the default, so local executors and every fake) or that we closed ourselves.
  `SshCommandExecutor` (`cmd/remote/ssh/sshj/executor.rs:315`) waits on `ConnectionShared::wait_lost`
  (`cmd/remote/ssh/sshj/client.rs:117`): the transport's death, which keep-alive (`keep_alive.rs:65`), a disconnect from the
  worker or a transport error records, unless `closed_locally` says it was our own `close()`.
- **`send_stop(executor, context)`** (`cmd/mod.rs:299`): the call's cancellation, or the loss, which fails the send with
  `ConnectionException: Lost the connection to <worker> during a transfer`, caused by the loss.
  **`receive_stop(executor)`** (`:305`): the loss only.
- **Devices.** The iOS simulator (`crates/marathon-ios/src/apple_simulator_device/worker.rs:56,60`; pushes at `:229,249`,
  pulls at `:239,259`) and the Mac (`crates/marathon-macos/src/device.rs:490,494`; pushes at `:843,863`, pulls at
  `:853,873`) build them from their own executor. `push_shared` also hands `send_stop` to `SharedTransfers` (iOS `:306`,
  macOS `:886`), and `push_generated_file` goes through `push_file`.
- **Why a receive takes no cancellation.** Every receive collects an artifact (result bundle, video, device log,
  screenshot), and its callers are cancelled around it on purpose: the screenshot capture at the end of every test, iOS's
  artifact queue 300 s after the run before its 60 s grace, D52 and D48 while a batch's listeners are pulling. Kotlin's pull
  finishes then, and so does this one. A receive hangs on a frozen worker no longer than before; the loss still ends it.

### In the rsync bridge

`crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs`:

- `send_with` (`:921`) waits for the bridge's per-worker send lock or its own stop, whichever comes first (`:929`).
- `transfer` checks `stop.now()` before it starts rsync (`:495`), so a stop that has already come starts no process.
- `process` has a fourth branch in its `select!` (`:345`), after the child's exit and D31/D43's watch, so an rsync that has
  already exited keeps its own outcome. On a stop it calls `stop_child` with `grace: false` (`:462`): D31's sequence without
  its first 5 s, which are for an rsync that has reported a stall and is on its way out. So: list the descendants, TERM to
  rsync and to what it started, 5 s, KILL. Then, once a stop has been sent (`:382`), `Descendants::end` runs as after a
  stall: TERM to what is left, 5 s, KILL, whatever rsync's status. An rsync that exited 0 all the same keeps its success.
  A stopped transfer is never a stall, so it records nothing in D31's `StallRecord`.
- `stopped` (`:553`) turns the stop into the outcome. A cancellation returns `Err(Cancelled)` and logs nothing; the caller
  reports its own cancellation, as before. A loss logs one ERROR line with the loss as its cause and fails a send with the
  loss; a receive returns `false`, which is what D43 gives a failed pull and what its listeners deal with.

**Waiting sends.** `SharedTransfers::send` (`cmd/shared_transfers.rs`) takes the caller's stop and races it against the
per-destination lock (`:77`), as `send_with` does for the per-worker one. A waiter stopped there returns its stop, with an
ERROR line on a loss; the send it waited for is untouched. Neither lock records anything for a cancellation or loss, so
what waits behind goes on: in the bridge only a stall records (U163); in `SharedTransfers` `previous` was cleared before
the transfer and is set only on success, and only `TransferStalled` is recorded.

**Why a receive returns `false` on a loss and not an error.** The brief asks for an ordinary failure, not a cancellation, so
that D53 counts the device as tried: that is about sends, which are what parsing does. For a pull the bridge's ordinary
failure has always been `false` with an ERROR line (Kotlin's, and D43's), and every listener handles it. A send fails with
an error rather than `false`, so that the message says what happened; both count as failures everywhere.

## The signal paths

**Cancellation, sends only.** The `VendorContext` each caller passes now reaches the send's rsync. An audit of every
caller, by reading:

| Caller | Its cancellation | When it fires during a send |
|---|---|---|
| Remote parsing: `nm_test_parser.rs:79`, `xctest_parser.rs:202` and the installer it runs | The parse's scope, `marathon.rs:624` | `deviceInitializationTimeoutMillis`, D52 |
| iOS prepare: installer (`apple_application_installer.rs:186,270`), `test_root_factory.rs:224,274,449,578`, media importer | The retained prepare's `LinkedSignal` (`prepare.rs:150,157`) | Prepare cancelled: device lost or pool ended (D48), termination, D52; or the sibling branch failed |
| macOS prepare | `self.context()`, a signal nothing cancels | Never: unchanged |
| iOS and macOS batch: test root pushes | The batch job's signal (`execution.rs:50`; macOS `device.rs:647`) | Batch cancelled (D52, device actor terminated) or disposal during a batch |

No caller sends under a context it has already cancelled and expects the send to happen; disposal uses fresh signals and
makes no transfers. Receives (video, device log, result bundles, screenshots) take no cancellation and are as before.

**Nothing waits longer, and nothing reports differently.** Each of these used to leave rsync running to its own end while
its caller was cancelled, and now stops it at once. The stop takes at most 15 s: 5 s for rsync to end after TERM before KILL,
5 s for pipes that a child it left still holds (U158), and 5 s for what it started to end after TERM before KILL; and up to
20 s more where `ps` is slow, since each of its four listings may take 5 s. A cancelled prepare's
result is dropped, a cancelled parse ends with its own cancellation, and a batch cancelled during a test root push used to
fail its next command on the same cancellation. A send stopped part-way leaves rsync's partial file, and the next send of it
transfers it again, since `SharedTransfers` keeps a source only after a success. A healthy run cancels no send.

**Loss, every transfer.** Keep-alive or the transport records the death in `ConnectionShared` → `wait_lost` → the executor's
`connection_lost` → the stop's loss future → `TransferStop::stopped` in the bridge's `select!` or `SharedTransfers`' wait. The
provider sees the same death through `connected()` on its next 16 ms tick and re-provisions the simulator (D44) or skips the
Mac (U171); the two are independent, and the transfer usually ends first. During prepare or a batch the device actor is
cancelled within milliseconds anyway: prepare's result is dropped on cancellation, and a batch whose push fails with a
`ConnectionException` is classified as lost (`execution.rs:226`), which is also what the xcodebuild session on the same dead
connection gives. A pull that ends `false` on a loss is what a pull on a dropped link already gave. During parsing there is no
device actor, and this is what ends the attempt.

## Tests

Every test was checked against a temporary revert, as listed. Round 1's changes are in the review section below.

| Test | What it shows | Without the fix |
|---|---|---|
| `marathon-apple/tests/rsync_file_bridge.rs::a_send_cancelled_mid_transfer_stops_rsync_at_once_and_the_send_behind_it_goes_on_d54` | A send cancelled mid-transfer returns that cancellation, logs nothing, leaves no rsync or ssh (the parent checks both pids), and the send queued behind it runs. On a clock whose delays take 30 times as long, so a 5 s step before TERM would take 150 s, past the 60 s hang guard | Stop branch removed: times out. Stop given D31's grace: times out |
| `…::a_cancelled_send_whose_rsync_ignores_term_is_killed_d54` | An rsync that ignores TERM is killed, at least 5 s later (real clock) | Stop branch removed: times out |
| `…::a_cancelled_send_whose_rsync_exits_0_on_term_still_ends_what_it_started_d54` | Round 1's witness: rsync exits 0 on TERM and keeps its success, and its child that ignores TERM is killed | Cleanup keyed on the kept outcome: survivors `["ssh-pid"]` |
| `…::a_send_whose_connection_is_lost_fails_stops_rsync_and_the_send_behind_it_goes_on_d54` | A loss fails a send with the loss, one ERROR line, the queued send runs (slow clock) | Stop branch removed or given grace: times out |
| `…::a_receive_whose_connection_is_lost_returns_false_and_stops_rsync_d54` | A loss leaves a receive `false`, one ERROR line (slow clock) | Same |
| `…::a_cancelled_send_under_sshpass_also_stops_rsync_d54` | Under `sshpass`, rsync is stopped too (slow clock) | Same |
| `…::a_transfer_stopped_before_it_starts_returns_its_stop_d54` | A stop that has already come returns at once | Pins behaviour; with the up-front check reverted the stop branch would return the same, so it does not isolate it |
| `marathon-apple/tests/shared_transfers.rs::a_cancelled_or_lost_send_lets_who_waited_behind_it_send_d54` | Behind a cancelled or lost send to the same destination, the next caller transfers | Pins existing behaviour (only a stall is recorded) |
| `marathon-apple/tests/shared_transfers.rs::a_shared_send_waiter_observes_its_own_timeout_d54` | Round 1's witness, paused time: behind a transfer that runs to 240 s, a waiter whose scope expires at 180 s returns its cancellation at 180 s, one whose connection is lost at 100 s returns the loss then with one ERROR line, the transfer ahead finishes at 240 s, and a waiter alongside then goes on | Lock wait without the stop: the lost waiter gets `Ok(true)` at 240 s |
| `marathon-apple/tests/ssh_session.rs::the_connection_loss_a_transfer_waits_for_is_keep_alives_and_not_a_close_d54` | Against the fake worker's real SSH: keep-alive's loss is ready with its cause; our own `close()` is not a loss | Default `connection_lost` kept: times out. `closed_locally` check removed: "our own close is not a loss" fails |
| `marathon-ios/tests/simulator_prepare.rs::a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54` | A simulator's push in flight fails with `ConnectionException: Lost the connection to local during a transfer`, caused by keep-alive's text, not cancelled; a pull after it is `false` | Device passes no stop, or only the cancellation: times out |
| `marathon-ios/tests/simulator_prepare.rs::the_parses_limit_ends_a_hung_send_at_the_limit_d54` | Paused time: a `with_timeout_or_null(180_000)` around a hung `push_shared` returns its own expiry at exactly 180 s | Device passes no stop: times out |
| `marathon-ios/tests/simulator_prepare.rs::a_receive_never_takes_its_callers_cancellation_but_a_send_does_d54` | Under a cancelled context, a screenshot's pull, `pull_file` and `pull_folder` get no stop, `push_folder` does | Not reverted in round 1; the screenshot exclusion it replaced failed with `[true, true]` when removed |
| `marathon-ios/tests/simulator_result_bundle.rs::review_healthy_artifact_pull_keeps_existing_shutdown_grace` | Round 1's witness, paused time: a result bundle pull that ends at 330 s, behind `close_and_join(300 s, 60 s)`, is delivered | `pull_folder` given the cancellation again: fails |
| `marathon-macos/tests/batch.rs::a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54` | A Mac's push fails on its loss as above, a pull is `false`, a cancelled push returns the cancellation, and a pull under a cancelled context runs until the loss | Device passes only the cancellation: times out |

The core/iOS test of the parse's limit is the iOS one above: core's `extract_remotely` was already cancellable, so a core test
with a fake parser would pass with or without D54. The real-clock tests below cover the whole path.

## The real-clock runs

`crates/marathon-cli/tests/ios_stalled_send.rs`, the x45 setup unchanged except that the stand-in rsync runs the real one in
the background and records `stopped` when the bridge's TERM reaches it. The tests are renamed, since their names stated the
old outcome: `a_worker_frozen_while_parsing_is_left_once_keep_alive_loses_it_d54` (was
`a_worker_frozen_while_parsing_with_default_keep_alive_x45`) and
`a_worker_frozen_while_parsing_with_keep_alive_off_fails_at_the_limit_d54` (was `…_with_keep_alive_off_x45`). Both are
`#[ignore]`d. Run by hand, in parallel, on this Mac with openrsync:
`caffeinate -i cargo test -p marathon-cli --test ios_stalled_send a_worker_frozen_while_parsing -- --ignored --nocapture --test-threads=2`.

Times are seconds after `Starting marathon` in the run's log, except the exit, which is on the test's clock (about 1 s
earlier).

**Default keep-alive:**

| Time | What happened |
|---|---|
| 0.26 | The frozen worker's simulator is set up |
| 0.42 | The parse finds the test binary and borrows that simulator, the only one set up; the bundle's send starts and freezes the worker |
| 10.29 | The healthy worker's simulator is set up; `Providing 2 devices` |
| 75.24 | `Dying because - Did not receive any keep-alive response for 60 seconds`; the provider starts re-provisioning the simulator (D44) |
| 75.32 | `send error: the device's connection was lost while sending to 127.0.0.1:/tmp/marathon/shared/marathon.xctest`, with `ConnectionException: Lost the connection to fixture@127.0.0.1:<port> during a transfer` caused by the keep-alive loss. The stand-in records `stopped` |
| 75.32 | The parser's two retries on the same simulator fail within 1 ms: `Unable to start a remote shell session [mkdir …]` twice, logged and ignored as in Kotlin, and the same `send error` line, each time. Three `send error` lines in all |
| 75.32 | D53's next attempt borrows the healthy simulator; its bundle's send starts |
| 80.41 | `Parsed 1 tests in 5101 ms` on the healthy simulator (5.1 s is its worker's `late_open` delay) |
| 80.41 to 90.64 | The healthy simulator's prepare: two sends, each 5.1 s for the same reason |
| 90.66 | test1 runs on the healthy simulator and passes. Exit 0 at 91.5 s on the test's clock |

**Keep-alive off** (same run, in parallel):

| Time | What happened |
|---|---|
| 0.26 to 10.29 | The same: the parse's send freezes the worker, and the healthy simulator is set up |
| 180 | `deviceInitializationTimeoutMillis` expires and cancels the send. The bridge stops rsync; the stand-in records `stopped` at 180.8 s on the test's clock. The cancellation ends the parser and core's `withRetry`, so there is no second attempt and the healthy worker gets no bundle |
| 180.10 | `Terminating AppleSimulatorProvider`: the run's finish, with the parse failed. The healthy simulator is disposed at once |
| 210.10 | Disposing the frozen simulator: `No reply to opening chan#12 in 30 s` on its connection, which nothing has closed since keep-alive is off, then `Exception shutting down simulator`, as in x45 |
| 210.11 | `NoDevicesException: Timed out waiting for a temporary device for remote test parsing`, `No devices found`. Exit 1 at 210.9 s on the test's clock |

The error line comes after the provider's termination, as the run reports its error once its finish is done; the parse
itself ended at 180 s.

Before D54 (x45): exit 1 at 311.6 s and 346.3 s.

## Android

Remote parsing's adb path does not have the flaw. `AmInstrumentTestParser::extract`
(`crates/marathon-android/src/remote_parser.rs:341`) passes the parse's signal down, and every adb request on its path runs
under `cancellable(signal, …)` (`crates/marathon-android/src/support.rs:74`), a `select!` that drops the request when the
signal fires: `request` and `bounded_request` (`device.rs:281-305`), the package install (`transfers.rs:440`), shell
commands (`device.rs:494-536`) and the instrumentation channel (`runner.rs:43-99`), which the attempt cancels and joins at its
end (`remote_parser.rs:268-275`). The adb client is async tokio I/O (`marathon-adb/src/client/transport.rs:28`), so dropping
a request closes its socket. The only blocking work is reading the APK locally. So the 180 s bounds an Android parse; no row
was added.

## Limits

- The loss signal is the transport's death as `ConnectionShared` records it. A russh handle that closed without reporting a
  death (not seen) would be `!connected()` to the provider but would not stop a transfer; a send then keeps its caller's
  cancellation, and either keeps D31/D43.
- A frozen worker with keep-alive off still costs the whole `deviceInitializationTimeoutMillis`, and the parse gets no
  second attempt, since a cancellation ends `withRetry` (Kotlin's). To give it one, keep keep-alive on, or raise the limit
  above about 320 s so that D31's stall ends the first attempt.
- On a lost connection the parser still makes its three attempts on that device, as in Kotlin for any ordinary failure. They
  fail within a millisecond, and log three `send error` lines and the `mkdir` errors.
- A receive on a frozen worker whose connection stays up (keep-alive off) still runs until D43's limit, whatever cancels
  its caller, as before D54; a stuck pull is bounded by D43, not by a cancellation.
- The local bridge, for a local worker, is a copy that is not stopped.
- Not run: Linux, rsync 3, a real worker or Mac, a worker frozen with the session already up, `scripts/check.sh`, the remote
  check and the workspace suite (the brief's gate rule).

## Incidental bugs

None found in the product. Three documentation statements that x45 found wrong are corrected: D53's **Limit**, D31's parsing
bullet and the x26 table row "Stall during remote parsing".

## Review round 1

Reviewer gpt-6.1-sol, five findings, all fixed on top of 453a8ac5.

| Finding | Fix | Test, and its check against a revert |
|---|---|---|
| S-x46-1 (major): a stopped rsync that exits 0 kept its success and also skipped ending what it started, so a TERM-resistant child survived | `process` keeps whether a stop was sent (`rsync_file_bridge.rs:382`) and runs `Descendants::end` on that, whatever the status; a success is still kept | `a_cancelled_send_whose_rsync_exits_0_on_term_still_ends_what_it_started_d54`, the reviewer's witness: with cleanup keyed on the kept outcome again, survivors `["ssh-pid"]` |
| S-x46-2 (major): receives took cancellation, so a healthy pull ending at 330 s, which used to finish in `close_and_join`'s 60 s after the 300 s drain, was discarded; D52, D48 and disposal also lost pulls in flight | The orchestrator's decision, the reviewer's: sends take cancellation and the connection loss; every receive takes only the loss (`send_stop` and `receive_stop`, `cmd/mod.rs:299,305`). The screenshot special case is gone, being the general rule now. The consequences listed in round 0 (lost pulls, bundles never submitted, batches ending as lost) no longer arise and are removed from D54, the CHANGELOG and the audit above. `docs/migration.md` only lists D54 under "need no action" and had none of them | `review_healthy_artifact_pull_keeps_existing_shutdown_grace` in `marathon-ios/tests/simulator_result_bundle.rs`, the reviewer's witness, on a bundle bridge that now honours a stop as the rsync bridge does: with `pull_folder` given the cancellation again, it fails. Also `a_receive_never_takes_its_callers_cancellation_but_a_send_does_d54` and the macOS test's new pull under a cancelled context |
| S-x46-3 (major): `SharedTransfers::send` waited for the per-destination lock before any stop existed, so a cancelled prepare waited behind another simulator's live transfer (240 s against a 180 s scope) | `SharedTransfers::send` takes the waiter's stop (`send_stop`, built by the device) and races it against the lock (`shared_transfers.rs:77`), as `send_with` already did for the per-worker lock. The waiter returns its stop, with an ERROR line on a loss; the holder is untouched and nothing is recorded. D54's "leaves the queue at once" is replaced by what holds: a waiting send gives up its wait on its own stop, and the send ahead and those behind go on | `a_shared_send_waiter_observes_its_own_timeout_d54`, the reviewer's witness and a loss case, paused time: with the plain lock wait, the lost waiter gets `Ok(true)` at 240 s |
| S-x46-4 (minor): `docs/testing.md` | The stop tests run on a clock whose delays take 30 times as long (`SlowClock`), so a 5 s step before TERM would take 150 s and fail the 60 s hang guard, instead of a 4 s real-time bound; checked by giving the stop D31's grace: those tests time out. TERM-resistant cases keep the real clock with only the lower bound (KILL at least 5 s after TERM). Every new hang guard is 60 s. The CLI bounds are now 60 to 240 s for the keep-alive case and 179 to 270 s for the limit, both apart from D31's 305 s | As listed |
| S-x46-5 (minor): the D53 bullet in the CHANGELOG and A103's status still said a stall decided after 285 s ends the parse | Both now say what D53's corrected limit says: keep-alive's loss at about 75 s with the defaults, the 180 s limit with keep-alive off. D54's claim on healthy runs now holds: a healthy run cancels no send, and receives are as before | |

The bridge-only tests of a cancelled receive were removed, since no device cancels a receive; the bridge still stops one
on a stop it is given.

**Runs.** `cargo fmt --check`, `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-test-support
-p marathon-cli --all-targets -- -D warnings`, and `cargo test` on those crates (123 test binaries, 990 passed, 0 failed, 33
ignored) all clean; the local name check clean. Not run, per the brief: `scripts/check.sh`, the remote check, the workspace
suite.

**The real-clock tests again**, once by hand under `caffeinate -i`, in parallel, after the fixes. Seconds after `Starting
marathon`; exits on the test's clock.

| | Default keep-alive | Keep-alive off |
|---|---|---|
| Healthy simulator set up, `Providing 2 devices` | 10.29 | 10.30 |
| Keep-alive loss, `Dying because …` | 75.25 | none |
| Frozen worker's send stopped | 75.31, three `send error: the device's connection was lost …` lines | 180 s, at the limit (stand-in: 180.8 s on the test's clock) |
| Healthy worker's bundle send starts | 75.3 (test's clock 76.1) | none |
| Parsed | 80.40, on the healthy simulator | none |
| test1 runs | 90.61, passes | none |
| Provider terminated | 90.62 | 180.10 |
| Frozen simulator's disposal | | `No reply to opening chan#12 in 30 s` at 210.10, then `NoDevicesException: Timed out waiting for a temporary device for remote test parsing` |
| Exit | 0 at 91.4 s | 1 at 210.8 s |

The same as round 0's runs to within 0.1 s.

## Review round 2

Reviewer gpt-6.1-sol: the product fixes hold; two minor findings, both fixed.

- **S-x46-6: the stop tests' stand-in could be stopped before its signal handling existed.** It published `ssh-pid`, which
  the test waited for, before its TERM trap, and its child's trap had no handshake. The reviewer's witness, a 200 ms delay
  before the trap, failed the exit-0 test with `Err(Cancelled)` instead of `Ok(true)`. Now the child sets up its handling
  and writes `ssh-ready`, the stand-in waits for that, sets up its own, and writes `ready`, and the test stops it only
  after `ready` (`stoppable_rsync` in `crates/marathon-apple/tests/rsync_file_bridge.rs`). Checked: with a 200 ms delay
  before each trap, in the stand-in and in its child, all seven D54 bridge tests pass; the same delay on round 1's stand-in
  fails the exit-0 test with the reviewer's `Err(Cancelled)`. The delay was then removed. The other stand-ins: the
  `sshpass` stand-in already set its trap before starting rsync, and the real-clock CLI tests' stand-in logged `start`
  before its trap; nothing waits on that line, since the stops come at 75 s and 180 s, but the trap now comes first too.
  Those two CLI tests were not run again for this ordering change; they compile and pass clippy.
- **S-x46-7: the cleanup bound was understated.** "At most D31's 5 s + 5 s, plus `ps`" left out the wait for output pipes.
  When rsync and its child both ignore TERM, the bridge waits 5 s for rsync, 5 s for the pipes the child holds, then 5 s
  for the child; the reviewer measured 15.35 s. The findings and D54 now say at most 15 s, and up to 20 s more where `ps`
  is slow (four listings of up to 5 s each: before the TERM, at it, and at each of the cleanup's TERM and KILL). The
  CHANGELOG states no bound and is unchanged.

**Runs.** `cargo fmt --check`, `cargo clippy -p marathon-apple -p marathon-cli --all-targets -- -D warnings` and the local
name check: clean. `cargo test -p marathon-apple`: 57 test binaries, 546 passed, 0 failed, 26 ignored. Its
`rsync_file_bridge` tests three more times: 55 passed each time, in 38.7 s, 38.0 s and 38.0 s.
