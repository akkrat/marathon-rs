# x99 simulator recorder start gate

WP x99 implements [D98](../deviations.md#d98--a-simulator-recorder-is-never-interrupted-before-it-has-started) and
fixes U251–U253 and, outside the legacy recorder, A109.

## The defect, reproduced

`xcrun simctl io <udid> recordVideo` prints `Note: No display specified…` (only without `--display`) and then
`Recording started` on **stderr**; stdout gets only `Recording completed. Writing to disk.` and `Wrote video to: …`
after INT. Checked on this Mac (Xcode 27, iOS 27.0 runtime) with disposable `mrs-x99-*` simulators:

- On a warm simulator the marker came 0.27–0.30 s after launch. INT before it ended simctl by SIGINT (wait status 2)
  with no movie and no lasting harm in 20 tries; INT after it finalized the movie (status 0).
- On a freshly booted simulator, INT 0.5 s after launch, after the display note but before the marker, ended simctl
  by SIGINT and left a movie file still being written by the video service. Every later `recordVideo` on that
  simulator exited **16** with `Error starting video recorder: … Code=16 "Resource busy" … A recording is already in
  progress`, with no `recordVideo` process left. Shutdown and boot cleared it. This matches the remote evidence.
- The same early INT sent through the 0.15.0 worker supervisor (`detach`, then token INT 0.5 s after launch on a
  fresh boot) wedged the simulator 4 of 4 times (`done:130` receipt, then status 16 on a probe recording).

## Paths, re-verified

| Path | Where the recorder runs | Before | x99 |
| --- | --- | --- | --- |
| a. SSH worker with Perl | `worker.pl` `run_owned`, detached, stderr piped to the supervisor | INT on the first wanted or forced loop once exec'd | INT only after the marker, held by the worker |
| b. SSH worker without Perl | `uses_legacy_recording()` is `has_worker_supervisor() && !worker_supervised()`; every SSH executor has a supervisor, so this **is** the legacy `nohup` path (d) | `kill -s INT $(cat pidfile)` | unchanged, see below |
| c. Local controller | `KotlinProcessCommandExecutor`, foreground, stderr visible | INT at the stop | INT only after the marker, read by `Recording` |
| d. Legacy (`nohup` + pidfile) | reachable on any Xcode whenever the Perl probe fails | as b | unchanged |

The brief listed b as a plain exec channel; no such path exists for video. A recorder over an SSH executor without a
supervisor would need a non-SSH executor that is not local, and there is none. macOS devices do not record video
(`MacosDevice::start_video_recording` is unsupported), so the hazard does not exist there.

The legacy recorder sends simctl's output to `/dev/null`, so neither the marker nor the busy code reaches marathon.
Gating it needs a changed launcher (stderr to a file next to the pidfile) and a changed stop command, which changes
worker commands that SSH command filters are told to allow (D96's migration action). That is left as an owner
decision; A109 records the Kotlin behaviour it keeps.

## Design and invariants

**One authority per path.** Whoever reads the recorder's stderr decides when INT may be sent.

- Worker (`worker.pl`): `drain_errors` splits the recorder's stderr into lines and sets `$confirmed` on a line that is
  exactly `Recording started` (an optional `\r` allowed); lookalikes such as ` Recording started` or
  `Recording started soon` do not count. The partial line is capped at 256 bytes; the 8 KiB diagnostic tail is
  unchanged. On confirmation the token state's `started` field goes from 2 to 3 under the token lock, and status and
  renewal replies say `recording:<expiry>` instead of `running:<expiry>`. INT is sent once `($wanted || $force)` and
  `$confirmed`. The escalation clock starts at the first stop request whether or not INT was sent, so TERM comes when
  TERM is wanted (or forced) and 2 s have passed since that first request, and KILL 1 s after TERM when forced: the
  0.15.0 times exactly. A recorder that exits on its own (the busy code) ends the loop through its status pipe as
  before.
- Local (`Recording::run_session`): for a video session without a worker report, `read_output` drains stdout and
  stderr to exit, as `await_result` did, and publishes the marker on a `watch`. After the stop is requested, the INT
  stage first waits for the marker or the exit, bounded by the INT cutoff through `wait_stage(…, 4)`, which follows
  window changes (a cancel during a healthy stop shortens it). On the marker INT goes out at once and the rest of the
  INT window is the finalization time. At the cutoff without a marker, INT is skipped and stage 5 sends TERM at that
  moment; KILL and the Unconfirmed end keep their cutoffs.
- SSH (`Lease`): `CommandSession::worker_start()` carries a `watch<bool>` fed from `recording:` replies; Rust sends INT
  at the stop as before and the worker holds it. Device logs have no marker and are never gated.

**Learning the start on SSH without slowing the healthy path.** A detached acknowledgement (data status 0) now wakes
the lease watcher, which sends one `status`. The first renewal stays at the configured interval
(`first_renewal`); before D98 the first request was at that interval too. While the recorder is acknowledged,
unconfirmed, not stopping and within 60 s of launch, the watcher polls `status` every 200 ms, and only over an open
control stream (`Peer::stream_ready`); without one it skips the request rather than dial or take a data session.
With the real worker the marker took 0.3–1.0 s, so a healthy start costs one to a few small requests on the existing
stream. The healthy-path CLI witness shows at most one extra `status` per recorder (none when the stop comes first)
and the same 21 s supervised run time as before.

Holding the detach acknowledgement until the marker was considered and rejected: it keeps the SSH session for the
start time, which x95b showed exhausts a ten-session worker, and it delays renewals and widens the "stop before
acknowledgement" window in which a seal sends token KILL.

**Recovery (ii).** A video session that exits with status 16 without having confirmed its start, while no other
launched video recorder of marathon's on that simulator is still live, sets `video_unavailable` on the simulator and
logs one WARN naming the simulator and the shutdown/boot commands. From then on `begin_capture` returns a recording
already finished as `Skipped`: nothing is launched or registered, `opened` fails, and the listener reports nothing
per test. A recording that was already waiting for its turn sees the flag after `ready` and also ends as `Skipped`.
The failed movie itself is still reported as undelivered. Only launched recorders count as marathon's own: the next
test's capture is registered while it waits for the previous one to finalize, and counting it lost the recovery
(found by the busy witness, fixed before review). The state lives on the device object, so a reconnected simulator
tries once more and warns once more.

Restarting the simulator (i) was not implemented. Marathon has no mid-run simulator restart in its lifecycle
machinery; it would need a new device state (stop admission, wait for the xcodebuild session and artifact pulls,
shut down, boot, repeat preparation steps) inside D93's command gate. That is an owner decision.

**D98, not a D96 note.** Skipping `recordVideo` after the busy code changes the commands sent to a worker
(§3.12 item 4) in that scenario, so it needs a D-row. Result files do not change: a busy simulator produced no movie
before either. Test outcomes, retries and the exit status are unchanged. No configuration field was added.

## Witnesses

Each was shown to fail on a scratch worktree with the fix reverted:

| Witness | Kind | Reverted | Failure |
| --- | --- | --- | --- |
| `a_detached_recorder_gets_int_only_after_its_own_start_marker_d98` | stock Perl, real processes | 0.15.0 `worker.pl` | INT reached the recorder before its marker |
| `an_unconfirmed_recorder_gets_term_and_kill_at_the_existing_cutoffs_d98` | stock Perl; TERM ≥ 2 s and KILL ≥ 3 s after the request, lookalike lines ignored | 0.15.0 `worker.pl` | INT reached an unconfirmed recorder |
| `a_stop_before_the_start_marker_holds_int_until_the_marker_d98` | virtual time; marker 5 s after the stop | `gated = false` | INT before the marker |
| `a_recorder_without_a_start_marker_gets_term_and_kill_at_the_existing_cutoffs_d98` | virtual time; TERM at 30 s, KILL at 32.5 s | `gated = false` | `[INT@0, TERM@30s]` |
| `a_confirmed_recorder_gets_int_as_soon_as_its_stop_is_requested_d98` | healthy control, INT at the stop instant | (passes either way) | — |
| `a_busy_video_service_turns_video_off_for_the_simulator_once_d98` | virtual time | recovery call removed | a second `recordVideo` launched |
| `a_supervised_recorder_reports_its_start_from_the_worker_status_d98` | fake SSH worker; start reported under 5 s, none from `running:` | no wake at acknowledgement / `recording:` ignored | 15.0 s / watchdog |

Updated pins: the two existing stock-Perl recorder witnesses print the marker (as simctl does) and expect
`recording:` in the superseded INT reply; `six_video_batches_keep_ssh_supervision_off_the_healthy_path_d96` allows one
launch `status` per recorder (6–12 control requests); its emulated worker answers `recording:`. The fake worker's
foreground `recordVideo` prints `Recording started` on stderr through a new `Publish.announce` field; a background
(`nohup`) run discards it unseen, so the JVM-recorded `ios-media` transcript is unaffected. The registry was
regenerated.

Stress: the three SSH witnesses passed 50 of 50 runs and 5 of 5 with 18 busy processes on 18 cores; the four iOS
witnesses passed 50 of 50 and once under the same load; the start-report witness passed 50 of 50 after the last
change.

## Real-simulator verification (this Mac, Xcode 27)

- Supervisor path, fresh boot, token INT 0.05, 0.3, 0.5 (×3) and 1.0 s after launch with the x99 `worker.pl`: every
  receipt `done:0`, the movie finalized, and a probe recording afterwards exited 0. At 1.0 s the INT reply already
  said `recording:`. With the 0.15.0 `worker.pl`: 4 of 4 wedged (above).
- Local path, through `ScreenRecordingListener` and the real `KotlinProcessCommandExecutor` (an uncommitted scratch
  test routing only `recordVideo` to the real simulator), fresh boot, stop 0, 200 and 500 ms after `test_started`:
  every recording `Finalized`, `started video recording` logged once, probe exit 0. With the gate reverted: stop at
  200 and 500 ms wedged the simulator (probe exit 16), stop at 0 ms did not.
- Both `mrs-x99-*` simulators were deleted at the end.

## Run locally

`cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p fake-worker -p marathon-test-support
-p marathon-cli --all-targets -- -D warnings`; `cargo test` for marathon-apple, marathon-ios, fake-worker,
marathon-test-support, marathon-macos and marathon-cli (including `differential_vendors`, `ios_production` and
`production_runner`); `make_registry.py --check`, `test_evidence.py`, `differential/run validate`,
`test_apple_evidence.py`, `check_process_spawns.py`, `check_plan_dag.py`, and the parity matrix and source inventory
checks against the release source tree. Not run: `scripts/check.sh`, `remote-check.sh`, `cargo test --workspace`
(orchestrator gates), and the real acceptance on the remote worker.

## Golden diffs

None. No result golden or JVM recording changed. The differential registry changed only in the hashes of the fake
worker sources.

## Verification limits and incidental bugs

- The SSH path's marker detection ran against real simctl through the worker script on this Mac, not over SSH on a
  remote worker; the orchestrator's acceptance run covers that.
- A recorder that confirms only after its INT cutoff still gets TERM (and with the supervisor, possibly INT first if
  the marker lands between the TERM request and TERM) and can still wedge the simulator; recovery (ii) then limits the
  loss to one warning and that run's remaining videos on that simulator.
- The start log on SSH is as precise as the 200 ms poll, and later than that when the control stream was not open;
  after 60 s without a marker it waits for the next renewal.
- Incidental: none outside this WP's scope were found.
