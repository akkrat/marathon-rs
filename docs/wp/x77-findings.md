# WP x77: three misleading log lines from the 0.14.0-rc.1 real runs

Base: `573a962d` (tag 0.14.0-rc.1). Approved 2026-11-17 by the project owner. Logs only (PLAN §3.12.5): no D-row; result
files, exit codes, the pinned progress and summary text, worker commands and the accounting of batches are unchanged.

## 1. Android: "briefly offline; keeping the device" for a device that was disconnected

**Cause.** `AndroidDevice::execute` (`crates/marathon-android/src/device.rs`) picks "the device was briefly offline"
whenever this serial's grace counter moved during the batch. A batch whose `DeviceLost` comes in a grace waits for the
grace to end (D41). When the grace runs out, the provider sends the list without the device, waits until the core has
handled it (`mark_disconnected`, `Disconnected`) and only then releases the hold, so the batch wakes on a device that has
already been dropped, and still logged `Batch failed because the device was briefly offline; keeping the device`. The
line named no device.

**Fix.** The reason is chosen as before, and so is the return value (`BatchExecution`, which D70's accounting relies on).
Only the line changes:

- device dropped meanwhile (`is_disconnected()`, the D24 flag the provider sets before releasing the hold):
  `Batch on <serial> failed because the device went offline and did not return in time; it has been disconnected`, or
  `… because the device was lost; it has been disconnected` when the reason was not a grace (an adb outage that ended
  in a disconnect);
- otherwise `Batch on <serial> failed because <reason>; keeping the device`, as before plus the device's name.

`<serial>` is the marathon serial (`127.0.0.1:<port>:emulator-5554`), as the Android transfer and boot lines give it.
The provider's own `Device emulator-5580 did not return in time; disconnecting it` keeps the adb serial; that line is
not part of this WP.

**Witness.** `offline_grace_test::a_batch_failed_in_a_grace_that_ends_in_a_disconnect_says_so_x77` (driven clock, a
log-capturing runtime): a grace the device returns from gives the kept line with the name; a grace that runs out gives
the disconnected line only after the `Disconnected` event. It fails without the change at the first assertion (no
name) and, with only the disconnected arm disabled, at the second (`keeping the device`).

The whole-run tests in `reconnect_test.rs` could not reach this path: the fake runner's instrumentation either hangs (the
actor's terminate cancels the batch, no `DeviceLost`) or the batch waits at the gate before its next instrumentation.

## 2. Android: a pull cut off by an offline device blamed sync support

**Cause.** adb's sync client (adam, and `marathon-adb`'s port) reports a short read inside a sync reply as
`UnsupportedSyncProtocol` ("Unsupported sync: protocol response"). A transport that closes mid-reply, as it does when
the emulator goes `offline`, is such a short read, so `pull_file` mapped it to `Device … does not support sync: file
transfer`.

**Fix.** On `UnsupportedSyncProtocol` `pull_file` asks `AndroidDevice::lost_connection` (new, beside `loss_cause`): the
device has been disconnected, or `loss_cause` gives a passing loss (adb server away, device offline/connecting) or the
device has left the server's list. Then the message is `File transfer from device <serial> broke off because <reason>`.
A device the server still lists as `device` keeps `Device <serial> does not support sync: file transfer`. The error
kind stays `Transfer`, so the callers (`safe_pull_file`, `safe_pull_folder`) still log and ignore it. The question costs
one `host:devices` request, only after such a failure.

**Witness.** `device_test::a_pull_cut_off_by_an_offline_device_blames_the_connection_x77`: a RECV cut off with the
device still `device` keeps the sync text; a RECV that hangs until the device goes `offline` gives `… broke off because
the device is briefly offline`. Fails without the change (the old sync text).

To make this deterministic the fake adb server got a narrow script knob: a file's `recv_cut` (`close` or `hang`) sends
the first DATA header and then closes, or waits until the device's session is dropped
(`crates/marathon-adb/src/server/{script,world,sync}.rs`, documented in `docs/design/fake-adb-script.md`). `world.rs`
is an input of the vendor differential rows, so `tools/differential/registry.json` was regenerated with
`make_registry.py`; the diff is that file's hash only.

