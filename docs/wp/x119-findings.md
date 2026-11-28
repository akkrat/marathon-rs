# WP x119 — Android recorder stop and artifact delivery

## Findings and changes

- **A (D102, N33).** The old listener stopped inline while consuming the instrumentation event. It ran a grep-based
  lookup, picked the last nonempty line, sent INT, and delayed 300 ms after every attempt, including the one that found
  no process. On API 36 the grep did not match itself, but all six published-build stops still logged the missing-process
  warning after a normal exit. The old fake probe recorded the separate wrong-PID case. The new stop admits no new
  chunks after taking the recorder-state lock, finds the exact process name once using the device's `ps` form, sends
  INT once, and checks that PID's `/proc` entry every 75 ms within a 1.5 s budget, including the recovery added in
  review round 1. An empty lookup does not prove that an in-flight launch never reached the device. An unconfirmed
  last chunk is not pulled or deleted, but earlier chunks with command completion can still be delivered. Multiple
  matching processes do not provide an exit receipt.
  There was no escalation in the old code. Below API 34 normal shell exits still roll to the next chunk, but a host
  recording-command timeout is not a shell exit and no longer rolls into another recording. No parser queue or
  provider teardown deadline was changed. D92 can still clean up an unconfirmed recording on the cleanup connection,
  and D70 still applies after result accounting. A withdrawal while the recorder stops before accounting can
  instead mark the ending test uncompleted (U286, below).
- **B (U273).** ADB STAT, DONE and writer completion can all agree on zero bytes or on an unfinished movie. Moved the
  U267 parser to `marathon-core` and kept the iOS module alias. Android uses the checker on the closed D97 stage, on a
  blocking worker, before rename. Rejection preserves an existing destination and the device copy and emits one
  `Video <path> was not delivered: …` line. Both per-test and batch-failure pulls use the check. It validates
  structure, not decodability. Withholding a zero-byte file changes the output file set even when reporting
  previously omitted its attachment.
- **C (U274).** Android formerly attached a planned GIF path even when the capture joined with no frames. It now
  requires a frame and a per-capture receipt from successful encoder finish, flush and close. A cancelled task's
  join status alone cannot provide that receipt. Zero-frame or failed captures are not attached. The healthy GIF
  path and the iOS caller are unchanged.
- **D (U275).** Both Android listeners used the iOS logging target. Each now uses its own Android module target.

## Emulator comparison

On an owned API 36 AVD, ran three selected tests from the existing sample, with identical video and screenshot
configurations across the published 0.15.6 and the native candidate. The same three tests passed in all six runs.
Wall time per test includes setup and teardown. Stop-to-delivery is the per-run median from `Stopping screen recorder`
to the completed ADB transfer log, excluding remote removal. Warn counts cover the whole run with debug logging.
Every published MP4 and GIF decoded at least one frame with `ffprobe -count_frames`.

| Run | Wall, s | Wall / test, s | Stop to delivery, ms | WARN | Playable / published |
|---|---:|---:|---:|---:|---:|
| 0.15.6 video | 26.06 | 8.69 | 662 | 3 | 3 / 3 |
| Candidate video | 29.23 | 9.74 | 242 | 0 | 3 / 3 |
| 0.15.6 video, warm pair | 26.41 | 8.80 | 658 | 3 | 3 / 3 |
| Candidate video, warm pair | 25.07 | 8.36 | 132 | 0 | 3 / 3 |
| 0.15.6 screenshot | 27.36 | 9.12 | — | 0 | 3 / 3 |
| Candidate screenshot | 25.49 | 8.50 | — | 0 | 3 / 3 |

Stop-to-delivery fell in both video pairs (by 420 and 526 ms). Total run time varied in both directions, so the
sample does not establish a whole-suite speedup. With 132 ms remaining in the warmed pair and single-digit
millisecond pulls there, a barriered queue does not look worthwhile for these local recordings. Revisit it only
if transfer/removal dominates a representative farm run. The owned AVD was stopped and deleted. No shared devices,
remote workers, or network settings were changed.

## Evidence and scope

The old JVM recordings remain unedited. The four recorder listener parity rows that explicitly expect the old
stop command are now `n/a` for D102. The focused transport comparator rewrites only the recorded empty-lookup
slot between a recorder launch and the first sync request; wrong or duplicate slots do not match. It still
checks every remaining request, attachment name, path and movie byte. The old host-timeout fake is adapted at
runtime to model a real first-chunk exit before a stop, and the native gated and timeout witnesses separately
check non-publication while a PID remains unconfirmed. No committed golden bytes changed. The incomplete-output
differences are covered by the rejected MP4 and GIF witnesses, as U267 covered iOS movies.

