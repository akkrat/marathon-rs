# X9: successful SSH commands parsed without their stdout (U145)

U145 had two problems. A (lost output) is fixed as deviation D19, approved by the user on 2026-11-03 (PLAN §3.11). B
(the run waiting after the setup failure) is Kotlin's behaviour and is unchanged. The full evidence is in the U145 row
of [found-bugs.md](../found-bugs.md).

## A: lost output (D19)

The loss came from a check-then-act race in the reader loop, shared with Kotlin (`Reader.kt:56,92,98`). Each poll reads
the stream first. If it read nothing, it then evaluates `can_read`:

- over SSH, `is_open && !is_eof`;
- locally, the process is alive and has not exited.

When the command's whole output and then its EOF or exit arrive between those two steps, the loop ends with the bytes
still buffered. The command then completes with its exit status and an empty or truncated output.

The transport is not at fault:

- russh queues Data, EOF and exit-status on one channel queue, in wire order.
- The fake worker sends its data before exit-status, EOF and close.
- The client drops only data that arrives after EOF, and none does here.

**Fix** (`crates/marathon-apple/src/extensions/reader.rs`). Once `can_read` is false, the reader reads what is still
buffered, until a read finds nothing, and only then drains its line buffer. That last read follows the loop's own rules:
it stops on cancellation or on a closed output channel, and stops at a read error as the main loop does. Unlike a normal
read, it does not update the shared last-output time or run the idle check, because the loop is ending.

Nothing else changes, and the fix needed no other change:

- Polling and backoff are unchanged.
- Idle expiry still closes the output before the drain, so the partial tail is still discarded (A24).
- Drain collectors stay detached (A26).
- The reader still polls past pipe EOF while `can_read` is true (A25).
- Error and cancellation ordering are unchanged.

The poll was moved into a helper, `read_now`, so the loop and the last read share it.

**Tests and evidence:**

- `crates/marathon-apple/tests/reader.rs::output_arriving_with_eof_between_an_empty_read_and_can_read_is_still_delivered`
  writes the output inside the `can_read` call, so the order is fixed by the test, not by timing.
  - On the pre-D19 reader it fails with `left: []`.
  - With D19 it receives both lines.
- `tools/oracle/reader-race-probe` (`expected.txt`) runs Kotlin's own `produceLinesManually` with the same arrival
  order. Kotlin returns `[]`; the control case keeps both lines.

## B: the run waits after the setup failure

The run is not stuck. It waits for a device for `deviceInitializationTimeoutMillis` (default 180 s), as Kotlin's
`Marathon.kt:131-143` does. The harness watchdog (60 s) fires first.

To check this locally, the fake worker's `env` reply was changed to leave out `HOME` (a temporary edit, not
committed) and the timeout was set to 5 s. The CLI then logged `Failed to set up simulator on Ssh(...)` with the
`DeviceSetupException` and exited 1 with `NoDevicesException` after 5 s.

## Incidental bugs

- The differential CLI runs on replay hooks, which are not live. Without the output pump, vendor log records wait for
  the next serial flush, so a run killed by the watchdog leaves no console output after provider initialisation. That
  made U145's evidence look like a silent hang. This is a harness observability limit only: live runs print records at
  once. Nothing was changed.