## 3. Core: a cancel during preparation logged at ERROR with its message twice

**Cause.** C36 (Kotlin `DeviceActor.kt:180–182`, ported in `crates/marathon-core/src/execution/device/actor.rs`
`initialize`): `catch (e: Exception)` also catches the initialize task's own cancellation and logs `Error <message>` at
ERROR with the same failure as the cause, hence `ERROR … Error Cancelled by a signal error=cancelled: Cancelled by a
signal`.

The iOS remote finding (`ERROR … Error Job was cancelled error=job cancelled: Job was cancelled` when a run ends while a
simulator is still booting) is the same path: the pool cancels the actor's job with `Job was cancelled`
(`pool.rs`/`scheduler.rs` `job_was_cancelled`), which cancels the initialize task during `prepare`.

**Fix.** When the retry loop ends in a cancellation and the initialize task's own job is cancelled, the actor logs
`WARN Device preparation has been cancelled` with the reason as the cause, as a cancel mid-batch logs `Device execution
has been cancelled`, then terminates as before. Every other outcome keeps Kotlin's ERROR line, including a vendor
`LeavePool` (not booted / not admitted), which arrives as a cancellation while the task's job is still active (oracle
row "a preparation that fails with a cancellation of its own…", unchanged).

**Witnesses.** `device_actor.rs`:
- `terminate_during_preparation_logs_the_task_cancellation_as_a_warning` (renamed from `…_as_an_error`, C36's witness);
- `cancellation_before_barrier_admission_leaves_the_token_unconsumed` (its expected line updated);
- new `parent_cancellation_during_preparation_is_one_warning_with_the_reason_as_its_cause`, for `Cancelled by a signal`
  and `Job was cancelled`: one WARN, the reason only as its cause, no ERROR.

All three fail without the change. `device_actor_oracle.rs` accepts the two JVM rows that hold this record ("terminate
during preparation", "retry delay cancelled at its deadline"): only an ERROR `Error DeferredCoroutine was cancelled`
with a `job-cancelled` throwable is mapped to the WARN line; the throwable is compared unchanged. This replaces x56's
wording map for the same record. The oracle recordings are not touched; no parity-matrix row changes (the renamed test is
not in the matrix). C36 in `docs/known-kotlin-bugs.md` is now `fixed by WP x77`.

## Result files

None of the three texts is stored anywhere but the log: the actor line goes to tracing only; the Android batch line is
a device log line, and the batch's outcome (`BatchExecution` with the original failure) is unchanged; the pull message
only reaches `Pulling … failed. Ignoring` (every pull goes through `safe_pull_file`/`safe_pull_folder`). No golden
changed, and the vendor differential (which compares the ordered console) passes.

## Verified

- `cargo fmt --check`; `cargo clippy -p marathon-adb -p marathon-android -p marathon-core --all-targets --all-features
  -- -D warnings`.
- `cargo test --no-fail-fast` for `marathon-adb`, `marathon-android`, `marathon-core` (all features), `marathon-cli`
  (end-to-end and differential suites, incl. `differential_vendors`), `marathon-ios`, `marathon-macos`,
  `marathon-apple`, `marathon-test-support`: all pass.
- `python3 -I tools/differential/make_registry.py --check`: passes after the regeneration.
- Each new or changed test failing without its fix, by temporarily reverting the product change (fix 1 also with only
  its disconnected arm disabled).

## Not verified

- No real device or simulator run: the texts are checked with the fake adb server, a driven clock and the actor's
  scripted vendor.
- `scripts/check.sh`, `scripts/remote-check.sh` and `cargo test --workspace` were not run (batch rule); the orchestrator
  runs the gates.

## Incidental bugs

- `marathon-core/tests/marathon_scenarios` and other differential-evidence tests fail with `missing YAML helper; run
  cargo build -q -p snakeyaml --example yaml_tree` in a fresh checkout until that example is built; a crate-scoped
  `cargo test` does not build it. Harness only.
- The batch's "briefly offline" reason is still chosen from the grace counter alone, so a `DeviceLost` unrelated to a
  grace that overlapped the batch is still attributed to the grace (the outcome is the same; only the reason in the
  line could be wrong). Not changed here.
