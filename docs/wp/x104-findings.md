# x104 findings: D98's lines with debug off, and the refused recorder's name

A full iOS UI-test run with video on, on a private CI farm with Xcode 26.6 workers and `debug: false`, found three
problems with how D98 reports itself. x104 changes only logging and the supervisor's hint: detection, recording and
the end-of-run shutdown are as in 0.15.1. No new D-row; the D98 section of `docs/deviations.md` has an x104 note.

## Defects

- **U260.** D98's own lines never printed with `debug: false`. The busy warning logged on
  `marathon_ios::listener::video`, and the end-of-run shutdown lines (the INFO for a completed shutdown, the WARNs for
  one that did not shut down, failed, ran out of its window, had no time left, or was skipped after a signal, from the
  device's own release or the provider's termination) on the device's own target `marathon_ios::apple_simulator_device`.
  `marathon_logging::filter` caps the `marathon_ios` crate at ERROR with debug off, so a CI run saw neither, although
  the 0.15.1 notes promise one warning naming the simulator and one line reporting the shutdown.
- **U261.** The worker supervisor's `Worker recorder <token> failed (…). stderr: …` named only the recorder's random
  token. The farm run showed three of them at its start, and nobody could tell which simulator or worker was refused.
  This line was already visible with debug off: `marathon_apple::cmd::remote::ssh::sshj::supervisor` has a WARN floor
  (the new witness runs with debug off and sees it).
- **U262.** That warning's restart hint matched only "A recording is already in progress". Xcode 26.6's refusal is
  `Error Domain=NSPOSIXErrorDomain Code=16 "Resource busy" UserInfo={NSLocalizedFailureReason=Host recording is already
  in progress}`: D98's detection (`BUSY_TEXT` in `recording.rs`) still matched it through "Resource busy", the hint did
  not, and the docs quoted only the old wording.

## Changes

- **One target with a floor (U260).** `marathon_ios::busy_video` (`busy_video::TARGET`) carries the busy warning
  (`Recording::recorder_busy`) and every line of `shut_down_busy_video_by`, which both end-of-run paths use (the
  device's run-end disposal and, x102, `shut_down_busy_video_after_disposal` from the provider's termination). The
  device logs them through `log_busy_video`. `marathon_logging::filter` gives the target INFO with debug off and DEBUG
  with debug on, in every configurator, like the existing floors: INFO, not WARN, because the completed shutdown is an
  INFO line the notes promise too. Texts are unchanged. Other D98 diagnostics were checked: the failed movie's
  `Video … was not delivered` is ERROR and already visible; `started video recording` (U253) is DEBUG and not promised;
  the legacy recorder's warnings predate D98.
- **The recorder's subject (U261).** `CommandRequest` gets `worker_subject` (crate-private, like `detached_worker`).
  `SimctlService::open_recording` takes the UDID and sets `simulator <udid>`; the SSH executor passes it to
  `Supervisor::prepare`, which stores it on the `Lease`. `Peer` keeps its worker's `host:port`. `Peer::interpret`,
  which logs the warning for both the start probes and the control requests, takes the lease's subject:
  `Worker recorder <token> for simulator <udid> on worker <host>:<port> failed (…). stderr: …`. The warning names the
  UDID, which the supervisor knows from the command, and the worker by host and port as the supervisor's other
  warnings do; the device's serial (`udid@host-id`) is not known at that layer, and the UDID with the worker identifies
  the simulator as the batch-outcome warning does. A lease without a subject (an rsync sender) is named by its worker
  only; only recorders carry stderr receipts today.
- **One busy text (U262).** `reports_recording_busy` in `marathon_apple::bin::xcrun::simctl::service` (the simctl io
  service, next to `record_video`) is the single definition: "Resource busy" or "recording is already in progress",
  which covers both "A recording …" and Xcode 26.6's "Host recording …". D98's detection in `recording.rs` and the
  supervisor's hint both call it. For detection the change is a no-op on every text simctl is known to print (they all
  carry "Resource busy"); it widens only to a stderr with the Host wording and no "Resource busy".

## Witnesses and failure checks

