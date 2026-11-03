# X9: successful SSH commands parsed without their stdout (U145)

Both problems behind U145 are Kotlin's behaviour. Nothing in the product is changed. Problem A is open as a parity
question for the user. The full evidence is in the U145 row of [found-bugs.md](../found-bugs.md).

## A: lost output

The loss comes from a check-then-act race in the reader loop that the port shares with Kotlin. Each poll reads the
stream first. If it read nothing, it then evaluates `can_read` (`is_open && !is_eof`). When the channel pump delivers
the command's whole stdout and then EOF between those two steps, the loop ends with the bytes still in the channel
buffer. The command then completes with its exit status and an empty stdout.

The transport is not at fault:

- russh queues Data, EOF and exit-status on one channel queue, in wire order.
- The fake worker sends its data before exit-status, EOF and close.
- The client drops only data that arrives after EOF, and none does here.

Witnesses:

- `tools/oracle/reader-race-probe`: Kotlin's own `produceLinesManually`, run from the built CLI distribution, loses
  both lines. The control case keeps them.
- `crates/marathon-apple/tests/reader.rs::output_arriving_with_eof_between_an_empty_read_and_can_read_is_never_read`:
  the same loss in Rust, made deterministic by gating on the `can_read` call. It asserts today's Kotlin-identical
  result.

The user has to decide between two options:

- Port the bug: add a known-kotlin-bugs row, and the witness stays as it is.
- Deviate with a new D19: once `can_read` turns false, read whatever is still buffered before draining, as the last
  step of the loop. The witness then expects both lines.

No stress run was made, because no product change was made. The rate seen in WP x8 was about 1 in 24 loaded macOS 14
workspace runs.

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
