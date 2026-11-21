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
- SSH (`Lease`): `CommandSession::worker_report()` carries a `watch<bool>` fed from `recording:` replies and the
  recorder's stderr from a failed exit receipt (set before the exit proof is published); Rust sends INT at the stop as
  before and the worker holds it. Device logs have no marker and are never gated.

**Learning the start on SSH without slowing the healthy path.** A detached acknowledgement (data status 0) wakes the
lease watcher. While the recorder is acknowledged, unconfirmed, not signalled, within 60 s of launch and before its
next renewal is due, the watcher sends a start probe instead of a renewal, then every 200 ms. A probe
(`Peer::probe`) uses only the already open control stream: it never dials, never falls back to a data session, never
calls `Pending::timed_out` (so an unanswered probe cannot count toward a reconnect), and gives up after 4 s; without
an open stream it is not sent. The worker's own gate protects the recorder either way; the probe only feeds the log
and the busy decision. Probes never move renewals: after any probe the watcher sleeps until the next probe or the
renewal already due (`next_renewal`, at least one interval after launch), whichever comes first, so the first
renewal stays at the configured interval and a confirmation between renewals does not delay the next one (round 2:
before, a probe reply that ended polling restarted the interval from the probe, up to almost twice the interval
between renewals). With the real worker the marker took 0.3–1.0 s, so a healthy start costs one to a few small
requests on the existing stream. The healthy-path CLI witness shows at most one extra `status` per recorder (none
when the stop comes first) and the same 21 s supervised run time as before.

Holding the detach acknowledgement until the marker was considered and rejected: it keeps the SSH session for the
start time, which x95b showed exhausts a ten-session worker, and it delays renewals and widens the "stop before
acknowledgement" window in which a seal sends token KILL.

**Recovery (ii).** A video session that exits with status 16 without having confirmed its start, whose stderr
carries simctl's busy refusal (`Resource busy` or `A recording is already in progress`, both in the Xcode 27 output and
in the remote evidence; read from the local stderr or from the supervisor's `done:16 <base64 stderr>` receipt), while
no other launched video recorder of marathon's on that simulator is still live, sets `video_unavailable` on the
simulator and logs one WARN naming the simulator, saying its video service is busy, that video is off for the rest of
the run, and the shutdown/boot commands. It does not claim marathon did not start the held recording: marathon's own
TERM or KILL at a stop cutoff can cause it too. A bare status 16 changes nothing (round 2). From then on `begin_capture` returns a recording
already finished as `Skipped`: nothing is launched or registered, `opened` fails, and the listener reports nothing
per test. A recording that was already waiting for its turn sees the flag after `ready` and also ends as `Skipped`.
The failed movie itself is still reported as undelivered. Only launched recorders count as marathon's own: the next
test's capture is registered while it waits for the previous one to finalize, and counting it lost the recovery
(found by the busy witness, fixed before review). The state lives on the device object, so a reconnected simulator
tries once more and warns once more.

Restarting the simulator mid-run (i) was not implemented: marathon has no mid-run restart in its lifecycle machinery.
The owner decided instead (2026-11-20) to shut such a simulator down at the end of the run.