Each check reverted only the named fix, ran the tests, and restored the file.

| Witness | Shows | Reverted | Failure |
| --- | --- | --- | --- |
| `busy_video_shows_info_and_warn_with_debug_off_u260` (`marathon-logging`) | the target's INFO and WARN pass with debug off (Apple, both outputs), DEBUG only with debug on; a sibling iOS target stays at ERROR | the floor in `filter` | fails, with the eight directive-inventory tests |
| `configurators_preserve_levels_and_compact_output` (`marathon-cli/tests/tracing_output.rs`) | through the real subscriber, an INFO on the target prints in every configurator; its DEBUG only with debug on | the floor | fails |
| D98 witnesses in `crates/marathon-ios/tests/ios_listeners.rs` | `turn_video_off` (used by every busy-simulator witness) switches the run to debug off; `busy_warnings` and `end_of_run_lines` assert each line's target. So the busy warning, the INFO shutdown, the skip after a signal, the no-time-left, did-not-shut-down and out-of-window WARNs, and the provider-termination shutdown are seen on `marathon_ios::busy_video` with debug off | the floor | 15 of the 20 `_d98` tests fail; of the 5 others, 4 reach no busy simulator and `a_busy_video_service_turns_video_off_for_the_simulator_once_d98` runs with debug on |
| the same | as above | the routing (target constant set back to the device's target) | 16 of 20 fail, including `a_busy_video_service_turns_video_off_for_the_simulator_once_d98`, which runs with debug on and fails on its target assertion |
| `a_refused_recorder_warning_names_the_simulator_and_worker_with_the_busy_hint_u261_u262` (`marathon-apple/tests/ssh_supervisor.rs`) | a recorder launched through `IoService::record_video` on a fake supervised worker whose receipt is status 16 with a given stderr; with debug off, each warning names `simulator fixture-udid` and `127.0.0.1:<port>`; the hint is present for Xcode 26.6's text, the earlier text, a bare "Resource busy" and each "… recording is already in progress" line alone, and absent for another refusal | the message (token only) | fails at the naming assertion |
| the same | as above | the subject plumbing (`open_recording` sets no subject) | fails at the naming assertion |
| the same | as above | the hint's match (back to "A recording is already in progress") | fails at the hint assertion on its first case, Xcode 26.6's text |
| `a_busy_video_service_turns_video_off_for_the_simulator_once_d98`, Xcode 26.6 case | the Xcode 26.6 refusal turns video off once | — | control: passes with the old `BUSY_TEXT` too, since that text carries "Resource busy"; it pins the shared definition against a regression |

The hung-shutdown witness switches debug back on after its setup: its check that no on-dispose lifecycle line appears
is on the device's ordinary target, which debug off would make vacuous. No sleep decides an outcome: the iOS witnesses
run in paused time, and the supervisor witness awaits the recorder's result before reading the warnings.

## Docs touched

- `docs/deviations.md`, D98: the busy text names Xcode 26.6's wording; an x104 note (lines with debug off, the
  supervisor warning's names, the shared text).
- `docs/migration.md`, D98 paragraph: Xcode 26.6's wording; the lines print with `debug: false`.
- `docs/design/logging.md`: the new floor in the list of specific-target exceptions, and a paragraph on the target.
- `CHANGELOG.md`: a `## Unreleased` section with one x104 bullet. The 0.15.1 section is unchanged: its quoted wording
  describes the 0.15.0 symptom, and its known limit (English text, "Resource busy" or "A recording is already in
  progress") is accurate for 0.15.1, which matched Xcode 26.6's text through "Resource busy".
- `docs/found-bugs.md`: U260–U262.

## Verification limits

- Not run on a real worker or the farm: the target and floor are pinned by unit, subscriber and virtual-time tests,
  and the supervisor warning by the fake SSH worker. The Xcode 26.6 text is taken from the farm's log.
- The supervisor witness's fake answers every control request with the failed receipt; the real worker reports a
  receipt after the recorder exits, which the existing D98 supervisor witnesses cover.
- A localized busy message is still not recognised (the 0.15.1 known limit).

## Incidental bugs

None found.
