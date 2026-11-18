# x89 Apple device timing failures (U212)

## Design

The Mac provider and Mac device used to return analytics clock and tracker failures from setup
and preparation. The iOS simulator factory and device had the same path through the core
`Track` helpers. An exhausted first timestamp could prevent the operation from starting. An
exhausted second timestamp or a failed tracker could discard its successful result.

All four sites now use `track_device_operation`. On a healthy operation the wrapper reads the
original start label, awaits the device body once, reads the same finish label and invokes the
original tracker. Serial lookup still follows setup or preparation. The Mac sites keep their
inline clock reads and iOS keeps its existing vendor-runtime hops. The wrapper catches panics
while creating or polling instrumentation futures. The vendor runtime reports panics in the
instrumentation it runs as task errors.

Missing either timestamp skips the submission. The wrapper attempts each submission once,
and a delegate that already accepted it keeps its copy. An instrumentation
failure logs one WARN and the operation keeps its own result. The body runs outside the
instrumentation catches, and a failed body runs no finish read or tracker. If successful work
encounters an instrumentation error while its owner is cancelled, the owner's cancellation
reason takes precedence. The existing cooperative cancellation and retained preparation
ownership still apply.

Replay hooks still record clock errors for end-of-tape validation. The iOS provisioning timer
used by the existing duration logs is separate from these analytics reads. Its established
failure and cleanup behaviour is covered by `post_setup_timing_failure_still_disposes_the_device`.
Android uses its own timing call sites and still calls the original `Tracker` methods.

## Verification

- The iOS and macOS `preparation_analytics` suites inject a failed first read, failed last read
  and tracker panic at each Apple site. Each checks one setup and preparation, admission,
  continued ownership of the device and no retry. Clock faults remain visible to
  `verify_consumed`. In a scratch worktree at `f7a02f59`, all twelve witnesses failed on the
  injected error or missing admission. All twelve passed with the fix.
- The core `preparation_analytics` witnesses verify one WARN per failure, including clock panics,
  tracker construction and polling panics, and cancellation originating inside the tracker.
  They also check a failing operation when the first timestamp fails and the owner's exact
  cancellation reason when a tracker returns a different reason. The fourteen new tests
  passed 50 parallel repetitions and one further run with four CPU workers.
- The scoped test runs covered core, iOS, macOS, test support, Apple, Android, analytics,
  and the CLI. The parallel run exposed a race in the warning witness's scoped tracing
  subscriber. The witness now captures warnings in its own test executable and passed
  the full changed-crate rerun.
- Formatting, Clippy for the four changed crates, the differential registry and evidence
  validators, the Apple evidence tests, the plan graph, source inventory and parity matrix
  checks passed.

## Contract and golden evidence

This enforces D51 for the Apple preparation metrics (U212). A successful device operation can
still admit the device when instrumentation fails. This uses the existing D51 decision. The
configuration, CLI, worker commands, result formats and pinned progress text follow the
existing rules. The CLI's `differential_vendors` suite matched all 16 native captures against
the committed JVM recordings, including the healthy iOS and macOS paths and the Android
controls. The golden diff is empty.

## Incidental bugs

None found. Physical Apple devices and the orchestrator's workspace and Linux gates remain
for integration.
