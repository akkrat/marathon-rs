# X46: Apple transfers stop when their call is cancelled or their device's connection is lost (D54)

x45 measured that a worker frozen while remote parsing sends the test bundle to it is not caught within
`deviceInitializationTimeoutMillis`: keep-alive drops the simulator's own connection at about 75 s but the parse doesn't
notice, the 180 s limit cannot end the send, D31 ends it at 305 s, and the run fails at about 311 s (A104). D54 makes every
iOS and macOS transfer, send or receive, take its call's cancellation and the loss of its device's own connection. Approved
2026-11-08 by the project owner.

**Result.** With the default keep-alive the frozen worker's attempt fails at 75.2 s and D53's next attempt parses on the
healthy worker: the run exits 0 at 91.5 s. With keep-alive off the 180 s limit ends the parse at 180.1 s, and the run exits 1 with `Timed out waiting for a temporary
device for remote test parsing` at 210.9 s, after disposing the frozen simulator has waited 30 s on its connection. Before
D54 these were exit 1 at 311.6 s and 346.3 s.

## Design

Nothing reaches a worker differently: the rsync and ssh command lines and the commands sent to workers are unchanged, and
`--rsh` gets no `ConnectTimeout` or `ServerAliveInterval` (x26 rejected both; a frozen worker hangs ssh on its banner, where
`ServerAliveInterval` never starts).

- **`TransferStop`** (`crates/marathon-apple/src/cmd/mod.rs:219`): the call's `CancellationSignal`, and optionally a future
  that is ready with a `Failure` once the device's connection is lost. `now()` (`:252`) says whether a stop has already
  come, without waiting; `stopped()` (`:268`) waits for one. A cancellation comes first if both have.
- **`FileBridge::send_with` / `receive_with`** (`cmd/mod.rs`, in the trait): `send`/`receive` with a stop. The default
  ignores the stop and calls the plain method, which is right for the local copy bridge (`JvmFileBridge`) and for test
  fakes. `RsyncFileBridge` implements them, and its plain `send`/`receive` call them with `TransferStop::never()`.
- **`CommandExecutor::connection_lost`** (`cmd/mod.rs:132`): a future ready with the cause once the connection is lost,
  never for one that cannot be lost (the default, so local executors and every fake) or that we closed ourselves.
  `SshCommandExecutor` (`cmd/remote/ssh/sshj/executor.rs:315`) waits on `ConnectionShared::wait_lost`
  (`cmd/remote/ssh/sshj/client.rs:117`): the transport's death, which keep-alive (`keep_alive.rs:65`), a disconnect from the
  worker or a transport error records, unless `closed_locally` says it was our own `close()`.
- **`transfer_stop(executor, context)`** (`cmd/mod.rs:290`): the stop the devices use. A loss fails the transfer with
  `ConnectionException: Lost the connection to <worker> during a transfer`, caused by the loss.
- **Devices.** The iOS simulator (`crates/marathon-ios/src/apple_simulator_device/worker.rs:56`, used by `push_file`,
  `pull_file`, `push_folder`, `pull_folder` at `:225-255`) and the Mac (`crates/marathon-macos/src/device.rs:490`, used at
  `:839-869`) pass `transfer_stop` with their own executor. `push_shared` and `push_generated_file` go through those.
- **The screenshot pull is the one exception** (`worker.rs:346`): it gets a context whose cancellation never fires, so a
  lost connection still ends it but a cancelled capture does not. The capture is cancelled at the end of every test
  (`listener/screenshot.rs`), and the core loop drops a frame whose `screenshot` call returns cancelled, so a healthy run
  would have lost its last frame, or its only one and with it the GIF. Kotlin's pull finishes, and so does this one.

### In the rsync bridge

`crates/marathon-apple/src/cmd/remote/rsync/rsync_file_bridge.rs`:

- `send_with` (`:919`) waits for the bridge's per-worker send lock or a stop, whichever comes first (`:927`). A send stopped
  while it waits leaves the queue at once.
- `transfer` checks `stop.now()` before it starts rsync (`:493`), so a stop that has already come starts no process.
- `process` has a fourth branch in its `select!` (`:345`), after the child's exit and D31/D43's watch, so an rsync that has
  already exited keeps its own outcome. On a stop it calls `stop_child` with `grace: false` (`:460`): D31's sequence without
  its first 5 s, which are for an rsync that has reported a stall and is on its way out. So: list the descendants, TERM to
  rsync and to what it started, 5 s, KILL. Then `Descendants::end` runs as after a stall: TERM to what is left, 5 s, KILL.
  An rsync that exited 0 all the same keeps its success. A stopped transfer is never a stall, so it records nothing in
  D31's `StallRecord`.
