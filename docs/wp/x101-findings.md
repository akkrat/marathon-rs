# x101 findings: the end-of-run shutdown of a busy simulator runs within its window

## Defect (U258)

D98 (x99) shuts down, at the run's end, a simulator whose video service was left busy, so the next run boots it
clean. The shutdown started only if its whole timeout (`timeoutConfiguration.shutdown`) fit in what was left of the
release window, less the D93 force reserve. That timeout defaults to the shell timeout. The x99 acceptance on a remote
Mac worker ran with a shell timeout of 2 minutes and no shutdown timeout, with about 65 s of the window left at the
release, so the shutdown was never started: the run logged that there was no time left, and the simulator stayed busy
into the next run. Adding `shutdown: PT30S` made the same run shut it down in about 4.3 s, after which the next run
booted it and delivered every video.

## Fix

The window, not the timeout, bounds the shutdown. It starts when at least the shorter of its timeout and twice the
force reserve (10 s of the default 70 s window) is left after the lease release's reserve, and is cancelled where the
window ends, as before. A shutdown cut there is one warning and changes nothing else (D98). Nothing else changes: the
same conditions (run end only, confirmed and not quarantined, not when the on-dispose lifecycle shuts down or erases)
and the same lines.

## Witnesses

- `a_busy_video_shutdown_longer_than_the_release_window_runs_within_it_d98` (replaces
  `the_busy_video_shutdown_needs_its_timeout_inside_the_release_window_d98`): a shutdown timeout of 300 ms against the
  harness's 200 ms window; the shutdown runs, the release ends within the window, one INFO line. Fails on 13bac094
  (no shutdown).
- `a_busy_video_shutdown_is_not_started_without_time_left_in_the_window_d98`: 170 ms of the window gone before the
  release; no shutdown, no added time, one WARN.

## Not verified

The changed path was not rerun on a real worker; the x99 acceptance ran the same shutdown successfully with a fitting
timeout.