**End-of-run shutdown.** `run_on_dispose` is reached only when disposal confirmed the simulator's work ended and it
is not quarantined (D93's verdict). It first runs the configured on-dispose lifecycle exactly as in 0.15.0
(`run_configured_on_dispose`), then `shut_down_busy_video`. That step returns at once unless `video_unavailable` is
set, the seal is the run's end (`SealCause::RunStop` or `Termination`; a `Disposal` seal, which recovery, replacement
and removal use, sends nothing: owner's decision, no mid-run cold boot), the configured lifecycle neither shuts the
simulator down nor erases it, and the simulator is connected and not quarantined. It then takes the window's end
afresh (without the lifecycle's D93 force-reserve floor, already clipped by the provider deadline and D97's signal
finish) and starts only if the configured `shutdown` timeout (30 s by default) is left; otherwise one WARN says there
was no time left. (x101 changed this: the shutdown timeout defaults to the shell timeout, which a run may set to
minutes, so the fit check skipped every shutdown in the x99 acceptance on a remote worker; it now starts with the shorter of that timeout
and twice the force reserve left, and the window bounds it. See `docs/wp/x101-findings.md`.) `shutdown_device` (state read, `xcrun simctl shutdown <udid>`, state reads until Shutdown) runs in a
retained task of its own through the command gate (`as_lifecycle`) under its own cancellation. The disposal waits for
it until the window's end, then cancels it and moves on; the command ends with its executor's bounded cleanup. One
INFO line names the simulator when it shut down; one WARN when it did not, could not, or did not finish within the
window. Nothing else changes: no quarantine, no D93 "on-dispose lifecycle did not end" line, no later disposal
(round 3: before, the shutdown ran inside the configured lifecycle's task, whose cut quarantines, so an overrunning
shutdown quarantined a simulator that had no on-dispose lifecycle at all).

Whether the next run would boot it: every simulator marathon uses goes through `prepare_body`, whose simulator half
calls `boot_for_preparation` unconditionally, and `boot_failure` boots a simulator whose state is Shutdown. The iOS
vendor has no mode that attaches to a booted simulator without that preparation, so the condition holds for every
simulator that can reach `video_unavailable`, and there is no "not managed" case to skip; the WARN still says how to
recover by hand.

**D98, not a D96 note.** Skipping `recordVideo` after the busy code, and the extra `simctl shutdown` at release, change
the commands sent to a worker (§3.12 item 4) in that scenario, so they need a D-row. Result files do not change: a busy simulator produced no movie
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
| `a_busy_video_service_turns_video_off_for_the_simulator_once_d98` | virtual time; busy text → off once, bare 16 → on | recovery call removed / busy text ignored | a second `recordVideo` launched / bare 16 turned video off |
| `a_supervised_recorder_reports_its_start_from_the_worker_status_d98` | fake SSH worker, 60 s renewal interval; ordered by events, 30 s hang guard | no wake at acknowledgement / `recording:` ignored | watchdog |
| `the_start_probe_never_dials_falls_back_or_counts_a_miss_d98` | fake SSH worker; control stream refused, then silent for three 4 s probes | probe through the normal request path | a data-session `control` request |
| `start_probes_do_not_move_the_renewal_schedule_d98` | fake SSH worker, 12 s interval, start confirmed 10 s after the first renewal; second renewal 12.0 s after the first, bound 17 s (5 s margin each side) | probe end restarts the interval | 7.7 s (at the round-2 4 s interval) |
| `an_unanswered_start_probe_does_not_hold_back_a_renewal_d98` | fake SSH worker, 1 s interval, probes after the first unanswered; renewal about 0.6–0.8 s after the unanswered probe's request event, bound 2.5 s | probe wait not clipped | about 4 s (4.4 s from launch) |
| `a_start_marker_split_across_writes_counts_only_when_its_line_is_complete_d98` | stock Perl; `Recording`, ` started`, newline as three writes | match on the partial line | INT before the line was complete |
| `lease_expiry_gives_an_unconfirmed_recorder_term_at_the_existing_time_d98` | stock Perl; 1.5 s lease, no renewals; `done:15` at ≥ 3.5 s, no INT | 0.15.0 `worker.pl` | INT at expiry |
| `a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98` | built CLI over real SSH, emulated supervisor; `done:16` with and without the busy stderr | busy text ignored / receipt stderr dropped | three launches / video stayed on |
| `a_simulator_with_a_busy_video_service_is_shut_down_at_release_d98` | virtual time; one `simctl shutdown`, one INFO | extra shutdown removed | no shutdown |
| `a_configured_shutdown_or_erase_is_not_repeated_for_a_busy_video_service_d98` | SHUTDOWN and ERASE configured | configured check removed | a second shutdown attempt and its line |
| `a_busy_video_shutdown_longer_than_the_release_window_runs_within_it_d98` (x101; replaced `the_busy_video_shutdown_needs_its_timeout_inside_the_release_window_d98`) | shutdown timeout above the window; the shutdown runs within the window, one INFO | full-timeout fit check (x99) | no shutdown |
| `a_busy_video_shutdown_is_not_started_without_time_left_in_the_window_d98` (x101) | most of the window gone before the release; no shutdown, no added time, one WARN | — | — |
| `a_quarantined_simulator_with_a_busy_video_service_is_not_shut_down_d98` | on-dispose lifecycle cut at the window and quarantined | (guarded by the quarantine check) | — |
| `a_hung_busy_video_shutdown_ends_with_the_window_and_never_quarantines_d98` | a state read, then `simctl shutdown`, that never return; disposal within the 200 ms window, not quarantined, one line | round-2 shutdown inside the lifecycle task | quarantined |
| `a_recovery_or_replacement_disposal_sends_no_shutdown_d98` | a `Disposal` seal | end-of-run check removed | shutdown sent |
| `a_healthy_simulator_gets_no_extra_command_at_release_d98` | control: no command at release | — | — |

Updated pins: the two existing stock-Perl recorder witnesses print the marker (as simctl does) and expect
`recording:` in the superseded INT reply; `six_video_batches_keep_ssh_supervision_off_the_healthy_path_d96` allows one
launch `status` per recorder (6–12 control requests); its emulated worker answers `recording:`. The fake worker's
foreground `recordVideo` prints `Recording started` on stderr through a new `Publish.announce` field; a background
(`nohup`) run discards it unseen, so the JVM-recorded `ios-media` transcript is unaffected. The registry was
regenerated.

Stress (round 1): the three SSH witnesses passed 50 of 50 runs and 5 of 5 with 18 busy processes on 18 cores; the
four iOS witnesses passed 50 of 50 and once under the same load; the start-report witness passed 50 of 50. Round 2
figures are in the next section.

## Round 3

**Linux gate failure of `a_busy_simulator_on_an_ssh_worker_turns_video_off_once_d98` (1 launch instead of 3).** A test
ordering defect in the witness, not in the product. In the bare-status-16 variant every recorder must launch, but the
emulated supervisor released all three tests' fake-world barriers at the first failed launch. Batches are serialized
on video collection, so the race is within one test: once its barrier is down, the fake xcodebuild prints the test's
start and end back to back, the listener's `test_started` spawns the recording task, and `test_passed` can request its
stop before that task first runs. `Recording::run` then sees the stop before it launches anything and ends as not
started, as it always has for a test that ends before its recorder launches (Kotlin behaves the same). The busy
variant was unaffected because its later recorders are not launched by design. Making test 1's finalization 2 s
slower did not reproduce it on this Mac, nor did 15 runs under 2× CPU load, which fits a race between two adjacent
events in one task rather than with the recorder's previous finalization. Fix: in the bare variant each test is
released only by its own recorder's launch (the `started` barrier the launch publishes), which orders the launch
before the test's end; only the busy variant pre-releases the later tests. The old form could not be made to fail
locally, so the fix is shown by reasoning and by 20 + 20 loaded passes, not by a revert.

Other round-3 changes: the end-of-run shutdown above; a start probe on a silent stream waits no later than the renewal
then due (it waited the full 4 s before); the renewal witness widened to a 12 s interval.

A round-3 stress run under 2× CPU load failed one of five runs of
`a_start_marker_split_across_writes_counts_only_when_its_line_is_complete_d98`: its one-shot `control status`
answered `busy`, which the worker returns when its token lock is held for that instant (the request is then not
applied; the controller's lease watcher retries it). The D98 Perl witnesses now send every one-shot control request
through `d98_control`, which repeats it while the worker answers `busy`; an INT request that met `busy` would
otherwise have been lost too.

Round-3 stress on the final tests: the SSH D98 witnesses 50 of 50 and 5 of 5 under 2× CPU load (36 busy processes on
18 cores); the iOS D98 witnesses 50 of 50 and once under load; the CLI busy-recorder witness 20 of 20 and 20 of 20
under 2× load.

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

- Review round 4 (orchestrator): D98's end-of-run shutdown now ends the D93 force reserve before the window's end, so
  the SSH lease release after it is not left with an already-passed deadline (which logged a second "lease release
  timed out" warning and abandoned the connection). The CLI witness now runs with no on-dispose lifecycle and counts
  the shutdown commands (1 for the busy refusal, 0 for a bare status 16), so a normal run end (`Termination`) is covered.
- The video-off state belonged to the device object, not the simulator, so a simulator set up again during the run
  tried recording once more and one disposed of during the run was not shut down at its end. x102 (U259) moved it to
  the simulator for the whole run; see [x102 findings](x102-findings.md).

- The SSH path's marker detection ran against real simctl through the worker script on this Mac, not over SSH on a
  remote worker; the orchestrator's acceptance run covers that.
- A recorder that confirms only after its INT cutoff still gets TERM (and with the supervisor, possibly INT first if
  the marker lands between the TERM request and TERM) and can still wedge the simulator; recovery (ii) then limits the
  loss to one warning and that run's remaining videos on that simulator.
- The start log on SSH is as precise as the 200 ms poll, and later than that when the control stream was not open;
  after 60 s without a marker it is learned at a renewal.
- The marker stream was verified on Xcode 27 only. If another Xcode printed `Recording started` elsewhere (stdout, or
  not at all), the gate fails closed: no stop ever sends INT before the INT cutoff, every stop takes the whole INT
  window (at least 15 s per test, off the test event path but holding the next recording) and ends with TERM (an
  unfinalized movie), and nothing wedges that would not have wedged before. Video would then be lost on that Xcode,
  visibly, rather than the simulator, and runs would be slower. The orchestrator checks the stream on the remote
  Xcode 26.6 worker during acceptance.
- The busy text is matched in English (`Resource busy`, `A recording is already in progress`). A localized simctl
  failure reason would not match: the simulator would then keep 0.15.0's behaviour, a failed recording per test with
  its stderr, and no end-of-run shutdown.
- Incidental: none outside this WP's scope were found.