- `stopped` (`:551`) turns the stop into the outcome. A cancellation returns `Err(Cancelled)` and logs nothing; the caller
  reports its own cancellation, as before. A loss logs one ERROR line with the loss as its cause and fails a send with the
  loss; a receive returns `false`, which is what D43 gives a failed pull and what its listeners deal with.

**Queued transfers.** Behind a stopped send in the bridge's lock, the next send runs: only a stall records anything that
fails who waited (U163). Behind it in `SharedTransfers` (`cmd/shared_transfers.rs`), `previous` was cleared before the
transfer and is set only on success, and only a `TransferStalled` error is recorded, so the next caller transfers. Neither
needed a change; both are pinned by tests below.

**Why a receive returns `false` on a loss and not an error.** The brief asks for an ordinary failure, not a cancellation, so
that D53 counts the device as tried: that is about sends, which are what parsing does. For a pull the bridge's ordinary
failure has always been `false` with an ERROR line (Kotlin's, and D43's), and every listener handles it. An error there would
take paths that no pull failure takes today. A send fails with an error rather than `false`, so that the message says what
happened; both count as failures everywhere.

## The signal paths

**Cancellation.** The `VendorContext` each caller passes now reaches rsync. An audit of every caller, by reading:

| Caller | Its cancellation | When it fires during a transfer |
|---|---|---|
| Remote parsing: `nm_test_parser.rs:79`, `xctest_parser.rs:202` and the installer it runs | The parse's scope, `marathon.rs:624` | `deviceInitializationTimeoutMillis`, D52 |
| iOS prepare: installer (`apple_application_installer.rs:186,270`), `test_root_factory.rs:224,274,449,578`, media importer | The retained prepare's `LinkedSignal` (`prepare.rs:150,157`) | Prepare cancelled: device lost or pool ended (D48), termination, D52; or the sibling branch failed |
| macOS prepare | `self.context()`, a signal nothing cancels | Never: unchanged |
| iOS and macOS batch: video, device log, macOS result bundle pulls, test root pushes | The batch job's signal (`execution.rs:50`; macOS `device.rs:647`) | Batch cancelled (D52, device actor terminated, D30's session close on the way) or disposal during a batch |
| iOS result bundle pulls | The artifact worker's signal (`artifact_processor/mod.rs:582`) | `close_and_join`, after the 300 s drain |
| iOS screenshot pulls | Excluded, see above | |

No caller transfers under a context it has already cancelled and expects the transfer to happen. The test runner's
"finally" runs listeners under the cancelled batch context, but each listener fails on an earlier command that checks
cancellation (`xcresulttool` via `safe_execute`, `kill`, `stop`, `healthy`) before it reaches a pull; disposal uses fresh
signals and makes no transfers.

**Nothing waits longer.** Each of these used to leave rsync running to its own end while its caller was cancelled, and now
stops it at once; the stop itself takes at most D31's 5 s + 5 s, plus `ps`.

**What else changes, all of it from ending sooner**, and only when a batch or the artifact queue is cancelled mid-pull:

1. The pull in flight is lost: a result bundle, video or device log of that batch is not delivered, and a partial local
   file may be left. A video's attachment for that test is missing.
2. iOS, no video recorder: the device log pull is the last listener step before the result bundle is submitted. A cancel
   landing during that pull used to let it finish and the bundle be submitted; now the bundle is not submitted and
   `close_and_join` reports it as never handed to the queue. If the cancel came from disposal with the outer context live,
   the batch now ends as lost where it could end `Ok`. The device is leaving either way.
3. macOS result bundle, pulled inline: a cancel during it returns cancelled, with no "was not delivered" line, where the
   bundle used to be moved into place. From disposal with the outer context live, the execute returns lost (`device.rs:665`)
   where it could return `Ok`.
4. iOS result bundles still pulling when the artifact queue is cancelled, 300 s after the run: reported undelivered at once
   (`processing did not complete`), where one that finished within the next 60 s was delivered.

A healthy run cancels none of these. The callers of D52's cancel, D48 and D30 get the same errors and lines as before.

**Loss.** Keep-alive or the transport records the death in `ConnectionShared` → `wait_lost` → the executor's
`connection_lost` → `transfer_stop`'s loss future → `TransferStop::stopped` in the bridge's `select!`. The provider sees the
same death through `connected()` on its next 16 ms tick and re-provisions the simulator (D44) or skips the Mac (U171); the two
are independent, and the transfer usually ends first. During prepare or a batch the device actor is cancelled within
milliseconds anyway, so the loss changes little there: prepare's result is dropped on cancellation, and a batch whose push
fails with a `ConnectionException` is classified as lost (`execution.rs:226`), which is also what the xcodebuild session on
the same dead connection gives. During parsing there is no device actor, and this is what ends the attempt.