Five regression witnesses passed 50 repeats each and one more under CPU load. In a scratch worktree, accepting a
still-live PID made both D102 stop witnesses fail; bypassing the MP4 check, attaching the zero-frame GIF, and
restoring the iOS log target made their three corresponding witnesses fail. Formatting, targeted Clippy, the plan
graph, the release-source parity matrix and inventory, the differential evidence validators and release tests passed.

The real run did not cover API < 34, a device outage or an empty production recording. The fake ADB checks cover
both lookup forms, chunked recording, delayed exit, missing PID, bounded stop, D92 disposal, attachment disposition
and Android logging targets. PID lookup is scoped to one ADB-managed device, not to a second independent recorder
client on the same device. PID reuse during a stop or nonstandard process-listing formats were not exercised.

## Review round 1

- **R-x119-3 (fixed).** Android 7.1 toolbox prints an unlabelled state between PC and NAME. With WCHAN present, the
  former parser read the state as the name, skipped INT, logged a missing-recorder warning and withheld the video.
  The parser now takes PID by its header position and matches the final field exactly. The healthy-stop witness
  uses the [API 25 toolbox source's layout](https://github.com/aosp-mirror/platform_system_core/blob/android-7.1.2_r39/toolbox/ps.c#L184)
  with populated WCHAN and a near-name decoy, and a separate real API 36 toybox recorder row captured on an owned
  local emulator. The capture AVD was stopped and deleted. The normal lookup, INT and exit-probe sequence is
  unchanged, so the previous full emulator comparison was not repeated.
- **R-x119-2 (fixed, narrowed).** The old stop let an unconfirmed PID survive the end of the test. The fix sends one
  KILL to the already identified PID if exit is still unconfirmed after the graceful phase, including when the host
  shell probe timed out. Lookup, INT and polling have 1.25 s, leaving at most 250 ms for KILL and its own polling
  within the original 1.5 s stop budget. Only confirmed exit or an existing shell exit receipt can unlock the
  current chunk, which must still pass movie validation. If force-stop cannot confirm exit, the listener emits
  one WARN naming the PID and does not fetch or remove that chunk. D92 disposal keeps its independent best-effort
  INT on the cleanup connection. No cross-test or batch ownership was added.

The gated fake ADB tests prove no sync request occurs before exit. Ignoring INT exercises recovery and confirmed
delivery, and a host probe timeout exercises KILL followed by bounded, unconfirmed exit and no delivery. A
third case keeps answering alive even after KILL. The four recorder witnesses each passed 50 runs plus one under
CPU load. In a scratch copy of `77e417aa` with only the witness changes, all four failed: the legacy lookup
returned before probing, the timed-out probe ended without KILL, and the two persistent-PID cases saw no KILL.
The modern row is a positive control. No golden files changed. Formatting, Android Clippy, the plan graph, and
the three CLI differential and production-runner tests passed. The Android suite passed all 328 tests,
including 42 artifact tests and all 40 reconnect tests. The reconnect suite took about 250 s.

## Incidental bugs

The local Android crate sweep stalled in the reconnect suite, whose test configuration disables both recorders.
The isolated D85 reprovision case also did not finish on the clean base within a 35 s guard. The suite was stopped
locally; it does not provide a clean gate result. The Android artifact suite and D70 test-end and D92 cleanup
witnesses passed. No worker fault was injected.

- **R-x119-1 (deferred).** The reviewer gated the real video listener in the PID probe after instrumentation
  reported `one` passed, then withdrew the device. The snapshot marked both `one` and the unstarted `two` as
  uncompleted. The recorder ran before result accounting in 0.15.6 too, and the existing D70 witness gates a
  listener after accounting. The new stop sequence does not increase the worst-case exposure: 0.15.6 could
  sleep for five 300 ms intervals on top of up to ten shell commands, while x119 caps the stop at 1.5 s including
  the final KILL phase. The sampled healthy stops were shorter. No listener was reordered.
- **R-x119-4 (deferred).** The reviewer seeded the current screenshot report path with a valid earlier GIF,
  then reproduced its deletion by ending a capture with zero frames. Two tests whose escaped or truncated
  names collide can share that path. The capture staging and cleanup are unchanged in this round.