## Tests

Every test was checked against a temporary revert, as listed.

| Test | What it shows | Without the fix |
|---|---|---|
| `marathon-apple/tests/rsync_file_bridge.rs::a_send_cancelled_mid_transfer_stops_rsync_and_the_send_behind_it_goes_on_d54` | A send cancelled mid-transfer returns that cancellation within 4 s, logs nothing, leaves no rsync or ssh (the parent checks both pids), and the send queued behind it runs | Stop branch removed from `process`: times out ("the stop did not end the transfer") |
| `…::a_cancelled_send_whose_rsync_ignores_term_is_killed_d54` | An rsync that ignores TERM is killed: returns after 5 to 16 s | Same |
| `…::a_receive_cancelled_mid_transfer_stops_rsync_d54`, `…::a_cancelled_receive_whose_rsync_ignores_term_is_killed_d54` | The same for a receive | Same |
| `…::a_send_whose_connection_is_lost_fails_stops_rsync_and_the_send_behind_it_goes_on_d54` | A loss fails a send with the loss, one ERROR line, the queued send runs | Same |
| `…::a_receive_whose_connection_is_lost_returns_false_and_stops_rsync_d54` | A loss leaves a receive `false`, one ERROR line | Same |
| `…::a_cancelled_send_under_sshpass_also_stops_rsync_d54` | Under `sshpass`, rsync is stopped too | Same |
| `…::a_transfer_stopped_before_it_starts_returns_its_stop_d54` | A stop that has already come returns at once | Pins behaviour; with the up-front check reverted the stop branch would return the same, so it does not isolate it |
| `marathon-apple/tests/shared_transfers.rs::a_cancelled_or_lost_send_lets_who_waited_behind_it_send_d54` | Behind a cancelled or lost send to the same destination, the next caller transfers | Pins existing behaviour (only a stall is recorded) |
| `marathon-apple/tests/ssh_session.rs::the_connection_loss_a_transfer_waits_for_is_keep_alives_and_not_a_close_d54` | Against the fake worker's real SSH: keep-alive's loss is ready with its cause; our own `close()` is not a loss | Default `connection_lost` kept: times out. `closed_locally` check removed: "our own close is not a loss" fails |
| `marathon-ios/tests/simulator_prepare.rs::a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54` | A simulator's push in flight fails with `ConnectionException: Lost the connection to local during a transfer`, caused by keep-alive's text, not cancelled; a pull after it is `false` | Device passes no stop, or only the cancellation: times out |
| `marathon-ios/tests/simulator_prepare.rs::the_parses_limit_ends_a_hung_send_at_the_limit_d54` | Paused time: a `with_timeout_or_null(180_000)` around a hung `push_shared` returns its own expiry at exactly 180 s | Device passes no stop: times out |
| `marathon-ios/tests/simulator_prepare.rs::a_screenshots_pull_finishes_when_its_capture_is_cancelled_d54` | Under a cancelled context, the screenshot's pull gets no stop, any other pull does | Exclusion removed: `[true, true]` |
| `marathon-macos/tests/batch.rs::a_lost_connection_fails_a_transfer_in_flight_as_an_ordinary_error_d54` | A Mac's push fails on its loss as above, a pull is `false`, and a cancelled push returns the cancellation | Device passes only the cancellation: times out |

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
  death (not seen) would be `!connected()` to the provider but would not stop a transfer; the transfer then keeps the
  caller's cancellation and D31/D43.
- A frozen worker with keep-alive off still costs the whole `deviceInitializationTimeoutMillis`, and the parse gets no
  second attempt, since a cancellation ends `withRetry` (Kotlin's). To give it one, keep keep-alive on, or raise the limit
  above about 320 s so that D31's stall ends the first attempt.
- On a lost connection the parser still makes its three attempts on that device, as in Kotlin for any ordinary failure. They
  fail within a millisecond, and log three `send error` lines and the `mkdir` errors.
- A transfer waiting in `SharedTransfers`'s per-destination lock does not see its own stop until the transfer ahead of it
  ends. That one now ends on its own stop, so the wait is no longer than before.
- The local bridge, for a local worker, is a copy that is not stopped.
- Not run: Linux, rsync 3, a real worker or Mac, a worker frozen with the session already up, `scripts/check.sh`, the remote
  check and the workspace suite (the brief's gate rule).

## Incidental bugs

None found in the product. Three documentation statements that x45 found wrong are corrected: D53's **Limit**, D31's parsing
bullet and the x26 table row "Stall during remote parsing".
