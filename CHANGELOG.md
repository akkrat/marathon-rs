# Changelog

The section for a version becomes the text of its GitHub release, which carries the date.

## 0.15.0-rc.1

The first release candidate of 0.15.0, a pre-release for trial runs. It adds run supervision, bounded waits and process
ownership, and fixes Android, iOS and macOS device-handling bugs found after 0.14.0, with the approved differences
D82–D88 and D90–D97 (D89 was reserved and not used). The inputs and the CLI are unchanged; there is no new
configuration field. The other contracts change only as listed below: D82 and D83 change which device runs a retry or a
copy, and with it attempts, batches, the order and grouping of test selectors in worker commands, and device-dependent
verdicts; D84 and D88 turn hangs into failed runs; D85 adds Android shell commands; D96 runs Apple worker commands under
a supervisor and makes SIGHUP a cancellation with exit status 129; D97 changes which artifacts the reports list. The
release also bounds waits, buffers remote metrics, and replaces saturated shared SSH connections after an abandoned
session, as each entry says.

### Check before you upgrade

- **Process groups and the reaper (D96):** every local tool marathon starts (rsync, ssh, adb clients, local xcodebuild
  and the like) runs in a process group of its own, so a signal sent to marathon's process group reaches marathon,
  which runs its bounded cleanup and then ends every tool it registered. A run-scoped reaper ends them even if marathon
  is killed. It shows as a second `marathon` process with the same arguments until the controller exits, and ignores
  INT, TERM and HUP; scripts that count processes or use `pgrep`, `pidof` or `killall` see both, so target the
  controller's PID. Background work a tool started in its group ends with it: keep a command session open until such
  work has finished. With `stty tostop`, a tool writing to the inherited terminal can stop, and so can SSH if it prompts
  on `/dev/tty` for a password or a key passphrase: use noninteractive SSH authentication and redirect inherited
  terminal output. ADB servers keep their own lifetime.
- **SIGHUP (D96, D97):** a hang-up now runs the same finish as SIGTERM and exits with 129, where it used to end marathon
  at once without reports or cleanup. The first INT, TERM or HUP seals the devices and starts the reports; cleanup runs
  within fixed cutoffs from the signal and hands over to the exit by 24 s, with up to 1 s more for console output (see
  the signal finish below). A second signal exits at once.
- **Password SSH (D96):** from 0.15.0-rc.1, password-based rsync uses OpenSSH's askpass on OpenSSH 8.4 or newer, which
  keeps rsync and its SSH client together in a private process group that marathon owns. `sshpass` is needed only with
  an older or unrecognized SSH client; marathon then logs one warning per run, and its cleanup covers only the processes
  it has seen.
- **Apple SSH workers (D96, D90):** stock `/usr/bin/perl` with its core modules on the worker is optional and
  recommended. With it, worker commands, remote rsync senders and video recorders run under a worker-side supervisor
  that ends them when their lease runs out without a renewal, even if marathon was killed, and keeps a small private
  directory under `/tmp` while the connection lasts. The lease is 79 s by default from admission or the last successful
  renewal; a renewal already in flight can extend it once, a recorder then gets up to 3 s more to finalize and stop, and a
  frozen worker acts once it runs again. Without usable Perl, marathon logs one WARN per worker address and keeps the
  previous command paths. An SSH forced command or filter must allow the supervisor's bootstrap, the `control-stream`
  command with stdin intact, token control requests, supervisor retirement and the detached video launch, and must
  `exec` the original command: a filter that stays in front of the wrapper can turn its failure into status 137, which
  marathon can read as a confirmed exit. Without Perl, allow the legacy video launcher, pidfile and PID-based stop
  commands. With Perl, marathon also keeps a separate SSH control connection to each worker (D96); with
  `shareWorkerConnection: true`, a worker can see one replacement data connection after a session-capacity refusal
  that follows an abandoned session (D90).
- **Only complete artifacts are published (D97):** a downloaded artifact (a video, screen capture, xcresult, log,
  coverage or file-sync file) appears at its final path only once it is verified complete: on Android, adb's DONE, a
  completed write and a byte count that agrees with the size STAT reported (its low 32 bits with legacy STAT), so a file
  that grows during its pull, such as a live app file collected by file sync, is not delivered; for rsync, a successful
  exit and confirmed cleanup, so a receive whose output pipes are still open after its group's cleanup is not delivered.
  A failed, cancelled or timed-out download leaves no partial artifact at the final path and keeps any earlier output;
  each artifact path it attempted is logged once as not delivered, and the reports have no attachment for it. On a
  filesystem that rejects rename flags, replacing an Apple directory can leave the final path absent for a moment, and
  if a failed publication cannot restore the earlier tree, it is kept in a named backup. On a signal, eligible artifacts
  drain until 12 s after it. A hard kill can leave `.marathon-artifact-*` or Android pull staging directories behind.
- **Retry placement and copy order (D82, D83):** a retry from an Android device goes to another device of the pool
  while other tests are queued, and the copies of different tests (flakiness copies, `count` repeats, a test
  discovered twice) take turns in the queue. A test can need fewer attempts and a run fewer executions; `raw.json` rows
  and their devices change; batches change, so `fixed-size` batching can start more or fewer instrumentations; and the `am instrument -e
  class` and xcodebuild `-only-testing` lists change in number, order and grouping. A test whose outcome depends on the
  device can get another verdict, and with `ignoreFailures: false` the run another exit status. Update snapshots of
  worker commands or batches for runs with retries or copies.
- **Android farms (D85, D88):** marathon reads `/proc/sys/kernel/random/boot_id` and writes, reads and removes a
  `.marathon-ready-<uuid>` marker under `/data/local/tmp`; allow them if you filter adb shell commands. During an adb
  outage or a brief `offline`, a device whose boot ID changed, or whose marker is confirmed missing or different, is set
  up again; if the marker's creation could not be confirmed, the boot ID alone is checked, and without readable evidence
  of a reset ordinary recovery goes on. A still-listed device gets one more setup after its preparation has used its
  retries. Android device monitoring that ends with an unexpected error or a panic (not the provider's own termination)
  now fails the run with the monitor's cause (exit 1, 0 with `ignoreFailures: true`) where the run went on without
  device events; a failing analytics call for a device no longer ends it.
- **What a stuck device or tool costs now (D87, D91–D95):** with the defaults,
  - a device that keeps failing slowly to get ready starts no new retry, and an iOS simulator no new replacement, after
    one preparation deadline; work already running can finish. It leaves in about 9 to 12 minutes for an iOS simulator,
    19 for an Android device (about one more if D85 sets it up once more) and 14 for a Mac (scenario estimates), where it
    took up to 33, 96 and 30, with a WARN saying it is not prepared within its preparation time. Raise
    `deviceInitializationTimeoutMillis` to give slow devices longer (D87);
  - the runner waits at most 40 s for a stopped rsync transfer to an Apple worker to be cleaned up and confirmed. If
    that fails, an ERROR names the worker and rsync's pid, nothing more is sent into that exact destination in the run,
    and a device that needs it leaves its pool (D91);
  - a stopping Android device has 60 s for its in-flight work and cleanup, 65 s in the Scheduler's wait (D92);
  - when a run stops after a failure, each sealed iOS simulator and Mac has one 70 s window for its work and its
    disposal together. The Scheduler waits up to 75 s for the devices, and the device waits take up to 80 s in all,
    apart from the iOS artifact drain, where they could take 145 s. A device whose work is not confirmed to have ended
    gets one WARN naming it and no on-dispose lifecycle, and its lease is quarantined: the connection stays open and
    takes no new device until the run ends, while devices already sharing it can go on using it. Look at that worker
    before the next run (D93);
  - pool and queue drains share 65 s, with one WARN per stalled pool or queue, and a cancelled device subscription has
    its own 65 s and warns if it stalls; committed results are kept, but work a stalled handler has not committed can
    be lost. iOS artifacts get 30 s to drain and 40 s for cancellation after an internal failure or owner cancellation,
    and 300 s and 60 s otherwise (it was 300 s and up to 120 s), and the log names bundles left undelivered (D94);
  - the last Apple worker lease has up to 40 s to disconnect, clipped to the device's or provider's deadline; the history
    backend 30 s to close, after a separate 3 s refresh join; the Android provider 65 s and the iOS provider 70 s after
    its artifacts. An expired deadline logs a warning and the finish goes on. In an ordinary finish the reports always
    finish writing, and one that takes longer than a minute logs the active reporter (D95); after a signal, D97's
    cutoffs apply.
- **A failing run's own machinery (D84):** a run whose queue, pool, device listener or run task fails or panics now
  ends with exit 1 (0 with `ignoreFailures: true`), writes its reports and logs `Marathon run failed` with the first
  cause, where it used to hang until killed or report no cause.
- **Metrics (D86):** Graphite and InfluxDB writes go through a bounded queue and can be dropped. Use local reports for
  result counts; if a dashboard has gaps or a history-based strategy changes, look for `The analytics backend is
  dropping test metric events`, the rejection count or `The analytics backend did not close` in the run log.

See [Changes that need action](docs/migration.md#changes-that-need-action) for the migration table and
[the deviations guide](docs/deviations.md#d82--a-retry-is-kept-off-the-device-it-came-from-also-without-a-worker) for
each approved difference. The 0.14.0, 0.13.0 and 0.12.0 requirements, including a finite `uncompletedTestRetryQuota`,
still apply when upgrading from an older version.

### Runs, queue and supervision

- **Internal failures end the run instead of hanging (D84, U206):** a run whose own machinery stops unexpectedly (a
  test queue or the device listener stopped by a cancellation of its own, a failed device listener, or a pool or the run
  itself panicking) now ends with exit 1 (0 with `ignoreFailures: true`), writes its reports, and logs `Marathon run
  failed` with the first cause, where it used to hang until killed or report no cause. The cause is logged as soon as it
  happens. A failure that reaches the run only after a signal or its owner cancelled it is logged at WARN and does not
  replace the cancellation (U215). Before writing the reports the run admits no more work, waits for each device to end
  its cancelled work, an Apple preparation included (up to 75 s for an iOS or macOS device and 65 s for an Android one,
  D92), and lets every queue take the results already sent to it, so tests that finished keep their results; a queue or
  pool that itself failed loses what was left in its mailbox. A failure in a pool that a device joining at the very end
  of the run started also fails the run, and the reports are written only after it has stopped. An iOS device monitoring
  that panics, or fails with no simulator available, ends the run with its cause instead of reporting no devices; with
  simulators connected the run carries on. An iOS or macOS preparation, batch, disposal, provider step or xcodebuild
  session that panics now ends with the panic as its error, which the run handles as any other device error (a
  preparation is retried, a batch's tests are run again within the retry limits), where the run used to hang. In runs
  without a failure: a device that connects while the run is finishing is no longer started, results that had reached a
  late pool's queue now always appear in the reports, and the reports are written even when terminating the provider
  fails.
- **A retry goes to another Android device (D82, U204):** a test that comes back incomplete from an Android device
  (for example when its instrumentation ends before the test starts), or that fails and is retried, is now left to the
  pool's other devices while other tests are queued. One emulator that keeps failing no longer takes the same test back
  at once until `uncompletedTestRetryQuota` is used up; it runs the other queued tests first and takes the retry itself
  only when nothing else is queued for it, so in a pool of one device a retry now runs at the end of the queued work.
  Another device takes the retry when it asks, by queue priority and batching. The quotas and exit-code rules are
  unchanged, but a test can need fewer attempts and get a different verdict (a retry that passes on another device ends
  it), so with `ignoreFailures: false` a run can exit 0 where it exited 1, or the reverse. `raw.json` names other
  devices in another row order, and a test's final result can name another device. Unless a fresh copy of the same test
  is queued too, a device's own retry no longer shares a batch with fresh tests, so `fixed-size` batching can start one
  more instrumentation, which adds wall time, and the `am instrument` commands change in number, order and `-e class`
  grouping. iOS and macOS already sent retries to another worker and are unchanged.
- **Flakiness copies take turns (D83, U205):** with `flakinessStrategy: probability` or `fixed`, or `shardingStrategy:
  count`, the extra runs of different tests now alternate in the queue instead of each test's runs waiting together; so
  does a test discovered twice, for example in two test APKs. Free devices spread over the tests, so with
  `executionStrategy.fast` (the default) more of a test's runs can still be queued when it is decided, after its first
  pass under `ANY_SUCCESS` or its first failure under `ALL_SUCCESS`, and those are dropped, as in the JVM build; runs
  that have started, or whose batch a device already has, still run. Runs can use less device time and list fewer
  executions. Which device runs which copy can change, so a test that passes on some devices and fails on others can
  get another verdict, and with it the run's exit status, and its final record can come from another run; the rules that
  decide them are unchanged. Worker commands can list a batch's tests in another order and grouping, `fixed-size`
  batches can be fuller or emptier and more or fewer, and with code coverage on an Android test in another batch gets
  that batch's coverage file.
- **Faster `class-name` batching on large suites (U213):** the runner indexes queued tests by class and preserves
  batch membership and order. In a local timing test, draining 50,000 one-test classes took 43 ms instead of 52 s.
- **A device that cannot be prepared leaves sooner (D87, U209):** a device's preparation, the retries inside it and an
  iOS simulator's replacements after a failed boot now share one deadline, the greater of
  `deviceInitializationTimeoutMillis` and what one preparation can take with its retries at their limits (550 s on iOS,
  about 18 minutes on Android with an app and its test app, 812 s on macOS, with the default timeouts). No retry or
  replacement starts after it, and work already running can finish, transfers included. D85's one more setup of an
  Android device continues the same deadline but always gets its first attempt. An iOS simulator whose readiness probe
  never succeeds now leaves after one replacement, in about 9 to 12 minutes instead of 26 to 33, an Android device whose
  install keeps timing out in about 19 minutes instead of 96 (about one more with D85's extra setup), and a Mac whose
  commands hang in about 14 instead of 30 (scenario estimates for those cases with the defaults). Healthy runs, and a device that fails once and then
  gets ready, are unchanged. Raise `deviceInitializationTimeoutMillis` to give slow devices longer.
- **Pool and queue drains share 65 seconds (D94, U220):** the Scheduler interrupts stalled handlers at the deadline,
  logs one WARN for each, and keeps committed test results; work a stalled handler has not committed can be lost. A
  cancelled device subscription has its own 65 seconds to exit and warns if it stalls. An internal run failure or owner cancellation gives iOS
  artifacts 30 seconds to drain and 40 seconds for cancellation. A suite with failed test assertions keeps the routine
  300-second drain. On routine runs, the worker joins and final receipt share one 60-second cancellation allowance,
  reducing the maximum artifact wait from 420 to 360 seconds. Both drains are separate from D93's device waits. The log
  identifies undelivered bundles.
- **Finish deadlines and report progress (D95, U221–U226):** test runs cap the waits for device-provider shutdown
  (65 s for Android; 70 s for iOS, starting after artifact finalization), Apple worker disconnects (up to 40 s for the
  last lease, clipped to the device's or provider's deadline) and history analytics close (30 s, after a separate 3 s
  refresh join). A deadline that expires logs a warning and the finish goes on. Interrupted Apple artifact pulls use the
  device's remaining shutdown time. In an ordinary finish, reports still finish writing and log the active reporter if
  writing takes longer than one minute.

### Process ownership and signals

- **Controller process cleanup on exit (D96, U227):** marathon runs each local tool in its own process group and stops
  the registered tools on exit, including when the controller is killed, through a run-scoped reaper. A hang-up runs the
  same bounded cleanup as a termination signal. ADB servers stay available.
- **Local simulator batch timeouts on macOS (D96, U245):** a batch whose command group is already exiting keeps its
  timeout outcome and the simulator remains available, where the close error marked the simulator lost.
- **Password-based rsync with OpenSSH 8.4 or newer (D96, U228):** transfers use askpass and keep rsync and its SSH
  client together in a private process group that marathon owns. Older or unrecognized SSH clients still need `sshpass`;
  marathon then logs one warning per run about reduced cleanup coverage.
- **The signal finish reserves time for cleanup (D97, U241, U242):** the first SIGINT, SIGTERM or SIGHUP seals the
  devices before their work is cancelled and starts the reports. Counted from the signal, provider and history cleanup
  start when the reports are written or at 10 s, whichever comes first; eligible artifacts drain through 12 s; SSH
  closes still waiting on TERM request KILL by 17 s; transfer cancellation and receipts end by 18 s; device verdicts,
  on-dispose lifecycles of confirmed devices and lease releases get the time left through 23 s; and the exit takes over
  by 24 s, with up to 1 s more for console output. A second signal exits at once. Android devices are sealed too, an iOS
  run reaches its device cleanup before it exits, and an iOS setup retry no longer creates and boots a simulator while
  termination drains artifacts. SIGINT exits 130, SIGTERM exits 143, and SIGHUP exits 129.

### Android

- **Android checks the boot before resuming tests (D85, U207):** after an adb outage or brief `offline`, a device gets
  fresh setup when its boot ID changed or its preparation marker is confirmed missing or different, as after a snapshot
  rollback. If the marker's creation could not be confirmed, recovery checks the boot ID alone; without readable
  evidence of a reset, ordinary recovery goes on. Batches that never started keep their retry quota. A still-listed
  device gets one more setup after exhausting preparation, within the pool's existing return wait. A shared-kernel
  restart that preserves both the boot ID and marker can go undetected.
- **Device monitoring that ends fails the run (D88, U211):** a failing analytics call for a connected device no longer
  stops device tracking, and device tracking that stops for any other reason (an unexpected error or a crash, not the
  provider's own termination) now fails the run with the monitor's cause, where the run went on without noticing devices that left or came, and a batch waiting out an adb
  outage could wait for ever; during remote test discovery it is now the run's error instead of a report that no device
  came.
- **Android cleanup has a shared deadline (D92, U218):** by default each device has up to 60 seconds to stop, including
  five seconds for forced ADB connection closure and producer joins. The Scheduler allows five more seconds. Other
  devices on the same server keep working when one device is retired. Teardown discards interrupted staged pulls, and an
  internal failure still reports its original cause and results already committed to the queues.
- **Android pulls are verified (D97, U235–U237):** a pull counts only with adb's DONE, its writer completed and a byte
  count that agrees with the size STAT reported (its low 32 bits on devices with legacy STAT), including for empty
  files; a mismatch, such as a file that grows during the pull, is undelivered. A short or cancelled pull no longer
  replaces the final file or logs `Transferred`. A pull whose staging fails is undelivered instead of being written straight into the final
  path. Videos, screen captures and profiling files are attached, and their device copies removed, only after their pull
  was delivered, and APP_DATA file sync removes application data only after its copied files have been delivered.

### iOS and macOS

- **Device monitors and preparations end cleanly (U210, U211):** on iOS, a crash while reading the Marathondevices
  file now fails the run instead of hanging it, and a preparation stopped while it sends the test bundle now ends after
  that transfer (waiting up to 70 s for it), so the run's stop and the simulator's disposal wait for it. On macOS, a Mac
  already disposed of at the end of the run no longer starts a preparation.
- **Apple device setup and preparation survive analytics failures (U212):** if a timing read or tracker fails, the
  runner warns and the device can still join the run and take tests after successful setup and preparation.
- **A saturated shared Apple worker can take new devices (D90, U172):** when an abandoned SSH session is followed by a
  session-capacity refusal, the next connection acquisition can open a replacement. Existing sessions keep running on
  their original connection, which closes after its last lease is released. Each shared worker transport has at most two
  live generations. If the replacement also fills after an abandonment while the first connection still has leases,
  preparation can still fail until a generation slot frees. A refusal without abandonment keeps the current connection.
  Each connection logs its first session-open refusal at WARN, also with `debug: false`.
- **A stopped Apple file transfer is waited for at most 40 s, and its simulator waits for it (D91, U210):** a cancelled
  rsync transfer (the run stopping, a preparation cancelled, test discovery timing out) no longer waits without a limit
  for rsync to be reaped or for its output, which held every later transfer to that worker; its cleanup and the
  confirmation of it take at most 40 s. An iOS simulator's disposal now waits for its sends, also those nothing else
  waited for any longer, before it shuts the simulator down or terminates its app. If marathon cannot confirm that
  rsync and what it started have ended, it logs an ERROR naming the worker and rsync's pid and sends nothing more into
  that exact destination in the run; a device that needs it leaves its pool. Healthy transfers are unchanged.
- **A failing run's Apple devices stop within one window, and are disposed of only once their work has ended (D93):**
  when a run stops after a failure, each sealed iOS simulator and Mac gets one 70 s window, started for all of them at
  the stop, for its cancelled batch, its preparations, its transfers and its disposal together; its disposal no longer
  waits another 70 s. The Scheduler allows 75 s for the devices, and the device waits take up to 80 s in all, apart from
  the iOS artifact drain, instead of up to 145 s. A simulator's on-dispose lifecycle runs, and a device's worker lease
  is released, only if all of that work is confirmed to have ended. Otherwise one WARN names the device, the lifecycle
  is skipped, the device sends nothing more, and its lease is quarantined: the connection is kept open and out of new
  use until the run ends (other simulators already sharing it keep using it; a simulator set up meanwhile gets another
  connection). The decision is made once and holds for every later step. A Mac's preparation is now
  stopped when the Mac stops, its transfers are waited for like an iOS simulator's, and a transfer to a worker on this
  host no longer starts once its device is stopping. A pull from the device (an artifact the run's end left running past
  its drain) still running at its disposal also keeps the lifecycle and the release from running beside it, and is
  stopped; so does a command the device sent just before it stopped, until it has ended. Healthy runs, results and exit
  codes are unchanged.
- **Apple SSH command cleanup (D96, U230, U243):** workers with stock Perl run commands and rsync senders in groups that
  expire if the worker stops receiving renewals, so xcodebuild no longer outlives a stopped or killed controller on the
  worker beyond its lease. Marathon reconnects its control connection after a drop, and closing a connection waits for
  a supervisor probe in flight and attempts the supervisor's retirement if it is ready, so the private directory is not
  left behind on the worker. For a worker without usable Perl, marathon logs one warning per worker address and keeps
  the existing command path.
- **iOS video recorder cleanup (D96, U229, U246):** supervised recorders release their SSH session after launch. Batch
  cancellation, an unhealthy device and simulator disposal all stop the recorder and give it time to finalize the movie
  before escalating. A forced stop gives the video service time to finalize, so a recorder killed at lease expiry no
  longer leaves the simulator's recording service busy, and a failed detached recorder reports bounded stderr. When a
  transcode fails, the runner attempts to remove the partial output and keeps the staged original for fallback (U244).
- **SSH worker follow-ups (D96, U247–U250):** successful pulls use the transfer's own exit status and token requests
  share one long-lived control session, which removes extra SSH round trips from healthy runs. Two consecutive
  unanswered 4 s control windows trigger a reconnect; a late reply resets the count, and a held detached-recorder INT
  is excluded. Each token request is bounded (8 s, within the remaining cleanup time). Only a detached recorder's INT can
  wait for the completed receipt, and a stronger signal can preempt it.

### Artifacts and reports

- **Reports include only complete artifacts (D97, U238–U240; Android above):** a failed or interrupted download leaves
  no partial artifact at the final path, keeps earlier files, and is reported once as undelivered for each artifact path
  it attempted. Each receive owns a private area on the destination filesystem and publishes a verified artifact by
  atomic rename; an xcresult is published whole under its batch name. An rsync receive needs a successful exit and
  confirmed cleanup, so one whose output pipes are still open after its group's cleanup is undelivered even when rsync
  reports success; a local iOS or macOS copy must finish reading and writing every entry. A requested iOS video also
  needs a finalized recording. A failed pull leaves its remote artifact on the device or worker for inspection, and
  listeners attach and remove artifacts only after their delivery. A complete zero-byte file is delivered.

### Analytics

- **Graphite and InfluxDB writes use a bounded queue (D86, U208):** bursts with room proceed immediately, and sustained
  load waits for capacity. Metrics can be lost if the wait expires, the backend is cooling off, admission closes, the
  delivery task stops, or shutdown reaches its deadline. Gaps can affect the flakiness and duration history used by
  later runs.

### Evidence

- The findings documents ([x80](docs/wp/x80-findings.md)–[x96](docs/wp/x96-findings.md)) record the regression
  witnesses for each change, the checks that reproduced the failures (a temporary revert or the commit before the
  change), and what was not verified. Coverage includes whole runs with the Stub vendor (D82–D84, D93, D94) and with
  fake adb in virtual time (D85, D87, D88, D92, D97), Apple simulator and transport tests over real loopback SSH (D90,
  D93, D96; D90's prepares a simulator on the replacement connection but is not a whole run), and real child-process
  tests of rsync, ssh, process groups and the reaper (D91, D96, D97). Selected concurrency witnesses passed 50 isolated
  runs and runs under CPU load.
- The JVM recordings are unedited. D82's and D83's queue recordings replay unchanged. Narrow comparator allowances, each
  with negative tests, cover D85's boot ID read, marker write and marker removal, D96's supervisor bootstrap, wrappers,
  receipts and recorder commands and rsync4j's warning on live native captures only, and D97's private directory in the final host argument of a native xcresult,
  video or device-log receive. Kotlin tests that pinned removed behaviour are `n/a` in the parity matrix with their
  reason: D93's disposal of a batch that has not finished in time, D96's pidfile waits, and D97's changed command and
  diagnostic expectations. No registered native golden changed.
- The findings documents record review rounds and their fixes, including cross-model reviews, from one or two rounds
  for most changes to eleven rounds and a closing audit for D93; [x88](docs/wp/x88-findings.md)'s document records its
  evidence but no review round.
- CI runs the repository gate on macOS and Linux; it passes at f90a6619, the code of this release candidate. During the
  batch six CI runs on `main` failed in a test and two more ended with a cancelled macOS job; later runs passed. Two of
  the failures, in the macOS local batch-timeout witness, exposed U245, a macOS process-group race that is fixed in the
  product (D96).
- Real-device acceptance ran on two earlier `main` commits, not on this release candidate's binary, and before the last
  fixes. On 1d88c0a9, before the x95e and x95f changes, the CI static Linux build ran on two API 36 Android emulators on
  a Linux host. The CI macOS build ran iOS simulator tests (iOS 18.6 and 26.4) and hosted macOS XCTest locally on this
  project's Mac, and iOS simulator tests over SSH to a second Mac as a real worker with stock Perl, key authentication
  and an `exec`-based forced command. The control runs matched the per-test outcomes of the earlier acceptance runs and
  0.14.0's release candidates, and their normalized report fields compared equal, apart from the Android video
  references that D97 omitted for recordings that were not delivered. On Android, SIGTERM during a stalled 300 MiB pull
  exited 143 with no partial file and one undelivered record for the file (a separate diagnostic named its enclosing
  folder), where an earlier build had published a 1,703,936-byte truncation (D97); a device retired during a stalled pull
  and a held cleanup command ended within D92's limits while the other emulator finished its tests; a reboot and a
  snapshot rollback were set up again (D85); a held InfluxDB response did not stop the run (D86); a failing install left
  within D87's deadline; and SIGKILL, a group TERM then KILL, SIGHUP (129) and a second SIGTERM left no marathon process
  or tool behind while the adb server stayed up (D96). On the Apple side, xcresult pulls stalled under SIGTERM or an
  internal failure stayed unpublished and were reported undelivered; where a pull was still unconfirmed at disposal,
  the device skipped its lifecycle and its lease was quarantined, and the first cause was kept (D93, D97). After
  SIGKILL of marathon, the worker's xcodebuild and recorders were gone by their lease expiry (D96). Those runs found what x95e fixes: a repeated healthy run on simulators whose recorders had been
  killed at lease expiry lost all six videos (U246), and the healthy SSH run had extra control round trips (U247).
- On 7a486ea1, which has the x95e changes but not x95f's, the SSH runs to the second Mac were repeated: three healthy
  runs, and one on the same simulators after a SIGKILL, each delivered all six videos and xcresults with the same report
  data; recorders killed at lease expiry finalized their movies and the next recordings worked; the extra control round
  trips of U247 were gone; and SIGTERM during a live stalled xcresult pull exited 143 with nothing published. One healthy
  run took 121 s against 40 to 51 s for the others, the difference in a slow app launch inside the tests, whose cause
  remained undetermined.
- Not exercised on real hosts: the password and askpass path, D90's connection rotation, network faults and delayed
  control replies on a worker (x95f's U248–U250), a terminal D88 monitor failure, and an induced queue or pool drain
  failure. Real-device runs of this release candidate's binary and the maintainer's private CI farm runs are pending
  and will be recorded before 0.15.0.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container; a CI build of
  an earlier commit of this release candidate ran on real emulators, and 0.14.0-rc.2 also ran on a CI farm. musl's resolver asks DNS directly, so an adb host given as a name that only the system's own name service knows,
  such as a `.local` name, is not found; use `localhost` or an address.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The differential suites run it with a
  stand-in file, so what the library itself does is not covered by them. The archives do not contain it, and this project's
  licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of that evidence
  ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a disposable environment and keep your
  JVM distribution for [rollback](docs/migration.md#trial-and-rollback).
- An Android device that restarted during an adb outage is set up again when its boot ID changed or its preparation
  marker is confirmed missing or different (D85). A restart that keeps both, such as a shared-kernel container restart
  that keeps `/data/local/tmp`, a snapshot rollback that keeps the boot ID when the marker's creation could not be
  confirmed (the boot ID alone is then checked), and a device with no readable evidence of a reset are not detected;
  such a device can repeat failing batches until `uncompletedTestRetryQuota` runs out (D22).
- On a worker without usable stock Perl, a cancelled run whose SSH server ignores signals (OpenSSH does for forced
  commands) can leave xcodebuild running there; a close that needed KILL logs a warning naming the command, also with
  `debug: false` (D30, U183). With Perl, the worker's supervisor ends the command's group when its lease runs out
  without a renewal: 79 s by default from admission or the last successful renewal, extended once by a renewal already
  in flight, plus up to 3 s for a recorder to finalize and stop; a frozen worker does so only once it runs again (D96).
- Xcode's shared service hosts, such as `DTServiceHub`, that xcodebuild starts in groups of their own can outlive a run,
  locally and on a worker. That is on purpose: other Xcode sessions use them (D96).
- A failed readiness probe costs a simulator about 9 to 12 minutes with the defaults while another simulator keeps the
  pool alive, and at most the preparation deadline plus one boot attempt, about 14.5 minutes (D87 scenario estimates).
  On the last device the run ends 180 s (D48's wait) after the simulator left the pool, or after a test result it
  reports later, with "remaining tests will not run": about 9 to 11 minutes after its first boot, or about 3 minutes
  after a loss if the probe starts failing at a re-setup (D38).
- A failed xcodebuild close retires its simulator; its disposal is then unconfirmed, so it gets no on-dispose lifecycle
  and its connection stays open and out of use until the run ends (D93), and a pool whose last device it was waits (D48),
  then ends (D30).
- An Android device that left its pool after D49's limit but not adb's list returns only if the list changes, so a pool
  waiting for it uses up D48's wait for nothing. After a failed preparation it gets one more setup (D85), and after that
  the same holds (D48).
- Two adb outages in a row, the second before a device's restoring after the first has finished, can make D48's wait
  for that device count from the later outage ([x83 findings](docs/wp/x83-findings.md#incidental-bugs)).
- A test whose end the listeners are still handling when its Android device is lost counts with its status but can lack
  the screen capture and profiling files attached as it ends (D70).
- An Android device whose instrumentation fails every time still uses one attempt of each test it takes, and takes its
  own retries back when nothing else is queued for it, so it can still fail tests on `uncompletedTestRetryQuota` while
  the other devices are busy (D82; keeping such a device out of its pool is a possible follow-up).
- When an Android device is lost during a batch with `isCodeCoverageEnabled: true`, the batch's coverage is logged as
  missing at ERROR, although its tests run again and their new batch's coverage is pulled. The exit code is unchanged
  (D81). Its cleanup is no longer also logged as failed (D97).
- Android's GIF listener can still attach a GIF after its capture job failed; it does not go through D97's delivery
  check ([x96 findings](docs/wp/x96-findings.md#verification-limits-and-incidental-bugs)).
- When adb cannot confirm a stopping Android device's cleanup, the device can keep an instrumentation process, a screen
  recording or trace and its files, a reverse port forward or D85's marker (D92).
- Zero-byte Android videos were seen in a real run, also seen in earlier runs and judged a pre-existing recording
  issue; D97 delivers a complete zero-byte file.
- A D88 monitor failure that ends Android device tracking has not been provoked on a real device; it is covered only by
  tests with fake adb.
- The password and askpass path, D90's connection rotation, and network partitions or delayed control replies on a
  worker were not exercised on a real worker.
- With `sshpass` (an SSH client older than OpenSSH 8.4), rsync's ssh runs in a session of its own, so cleanup covers only
  the processes a listing has seen, and one that ends its parent between two listings can survive (D91, D96).
- A process marathon has spawned but not yet registered is outside the reaper's view, and a full reaper table (4,096
  entries) fails the launch. After an abrupt loss of both connections to a worker, the supervisor's private directory and
  its small receipts can remain under the worker's `/tmp` (D96).
- A quarantined worker connection stays open until marathon exits, and with `shareWorkerConnection` it keeps one of the
  worker's two connection slots (D90, D93).
- On a filesystem that rejects rename flags, publishing a bundle checks that the destination is absent and then renames,
  so another writer can win in between, and replacing an existing tree can leave its path absent for a moment; if a
  failed publication also fails to restore the earlier tree, that tree is kept in a named `.marathon-artifact-*`
  backup. A filesystem call already under way cannot be cut by a deadline (D97).
- A Graphite backend that cannot be set up still fails the run, where an InfluxDB one falls back to no history (D71).
- A JUnit3 `TestCase` method that also carries `@Test` is still listed twice, by both discovery rules, as in Kotlin (D75).
- A remote parse cut after other text, before its first status atom, can't be told from output that is not the
  instrumentation protocol: it gives no tests for that bundle and is not retried. With several bundles the run goes on
  without them (D25).
- Apple event/line scheduling can move a boundary line between per-test and batch logs (A20). The Phase 8 differential
  harness gates that interleaving; production retains it ([WP 8.12](docs/wp/8.12-test-changes.md#differential-only-eventline-gates)).

## 0.14.0

This release fixes run-stability and Android, iOS and macOS device-handling bugs found after 0.13.0, with the approved
differences D68–D72 and D74–D81 (D73 was reserved and not used). The inputs, CLI, exit-code policy, result-data and
worker-command contracts are unchanged except as listed below: D72 adds a `flakinessStrategy` type, and the other
differences change which runs happen and what reports show for them, as each entry says. It has the code of 0.14.0-rc.2;
the release candidates' changes are all listed here.

### Check before you upgrade

- **Probability flakiness without history (D71):** with `flakinessStrategy: probability`, history that cannot be read
  (analytics disabled, an InfluxDB backend that could not be set up, or a failed success-rate query until a later
  refresh answers) no longer gives every test `maxCount` extra runs; it adds none and logs one warning saying why. A
  Graphite backend that cannot be set up still fails the run, as before. If a job relied on disabled analytics to run
  every test several times, switch it to `flakinessStrategy: {type: fixed, count: N}` (D72).
- **`fixed` is native-only (D72):** the Kotlin marathon and 0.13.0 reject a Marathonfile that uses `type: fixed`. Remove
  it before rolling back to either.
- **Fewer executions in reports (D69, D70, D75):** a test that has finished is no longer run again, a batch whose
  Android device is lost keeps the tests that finished, and a JUnit3 test with an array-valued annotation in a multi-dex
  APK is listed once. Reports, dashboards and checks that count executions, uncompleted attempts or tests can show lower
  numbers. A run with a small `uncompletedTestRetryQuota` that used to fail after an adb outage can now pass. A strict-mode
  retry that D69 refunds can let another test of the pool be retried, which can change that test's outcome.
- **`fast: false` reports follow the verdict (D77):** with `executionStrategy.fast: false`, the summary's first block, the
  per-test result files, JUnit and HTML show the run that decided a test's verdict (its first pass under `ANY_SUCCESS`,
  its first failure under `ALL_SUCCESS`), not its last run, so they agree with the exit code. A test's place in the JUnit
  and HTML order can move. Read `raw.json` for every run.
- **A failed queue exits instead of hanging (D68):** a run whose test queue fails now ends with exit 1 (0 with
  `ignoreFailures: true`), writes its reports and logs `Marathon run failed` with the cause, where it used to hang until
  killed. A console that refuses a progress line no longer stops the run; that line is lost.
- **Logs and worker checks:** an SSH session closed only after KILL now logs a warning with `debug: false` (U183). An iOS
  unit-test bundle built inside its app is pushed to the worker once, so a wrapper that counts transfers sees one fewer
  (D76). Disposing of an iOS simulator can take up to 70 s longer while it stops a stuck preparation (D74).
- **JUnit `flakyFailure` (D78):** a passed test lists only its runs that did not pass. A step that counted these
  elements as extra runs should read `test_result/raw.json`.
- **iOS videos and durations (D80):** with video recording on a remote worker, tests after the first of a batch now keep
  their videos and real durations, so reports can show more videos and longer times than before.
- **Android coverage (D81):** with `isCodeCoverageEnabled: true`, each instrumentation writes its coverage file under the
  app's external files directory instead of its private data, without `run-as`, so a non-debuggable app now produces
  coverage. The `.ec` files land in `device-files/<pool>/coverage/` as before, one per instrumentation
  (`coverage-<batch>.ec`, then `coverage-<batch>-2.ec` for a second instrumentation of the batch). A missing coverage
  file is an ERROR but does not change the exit code. Each test's result file and Allure list the file as a `COVERAGE`
  attachment with its absolute path on the machine that ran marathon; a tool reading a copied output folder joins the
  part after `/device-files/` onto its copy (see D81 in the deviations guide).

See [Changes that need action](docs/migration.md#changes-that-need-action) for the migration table and
[the deviations guide](docs/deviations.md#d68--a-failed-queue-ends-the-run-a-failed-progress-line-does-not) for each
approved difference. The 0.13.0 and 0.12.0 requirements, including a finite `uncompletedTestRetryQuota`, still apply when
upgrading from an older version.

### Runs and queue

- A run whose test queue fails now ends, instead of hanging, with exit 1 (0 with `ignoreFailures`), writes its reports and logs
  `Marathon run failed` with the cause; its devices stop as at a normal end of the run. A console that refuses a progress line
  no longer stops the run: the line is lost, the test is still reported, and one warning is logged, shown with
  `debug: true` (D68, U168).
- A test that has finished is no longer run again. Dropped are: a strict-mode retry granted to the attempt that
  completed the test, which returns its retry quota; copies of a completed test in a batch whose device left before
  starting it, which keep theirs; and a copy of a completed test that comes back incomplete, which is reported once as
  before. A failed copy beside a retried copy of the same test is now reported too. Reports can show fewer executions
  (D69).

### Android

- **A test that finished is not run again when its device is lost later in the batch** (D70, U197). A device
  lost while a batch runs, during an instrumentation or between two of them, ends that batch with what ran: its finished
  and failed tests count, and only the test in progress and those not yet run are run again, each charged one attempt
  of `uncompletedTestRetryQuota`. Before, the whole batch could come back, so its finished tests ran again and every
  test of it was charged. Reports of such a run can show fewer executions. No configuration change.
- **An adb outage no longer fails tests that never ran** (D70, U160, U166). When an adb server stays away, or a device stays
  `offline`, until its device is given up, a batch the device was waiting to start goes back to the queue without using
  up any of `uncompletedTestRetryQuota`, so with a small quota those tests now run later instead of failing. In a batch of
  several test APKs, each instrumentation now waits for the device to return, and one given up between them keeps the
  results of the part that ran. No configuration change.
- **Devices returning after an adb outage or a brief `offline` no longer delay other devices** (U170). A device
  that came back is checked and has its logcat and reverse port forwards restored alongside the others, up to 8 at a time
  per adb server, so one that does not answer no longer holds up another device's 10 s grace or keeps the rest of its
  server waiting. After an adb server restart each device takes new tests as soon as its own restoring is done. Commands
  and results are unchanged. No configuration change.
- A JUnit3 test with an array-valued annotation, such as Kotlin's `@Throws`, in a test APK with several dex files
  is now listed and run once, not once per re-scan of the dex files that include it (D75).
- **Code coverage works for a non-debuggable app (D81):** with `isCodeCoverageEnabled: true`, coverage used to be
  written to the app's private data and copied out with `run-as`, which only a debuggable app allows; a non-debuggable
  app's run passed with no coverage and no warning. Each instrumentation now writes
  `<external storage>/Android/data/<app>/files/coverage/coverage-<batch>.ec` (`-2`, `-3` … for further instrumentations
  of the batch), which marathon prepares and pulls without `run-as` into `device-files/<pool>/coverage/` as before. A
  coverage file that is missing or cannot be pulled is an ERROR naming the batch, with the exit code unchanged, and each
  test's result file and Allure list its instrumentation's file as a `COVERAGE` attachment.

### iOS and macOS

- iOS: a unit-test bundle built inside its app is no longer pushed to the worker again after the app, saving one transfer
  per simulator setup; the files on the worker are unchanged (D76).
- iOS: disposing of a simulator now stops a preparation still running on it and waits for it, up to 70 s, so a stuck
  preparation no longer sends commands to a simulator that is gone (D74, U179).
- iOS and macOS: worker connections that stayed open until marathon exited, those of a device replaced after a lost worker
  or otherwise, or whose setup or disposal failed, are now closed, at the latest when the run ends (D74).
- A session that only ended after marathon sent KILL now logs a warning, also with `debug: false`, naming the command and
  saying the process may still be running on the worker (U183).
- iOS: a simulator that is shut down or crashes while marathon waits for it to boot, once it has been seen booting, is
  booted again within a poll and the 10 s retry pause, not after `bootWait` (240 s by default). A simulator that stays booting is still waited for until `bootWait`,
  and the readiness-probe times under the known limits are unchanged (D79).
- **iOS videos and durations survive a slow worker link (D80):** with video recording on, a test's recorder stop,
  transcode and pull no longer hold back the handling of the batch's next test. Before, a slow transfer could leave the
  next test without a video and with a reported duration of 0 (JUnit `time`, result file, `raw.json`, HTML), and the
  recorder stop alone shortened each following test's duration by a few seconds on a remote worker (U201). Videos are
  still all pulled and attached before the batch is reported.

### Flakiness and results

- **Flakiness copies without history** (D71): with `flakinessStrategy: probability`, a run whose test history cannot be
  read (analytics disabled, an InfluxDB backend that could not be set up, or a failed success-rate query until a later
  refresh answers) no longer gives every test `maxCount` extra runs; it adds none and logs one warning saying why. A
  Graphite backend that cannot be set up still fails the run. Tests missing from working history still
  use `defaults.successRate`. If you relied on disabled analytics to run every test several times, use `fixed` below.
- **`flakinessStrategy: fixed`** (D72): a new strategy type, `{type: fixed, count: N}`, schedules every test `N + 1`
  times whatever the history says, for stabilization jobs. It is allowed with `executionStrategy.mode: ALL_SUCCESS`, so
  a test passes only if all its runs pass (with `fast`, the default, its first failure ends it); `probability` is still
  rejected there. Under `ANY_SUCCESS`, set `fast: false` to run every copy after a pass. The Kotlin marathon does not
  accept it. The `Raw:` line reflects every run.
- **With `executionStrategy.fast: false`, a test's reported result agrees with its verdict** (D77). The summary's
  passed/failed line, the per-test result file, JUnit and HTML used to show a test's last run, so under `ANY_SUCCESS` a
  test that passed once but failed last was listed as failed although the run exited 0 (and the mirror image under
  `ALL_SUCCESS`). They now show the run `fast: true` would have stopped at: the first pass under `ANY_SUCCESS`, the
  first failure under `ALL_SUCCESS`, otherwise the run that completes the test. Exit codes, retries, the `Raw:` line, `raw.json` and
  Allure are unchanged, and so is everything with `fast: true`.

### Reports

- **JUnit no longer lists a passed test's passing runs as `flakyFailure` (D78):** a test that passed every run, for
  example all runs of `flakinessStrategy: {type: fixed, count: N}`, had an empty `flakyFailure` element per extra run,
  so JUnit tools showed a stable test as flaky. A passed test now lists only its runs that did not pass, with their
  stack traces; a failed test's `rerunFailure` elements are unchanged. A CI step that counted `flakyFailure` elements
  as the number of extra runs should read `test_result/raw.json`, which lists every run.

### Logs

- Clearer log lines, with no change to results or exit codes: a signal or the end of a run during device preparation
  is one WARN line (`Device preparation has been cancelled`) instead of an ERROR that repeated its message; an Android
  batch that fails in an `offline` the device does not return from says the device was disconnected, not that it is
  kept, and names the device; and a file pull cut off by an offline or lost device says so instead of blaming the
  device's sync support.

### Evidence

- Every fix has a witness test that failed with the fix temporarily reverted, as each change's findings document records
  ([x61](docs/wp/x61-findings.md)–[x72](docs/wp/x72-findings.md), [x75](docs/wp/x75-findings.md)–[x79](docs/wp/x79-findings.md)).
  Whole runs use the Stub vendor (D68, D77), the fake adb with the virtual-clock reconnect harness (D70, U170, U197), and
  fakes of the Apple worker and its SSH sessions (D74, D79, D80, U183). Device-level tests order events with gates and
  listener signals; no sleep decides an outcome.
- The JVM recordings are unedited. D69 sets aside exactly the three queue recordings of C45–C47, and a negative test pins
  the steps where they differ. Narrow allowances compare the rest: D77 moves only the final flag of tracked runs in
  `fast: false` rows (22 progress rows and 13 queue flags), D75 drops exactly the 8 recorded repeats of one fixture
  APK, D76 removes exactly one recorded bundle push from two iOS rows, D78 removes only the `flakyFailure` elements of
  passing runs, and two D74 tests assert the JVM's recorded lease value beside the native one instead of equating them;
  negative tests show that none hides anything else.
- No registered native golden changed. One golden was added,
  `fixtures/contract/virtual-runs/android-lost-after-start.json`: the result files of an Android batch lost after it
  started (D70, U197), reviewed as a new file.
- Each change had one or two review rounds by another model, recorded with their findings and fixes in its findings document.
- CI runs the repository gate on macOS and Linux; it passes on the code of this release. The changes of x61–x66 and
  x75–x79 also passed it locally and on a Linux build box before merging. Two CI failures on `main` during the first
  batch were races that only tests could hit, fixed in a test (U198) and in the console's filter reload (U199). Two
  tests that fail rarely under heavy load are recorded as open (U202, U203); neither involves the released binary.
- 0.14.0-rc.1 ran on real hosts: the static Linux build on two API 36 Android emulators (adb server killed and an emulator
  lost mid-batch, brief `offline`, SIGTERM, and D71, D72 and D77 on the real runner), and the macOS build on this
  project's Mac with iOS 18.6 and 26.4 simulators and macOS tests, locally and over SSH to a second Mac as a real worker.
  Healthy runs gave the same per-test outcomes and report data as 0.13.0-rc.1, and the D69–D72, D74, D76 and D77 cases
  that were exercised behaved as documented; D68 and D75 were not reachable there. On the remote worker one test lost
  its video and got a JUnit time of 0 after a slow video transfer, as in 0.13.0. Those runs found what D78–D80 and the
  log changes fix.
- 0.14.0-rc.2 repeated those runs where they had found something. On the emulators a healthy run gave the same per-test
  results, JUnit and `raw.json` as rc.1, with one `COVERAGE` attachment per test and a readable coverage file per batch
  (D81), also for a non-debuggable app, which rc.1 left without coverage. On iOS a simulator shut down on purpose during its
  boot wait was ready after 26 s instead of 255 s (D79). The remote run that had lost a video was repeated over a faster
  link: every test kept its video and a duration equal to its xcodebuild timespan, and no test's handling waited for a
  pull; a pull longer than the next test was not reproduced (D80). Passed tests had no `flakyFailure` for passing runs
  (D78). The changed log lines appeared once, at the documented levels.
- Outside this repository, the maintainer ran 0.14.0-rc.2 on private CI farms against recent runs of the same jobs on the
  JVM build: the macOS build on 80 iOS simulators and the static Linux build on Android emulators, in jobs of up to 1426
  tests. Every job ended with the same passed tests as the JVM runs, two of them after a retry of the job (one after an
  app that did not launch within a test, one after an emulator whose instrumentation kept failing), with wall times
  similar to the JVM runs. Retried runs were mostly within the range of the JVM runs; that emulator's job and a job with
  `probability` flakiness ran more, as Known limits explains. These runs are private and cannot be checked here.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container, and
  both 0.14.0 release candidates ran on real emulators, rc.2 also on a CI farm. musl's resolver asks DNS directly,
  so an adb host given as a name that only the system's own name service knows, such as a `.local` name, is not found;
  use `localhost` or an address.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The differential suites run it with a
  stand-in file, so what the library itself does is not covered by them. The archives do not contain it, and this project's
  licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of that evidence
  ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a disposable environment and keep your
  JVM distribution for [rollback](docs/migration.md#trial-and-rollback).
- An emulator that restarted during an adb outage (D22) repeats its failing batches until `uncompletedTestRetryQuota` runs
  out.
- A cancelled run on a worker whose SSH server ignores signals can leave xcodebuild running there. A close that needed KILL
  logs a warning naming the command, also with `debug: false` (D30, U183); the process itself is not ended.
- A failed readiness probe can cost a simulator 26 to 30 minutes with the defaults while another simulator keeps the pool alive.
  If only the probe hangs and needs KILL it takes about 33 minutes. On the last device the run ends 180 s (D48's wait) after the
  simulator left the pool, or after a test result it reports later, with "remaining tests will not run": about 9 to 11
  minutes after its first boot, or about 3 minutes after a loss if the probe starts failing at a re-setup (D38).
- A failed xcodebuild close retires its simulator, and a pool whose last device it was waits (D48), then ends (D30).
- An Android device that left its pool but not adb's list, after a failed prepare or D49's limit, returns only if the list changes,
  so a pool waiting for it uses up the limit for nothing (D48).
- A test whose end the listeners are still handling when its Android device is lost counts with its status but can lack
  the screen capture and profiling files attached as it ends (D70).
- An Android device whose instrumentation keeps failing before a test starts can take that test back as soon as it is
  returned to the queue, so one device can use up a test's `uncompletedTestRetryQuota` within seconds while other
  devices are free, as in the Kotlin build: Android devices have no worker, so nothing steers a retry to another device.
- With `flakinessStrategy: probability` or `fixed`, the copies of one test are queued together (D60), so with enough
  free devices they all start before one of them passes (or fails, under `ALL_SUCCESS`) and `fast` has none left to
  cancel. Results are the same; the copies cost more device time than in the JVM build, whose heap order interleaved
  copies of different tests.
- When an Android device is lost during a batch with `isCodeCoverageEnabled: true`, the batch's coverage is logged as
  missing, and its cleanup as failed, at ERROR, although its tests run again and their new batch's coverage is pulled.
  The exit code is unchanged (D81).
- A Graphite backend that cannot be set up still fails the run, where an InfluxDB one falls back to no history (D71).
- A JUnit3 `TestCase` method that also carries `@Test` is still listed twice, by both discovery rules, as in Kotlin (D75).
- A remote parse cut after other text, before its first status atom, can't be told from output that is not the
  instrumentation protocol: it gives no tests for that bundle and is not retried. With several bundles the run goes on
  without them (D25).
- Apple event/line scheduling can move a boundary line between per-test and batch logs (A20). The Phase 8 differential
  harness gates that interleaving; production retains it ([WP 8.12](docs/wp/8.12-test-changes.md#differential-only-eventline-gates)).

## 0.14.0-rc.2

The second release candidate of 0.14.0, published on 2026-11-18 as a pre-release for trial runs. It added D78–D81 and
log wording to rc.1; its changes are all in 0.14.0 above.

## 0.14.0-rc.1

The first release candidate of 0.14.0, published on 2026-11-17 as a pre-release for trial runs. Its changes are all in
0.14.0 above.

## 0.13.0

This release removes the JVM artefacts required by Kotlin parity under [PLAN §3.12](PLAN.md#312-phase-8-contract), keeping the
inputs, CLI, exit-code policy, result-data and worker-command contracts except for the approved differences below
(D55–D65). D66 and D67 change how SIGQUIT and console completion are handled. It has the code of 0.13.0-rc.2 plus D67
and further log wording; "Signals, console and logs" below lists all of these changes.

### Check before you upgrade

- **Log parsers:** update diagnostic prefix, target and wording matchers for `tracing`; some provider WARN/INFO lines now need `debug: true`. Per-test progress lines and the complete summary block, including `Marathon run finished` and `\tRaw:`, keep their text format.
- **Failure classifiers:** generated messages and stack traces in reports and logs use plain descriptions instead of Java class names (D59). Update text expectations; external tool text stays verbatim. Hidden `config-dump` consumers must replace `error.class` with `error.kind`.
- **CLI text:** update help, usage and parser-error snapshots for clap (D61). Commands, flags, defaults, boolean meaning, streams and exit codes stay.
- **Allure and annotations:** update `environment.xml` and raw-metadata expectations from identities to simple object names, `[elements]` and `null`; update identity-based `annotationData.valueRegex` filters (D58).
- **Image comparisons:** review screenshot/GIF baselines and refresh affected hashes for colour conversion, channel decoding, resize ties and transparency; multi-frame GIFs have one loop extension (D63). Paths, dimensions, frame timing and report references stay.
- **Test order:** update tie-order, seeded-order and batch snapshots for FIFO ties and insertion-time keys; selector order/grouping and batch counts can change (D60).
- **Late pools:** a run whose late-pool tests were silently missing can now run them, take longer and exit 1 (D65).
- **Serialized text:** compare YAML and JSON as parsed data. Serde writers can change YAML formatting/quoting and JSON numeric/escape spellings, including embedded report JSON (WPs 8.9, 8.4).
- **Text and paths:** update malformed-Unicode, socket-error and rare Unicode sort expectations (D55, D57); path cuts through a supplementary character now stop before it and can be written (D56).
- **Non-ASCII Basic credentials:** check that your analytics server accepts UTF-8 credentials (D57). ASCII credentials stay identical.
- **Small Apple idle limits:** allow for expiry at the configured limit, without the old 20 ms polling margin (D62). Raise the limit if it relied on that margin.
- **Signal wrappers:** SIGQUIT no longer ends a run: it logs a state dump and the run carries on (D66). Stop a run with
  SIGTERM or SIGINT. After a run has finished, a stalled console can hold the exit for up to 60 seconds, and SIGINT/SIGTERM
  are not acted on then; use SIGKILL for a hard stop (D67).

See [Changes that need action](docs/migration.md#changes-that-need-action) for the migration table and
[the deviations guide](docs/deviations.md#d55--text-uses-unicode-characters) for each approved difference.
The 0.12.0 requirements, including a finite `uncompletedTestRetryQuota`, still apply when upgrading from an older version.

### Text, paths and credentials

- Text uses native Unicode strings: lone surrogates become U+FFFD at decoding, supplementary/BMP sorting follows
  Unicode scalar order, and diagnostic abbreviations keep whole characters. Valid-Unicode fragmentation hashes and
  Allure history IDs stay exact; path limits still count UTF-16 units (D55).
- A limit through a supplementary character cuts before it, allowing the shorter path to be written; an empty component
  still fails (D56). Malformed UTF-8 replacement counts and socket diagnostic spelling become native; Basic credentials
  use UTF-8. Device IDs and URL form/query bytes stay (D57).

### Reports and CLI

- Stateless Allure environment objects use simple class names; array annotation values show their elements and decoded
  null shows `null`. Raw metadata and annotation filters use those same values (D58).
- Generated failures use plain descriptions in JUnit, Allure, per-test JSON, HTML and logs; cause order and frame text stay.
  External traces stay verbatim, and hidden `config-dump` exposes stable kind identifiers (D59).
- clap replaces the Clikt text emulation, preserving the command/flag surface, defaults and exit codes (D61).
- Serde replaces the JSON and YAML writer emulations while preserving ordered parsed data, null/omission and scalar types
  (WPs 8.4, 8.9). YAML input still uses the YAML 1.1 reader. A metadata key containing NEL now round-trips without losing
  that character (U184).

### Scheduling, runtime and diagnostics

- Equal-priority tests are FIFO, queue listings follow priority order and keys are computed on insertion. Random key
  assignment can change for a seed; re-added entries can use newer local duration observations with backend history still
  pinned to queue construction. Only queue-derived test-selector order/grouping changes in worker commands (D60).
- Apple output uses async reads and the exact configured idle limit, preserving terminal draining and ready-byte precedence
  (D62).
- Ordinary cancellation has one tag and completion callbacks can no longer be withdrawn; payloads, timeout identity,
  device cleanup, results and exit codes stay (D64).
- The scheduler joins every pool admitted before it finds no next pool, correcting missed late-pool tests and outcomes.
  A run can end later and select a different existing status. Completion seals parent and sibling outcomes before callbacks;
  callback and diagnostic order can move. Per-pool waits and bounded cancellation remain (D65).
- `tracing` replaces logback emulation: regular diagnostics use local time, level and Rust module target, and Apple compact
  output omits time. The thread column is gone. Ordinary levels now consistently follow `debug`; host-key warnings stay
  visible. Other diagnostic wording and debug representations can change (WPs 8.8, 8.11;
  [logging design](docs/design/logging.md)). Progress and summary formats stay pinned.
- Runtime cleanup now delivers unhandled diagnostics, releases first-poll panic cancellation gates, retires replay work
  through destruction, consistently prefers parent cancellation on iOS and drops deep job trees without recursive stack
  overflow (U185–U189, U193). These fixes add no configuration.

### Screenshots and GIFs

- Framebuffer decoding shares native straight-alpha RGBA, converts P3 once to sRGB, honours 16-bit channel headers and
  corrects RGB565 and raw 10-bit output. Nearest resize ties and GIF partial-alpha handling change; GIF loop metadata is
  emitted once (D63, U190, U191). Canonical RGBA8888 decoding avoids losing captures to needless conversion work (U192).
- Ordinary opaque unprofiled 8-bit PNGs and opaque single-frame GIFs with unchanged sampling/geometry have exact byte
  regressions. Other images can change visibly. Names, paths, dimensions, rotations, frame timing, loop value, disposal,
  frame order, worker commands and Allure/HTML references remain as specified by D63.

### Signals, console and logs

- **SIGQUIT no longer ends a run** (D66). It logs a bounded dump of the run's state, at WARN on `marathon_core::diagnostics`:
  the scheduler's phase, each pool's queue and devices, and the tests each device is running and for how long. The run then
  carries on. A wrapper that sends SIGQUIT, waits and then sends SIGTERM gets the dump, then D52's reports and exit status;
  before, the first signal ended the run with no reports. A wrapper that signals the whole process group still ends local
  child commands such as `xcodebuild`, as with the JVM build.
- Ordinary console completion now shares one 60-second deadline for flushing and draining output (D67), avoiding premature flush failures on loaded hosts. Signals during the run keep the one-second output watchdog, immediate second-signal exit and D66 SIGQUIT behavior. After the run finishes, SIGINT/SIGTERM are not acted on during console completion; this existing window grows from about two seconds to up to 60 seconds on a stalled console. A wrapper needing a hard stop then must use SIGKILL.
- Apple batches with no test results now log one warning with the device, batch size, session duration in seconds and exit, launch-failure or timeout reason, visible with `debug: false`. Cancelled runs and partial batches do not produce this warning.
- Diagnostic logs use plain values: run failures no longer say `kotlin.Unit`, cancellations are no longer reported as stuck tests, and Kotlin class renderings are removed from log lines.
- Diagnostic logs describe recoverable batch failures plainly, label startup telemetry values as CLI flags, and show timeout durations in seconds. Stored failures, configuration dumps, results and progress/summary text retain their existing formats.
- A flaky analytics test that sometimes connected to a full accept queue is fixed (U194).

### Evidence

- Phase 8 verification includes native contract goldens, the configuration corpus, historical JVM differential suites and
  focused boundary/runtime tests, with gates on macOS and Linux.
- Reviewed native diffs record [120 identity-value replacements](docs/wp/8.5-golden-diff.md),
  [six generated EOF-name replacements in four report files](docs/wp/8.6b-golden-diff.md), and
  [selector reordering in six views](docs/wp/8.7-golden-diff.md), with unchanged batch membership in those views.
- The [logging audit](docs/wp/8.8-golden-diff.md) keeps all pinned console bytes exact. The
  [image audit](docs/wp/8.16-golden-diff.md) finds no changes in the 18 registered native views; its limited image cases
  are supplemented by independent known-pixel, encoded-byte and transparency tests. The
  [late-pool CLI witnesses](docs/wp/8.19-test-changes.md) check corrected reports, progress, counts and existing statuses.
- 0.13.0-rc.1 ran on real hosts: the macOS build on a Mac with Xcode 27.2, with iOS 18.6, 26.4 and 27.0 simulators and
  macOS tests, locally and over SSH to a second Mac as a real worker; the static Linux build on two API 36 Android
  emulators. Per-test results and reports matched 0.12.0's runs, including SIGTERM and SIGINT cancels.
- 0.13.0-rc.2 repeated a short local iOS run with the same per-test results. A SIGQUIT logged the state dump and the run
  carried on; SIGQUIT then SIGTERM exited 143 with its reports. The warning for a batch without results appeared for a
  killed `xcodebuild` and for a test that hit its output timeout, and not for a SIGINT or a healthy run.
- Outside this repository, the maintainer ran 0.13.0-rc.1 and the JVM build side by side on private CI farms: the macOS
  build on 80 iOS simulators and the static Linux build on 32 Android emulators, with the same passed tests on both
  sides; a smaller iOS job also ran on rc.2. These runs are private and cannot be checked here.
- D67 and the log wording added after rc.2 were tested against fakes and the built binary only, not on real hosts.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container, and
  0.13.0-rc.1 ran on real emulators and a CI farm. musl's resolver asks DNS directly,
  so an adb host given as a name that only the system's own name service knows, such as a `.local` name, is not found;
  use `localhost` or an address.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The differential suites run it with a
  stand-in file, so what the library itself does is not covered by them. The archives do not contain it, and this project's
  licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of that evidence
  ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a disposable environment and keep your
  JVM distribution for [rollback](docs/migration.md#trial-and-rollback).
- An emulator that restarted during an adb outage (D22) repeats its failing batches until `uncompletedTestRetryQuota` runs
  out.
- A cancelled run on a worker whose SSH server ignores signals can leave xcodebuild running there, with only a debug line
  saying how long the close took (D30, U183).
- A failed readiness probe can cost a simulator 26 to 30 minutes with the defaults while another simulator keeps the pool alive.
  If only the probe hangs and needs KILL it takes about 33 minutes. On the last device the run ends 180 s (D48's wait) after the
  simulator left the pool, or after a test result it reports later, with "remaining tests will not run": about 9 to 11
  minutes after its first boot, or about 3 minutes after a loss if the probe starts failing at a re-setup (D38).
- A failed xcodebuild close retires its simulator, and a pool whose last device it was waits (D48), then ends (D30).
- An Android device that left its pool but not adb's list, after a failed prepare or D49's limit, returns only if the list changes,
  so a pool waiting for it uses up the limit for nothing (D48).
- A batch that D22's hold kept waiting is charged an attempt when its server is given up, so with `uncompletedTestRetryQuota: 0`
  tests can fail without having run (U160).
- A queue that ends for a reason other than a metrics failure, such as a broken console, leaves the run hanging (U168).
- A remote parse cut after other text, before its first status atom, can't be told from output that is not the
  instrumentation protocol: it gives no tests for that bundle and is not retried. With several bundles the run goes on
  without them (D25).
- Apple event/line scheduling can move a boundary line between per-test and batch logs (A20). The Phase 8 differential
  harness gates that interleaving; production retains it ([WP 8.12](docs/wp/8.12-test-changes.md#differential-only-eventline-gates)).

## 0.13.0-rc.2

The second release candidate of 0.13.0, published on 2026-11-11 as a pre-release for trial runs. It added D66, the
warning for Apple batches without results and plain log wording to rc.1; 0.13.0 adds D67 and further log wording.

## 0.13.0-rc.1

The first release candidate of 0.13.0, published on 2026-11-11 as a pre-release for trial runs. Its changes are
all in 0.13.0 above.

## 0.12.0

This release fixes bugs of the Kotlin fork, each one an approved difference (D21–D54). Most Kotlin bugs remain; the
[bug ledger](docs/known-kotlin-bugs.md) marks the ones that are fixed. Read
[Changes that need action](docs/migration.md#changes-that-need-action) before you upgrade, and the
[deviations guide](docs/deviations.md) for each change in full.

### Check before you upgrade

- **Breaking: every Marathonfile must set a finite `uncompletedTestRetryQuota`** (D47). `marathon run` and `parse` exit 1
  when it is unset or 2147483647, on every platform, because nothing else ends a run in which a test keeps coming back
  uncompleted. Add this line. As a rule of thumb, not a measurement, use the number of devices + 2 if that is more than 5, and more with flakiness copies:

  ```yaml
  uncompletedTestRetryQuota: 5
  ```

  `maxBatchesWithoutProgress` stays optional and doesn't replace it. See
  [the migration guide](docs/migration.md#a-finite-uncompletedtestretryquota-d47).
- **Device preparation no longer wipes external storage** (D27). Kotlin deleted everything under, for example, `/sdcard`
  on every prepare. A test suite that relies on an empty external storage must now clear it itself. Videos left behind
  by an aborted run, or by a recording that a lost connection cut short in a run that still exits 0, are no longer
  removed by the next one.
- **A cancelled run now takes up to 25 s to exit, with `debug: false` too** (D52). On SIGINT or SIGTERM it writes its reports
  and terminates its devices before it exits with 130 or 143; Kotlin did that only with `debug: true`. Give the job at least
  30 s between SIGTERM and SIGKILL, or the reports may be cut short. A second signal still exits at once.
- **`socketIdleTimeout` is now real milliseconds** (D21). Kotlin read it as seconds, so `PT30S` lasted 30,000 s. It now
  bounds only four requests: the adb server's version, a device's features, the property reads after boot and the removal
  of port forwards at dispose. Remove the setting, or keep it at 30 s or more; `PT2S` now fails those on a slow adb server.
- **xcodebuild on an iOS or macOS worker is sent as `env NSUnbufferedIO=YES xcrun xcodebuild …`** (D32), and sends and
  pulls carry `--timeout=300` (D31, D43). On a host with rsync 3 the worker's `rsync --server` gets the flag too, and so does a wrapper set
  with `RSYNC4J_RSYNC`. A forced command or wrapper on the worker must match `env NSUnbufferedIO=YES xcrun xcodebuild`
  at the start of `SSH_ORIGINAL_COMMAND`, `env` must be on its `PATH`, and the flag must be accepted.
- **iOS 16 and 17 simulators on a slow host or link:** a readiness probe returns about 20 MB and must arrive within `shell`
  (30 s by default), or the probe counts as failed (D38). Raise `shell`. From iOS 18 the probe is about 100 KB (D46).
- **A forced command or filter on an iOS worker must allow `xcrun simctl spawn <udid> launchctl print user/foreground`** (D46),
  the readiness probe of iOS 18 and later, where it was `launchctl dumpstate user/foreground/com.apple.SpringBoard`. iOS 16 and
  17 still send `dumpstate`.
- **A lost iOS worker has a limit now** (D44). A simulator whose worker stays down is no longer tried every 16 ms until the run
  ends: it is tried at `workerConnection`'s pace for `lateJoinCutoff` (600 s) after the first failure, and then stays out. The
  end is noticed when the next pause is over, at 630 s with the defaults. With
  `attempts: 1` and `lateJoin: false` it gets one try. Set `lateJoinCutoff: 0` to wait until the run ends. A devices update
  that removes the simulator and adds it back starts these limits over.
- **A worker silent for 5 minutes during a send can lose simulators for the run** (D31, U163). When rsync reports the stall, the
  send isn't retried, its simulator is retired, and sends waiting behind it to that worker fail at once, so their simulators are
  retired too. Any other failed send keeps its retries, as does any failure within the send's first 285 s, unless the run was held up
  for that long as the failure came.
- **A run that loses all its devices takes longer to fail** (D48). A pool with tests left waits up to
  `deviceInitializationTimeoutMillis` (180 s by default) for a device to return, so a killed emulator alone now exits 1 after
  about 180 s, not 12.5 s, plus whatever the devices leaving still spend cleaning up then. Lower that value if you need it
  sooner; it also bounds the wait for the first device.

### Android

- `socketIdleTimeout` counts milliseconds, not seconds, and bounds only requests with no timeout of their own, so a silent
  screen recording or install still works (D21).
- A run survives an adb server restart: device tracking reconnects, a loopback server is started again, and batches wait,
  as does remote test parsing. A server still away after `waitForDevicesTimeoutMillis` loses its devices, so the run still
  ends (D22).
- A device that goes `offline` for a moment is kept: it gets up to 10 s to return, with batches waiting, instead of being
  disconnected at once, which ended a run on its last device. A killed emulator now reaches the retry path after about
  3 s instead of at once. A device gets at most 5 of these per adb server in a run, and the count is never reset. Either
  limit can run longer while devices that returned are checked one after another: by up to the `shell` timeout (20 s by
  default) for each one that does not answer, and while their logcat and reverse port forwards are restored (D41, U170).
- An adb request rejected or closed before its reply no longer retires a device that is still listed and answers: only
  the batch fails, 3 times per adb server in a run, and the count is never reset (D49).
- A file sync push whose md5 never matches fails after `pushFile`, naming the file, instead of waiting for the batch
  timeout (D23).
- A disconnected Android device can no longer be borrowed, and `networkState` reports `DISCONNECTED` once it is lost, which
  can show in that device's per-test JSON. iOS devices still report `CONNECTED` (D24).
- An instrumentation stream that fails, or stops before its result as it does when the adb connection drops, is reported
  as a test-run failure with its cause and retried, without retiring a healthy device. A remote parsing attempt fails and
  is retried instead of returning only the tests found so far, which could give a passing run with tests missing (D25).
- A remote parse whose instrumentation fails before it has listed every test it announced is retried, and fails the run with
  the reason if it keeps failing. A failing exit code on a complete list is still accepted (D42).
- Test APKs in DEX 040 and 041 are accepted, with every class of a 041 container found (D26).
- In a batch that mixes test APKs, each instrumentation runs only its own tests, the batch reports one result, and a test
  that a later instrumentation did not report no longer carries an earlier one's failure text (D28, U151).
- A folder left on the device by a run that was cut short is removed before a file sync push, so the first batch does not
  read the previous run's files. A folder that can't be removed fails the batch with `Could not delete remote file(s): …`.
  A pushed folder whose name holds a space no longer deletes a sibling when it is removed (U159, N41).

### iOS and macOS workers

- A command that a worker rejects no longer holds one of its SSH session slots until the connection ends. A command that
  gets no reply in 30 s, or a channel the worker opens after its 30 s are up, is closed too, with an error line that names
  it (D29, U154, U157).
- Closing an SSH session takes at most 30 s: TERM, then KILL, then the session is abandoned with an error line, whatever the
  worker does. A server that ignores signals may leave the process running. On iOS an abandoned session retires its
  simulator (D30).
- An rsync send that makes no progress for 300 s is stopped and fails with a clear error, instead of blocking that worker's
  sends for ever, when rsync says so: its own timeout report as a whole line, or rsync 3's status 30, after at least 285 s by
  the bridge's clock. A failure that comes sooner keeps its retries, whatever its text. A large bundle over a slow link is
  unaffected while it keeps moving (D31). A stalled send no longer costs a device 90 sends and about 7.6 hours: it isn't
  retried, the device's prepare fails and it leaves its pool, an iOS simulator for the rest of the run, sends waiting behind it to
  that worker fail at once, and another worker carries on (U163). A dead link ends in about 75 s with the default keep-alive.
- An rsync pull is no longer cut at 30 s, so a big video or result bundle over a slow link arrives, as long as its files keep changing at least once
  every 300 s; a link slower than about 1 KiB/s may not. A pull with no progress for 300 s is stopped, and so are the processes it started that the bridge has
  seen and can still tell apart from later ones, including an ssh left behind by `sshpass` or rsync and one that ignores TERM. Each pull that hangs on a stuck worker costs about 305 to 330 s, and
  nothing bounds the total over a run (D43).
  A receive whose process ended can be held up by a child holding its output, such as a hung ssh, for at most 5 s (D31, U158).
- With password authentication through `sshpass` 1.10, a stalled or stopped transfer whose ssh had not yet reached its
  password prompt used to count as a success, because that sshpass exits 0 when stopped then. The stop now goes to rsync
  and ssh, so the transfer fails as it should; this works with every sshpass version (D31, D43, D54, U181).
- xcodebuild on a worker runs with `NSUnbufferedIO=YES`, as was always intended, so its output arrives as it is produced and
  the output idle timeout measures real silence (D32).
- A worker that shares no key exchange, cipher, MAC or host key algorithm with marathon is no longer retried as if the
  network had failed. The error text is unchanged (D35).
- A worker that drops the connection right after login is handled as a lost connection instead of aborting with
  `IllegalStateException: Not connected`. On iOS the usual reconnect takes over; macOS has no reconnect, so the device leaves
  the run (D36).
- A command that floods one output stream while the other is being read no longer overflows a buffer and loses the
  connection for every command on that worker; the command blocks until it is read, times out or is closed (D37).

### iOS simulators

- A simulator whose setup failed is set up again, up to three times, 30, 60 and 90 s after each failure, instead of being out
  for the whole run (D33), and so is a failed re-provisioning, whether its worker was lost, it was lost with its connection
  up or it was added back (D44). That makes at most four setups that fail with the worker answering. Setups that can't reach
  the worker are left to the worker's connection retry and aren't counted, and removing the simulator and adding it back
  starts the count over. When another simulator's setup on the same worker also fails it, the cap holds but the pauses can
  be shorter. `attempts: 1` with `lateJoin: false` in `workerConnection` turns this off.
- A simulator whose own connection is refused is not tried again, by its own retry or by another simulator's setup, until the
  devices file removes it and adds it back, which gives it one new attempt. Work that started before such a re-add doesn't
  decide for the simulator after it. A simulator that was in the run can return after the late-join cut-off (D44).
- Remote parsing on iOS no longer gets a removed, lost or disposed simulator, which could use up its attempts and fail the
  run at the start. It waits for one that can work (D44).
- A failed or timed-out readiness probe means the simulator is not running yet, on every iOS runtime (D38).
- The readiness probe of iOS 18 and later is `launchctl print user/foreground`, about 100 KB where `dumpstate` returned 10 to
  20 MB, with the same marker line. It may report a booted simulator ready up to one poll earlier. iOS 16 and 17 keep `dumpstate`
  (D46).
- A test name, a test's own assertion text or an application's log line that contains a device failure text, such as
  `Failed to terminate`, no longer aborts the batch. After a prefix such as `XCTAssertTrue failed - ` that test fails and the
  batch goes on, and a simulator is no longer dropped after 3 such batches in a row (D39).
- Three output timeouts in a row with no test passed or failed now set a simulator up again, so a real failure that D39 misses
  no longer keeps a sick simulator in the run. This also reaches a test that stays silent past `testOutputTimeoutMillis`, which
  Kotlin repeated on the same simulator for ever. While another device keeps the pool alive, a simulator that does this after
  each of its 3 recoveries leaves the run after 12 such batches. On a pool's only simulator each recovery must also be ready
  within D48's wait, 180 s by default, or the pool ends sooner (D39, U165).
- A test that hangs past its execution time allowance now fails, with the hang's line as its text, whatever form the allowance
  is printed in. With `1 minute`, which a 60 s allowance prints, Kotlin aborted the batch and repeated the test until CI killed
  the job (D45).

### All platforms

- A Graphite or InfluxDB backend that is down or slow no longer stops a run. With carbon down, a run used to hang at its first
  test result. The failure is now logged once per kind, and sends have time limits, with metrics skipped for 5 minutes after a
  slow failure. The backend's close at the end of a run gets 30 s, so an InfluxDB server that answers its last flush slowly
  can no longer hold the end of a run (D51, U167).
- A Graphite series with no points in the window no longer loses the whole history query (D51, C38).
- A cancelled run writes its reports and terminates its devices whatever `debug` says, so a cancelled CI job has results and leaves
  less behind on workers. SIGINT or SIGTERM stops the tests, writes the reports first, and exits with the same 130 or 143 within
  25 s. A second signal exits at once, however soon after the first; two of the same signal that arrive together count as
  one. Tests no longer bounce after the signal, which added false INCOMPLETE results to the reports (D52, C10, U161).
- A failed reporter or close no longer skips the ones after it, such as a later report or terminating the devices. Each failure
  is logged, and the exit code is unchanged (D51, C11, C25).
- A run whose filters removed every test logs an error line saying what each filter removed, so a mistyped filter is visible. It still exits 0 (D51).
- A pool that loses its last device waits for one to return, instead of ending with its tests unrun: an `adb reboot` on a
  single emulator now exits 0, and an iOS simulator recovered after its worker dropped finds its tests still there. The wait
  is up to `deviceInitializationTimeoutMillis` (180 s by default) from when the pool's last working device went away, which
  includes D22's and D41's waits for that device, or from a test that the device leaving reports after that. It runs on
  while the devices leaving clean up, and while devices come back without getting ready: a device that comes back counts
  only once it is ready. When it is up, the pool stops the devices still getting ready, takes no new ones, and ends once
  every device has finished cleaning up. A device that came back and got ready keeps the pool while it stays; when it
  leaves without a test having ended, the pool ends once it has cleaned up. Any test that ends gives the pool a whole new wait. macOS does
  not wait (D48).
- A history query that fails is no longer final for the run: it is retried in the background a limited number of times, at
  5, 10, 20 s and so on counted from the first failure, so a query that fails later joins the next round. Queues built and
  batches composed afterwards use the history it finds. A run never waits for this. With a failing analytics backend the log
  has more warnings and timing lines. If a query is stuck inside a blocking call, the run moves on after 3 s and writes its
  reports, and the process ends at most 3 s after the run (D34, D52).
- `fixed-size` batching counts only the tests a batch accepts, against both `size` and `durationMillis`, so flaky copies and
  tests of another test APK no longer end a batch early. With two test APKs in sorted order, 1,000 tests at size 10 take 100
  batches instead of 150. A batch rejects at most twice its size. A batch with nothing rejected is unchanged (D50).
- Remote test parsing no longer spends all three attempts on one broken device while another is healthy: after an attempt
  fails on a device, the next one picks among the devices not yet tried, and among all once each has been tried, so a
  run with one device is unchanged. A device that reconnects counts as the same one. It never waits for an untried device.
  A worker that freezes while the test bundle is sent to it is left at about 75 s with the default keep-alive (D54); with
  keep-alive off, the default 180 s `deviceInitializationTimeoutMillis` ends the parse with its timeout error. On macOS, parsing also skips a Mac whose connection is lost, and waits within that limit if no
  Mac is connected (D53, A103, U171).
- An iOS or macOS send now stops when the work it belongs to is cancelled, and any transfer stops when its simulator's or
  Mac's connection is lost, with the same TERM and KILL as a stalled send. A pull is never stopped by a cancellation, so
  artifacts arrive as before. Remote test parsing no longer hangs on a worker that froze while the test bundle was being
  sent to it: with the default keep-alive the attempt fails after about 75 s and the next one uses another device, and with
  keep-alive off `deviceInitializationTimeoutMillis` ends the parse at its limit. Before, the run failed after about 311 s
  even with a healthy second worker. A send waiting behind another to the same worker or destination gives up its wait
  when it is cancelled. A lost connection logs one `send error: the device's connection was lost …` or
  `receive error: …` line (D54, A104).
- A delayed disconnect for an old device no longer removes its replacement with the same serial from the pool (D40). No
  known path triggers this, so you are unlikely to have met it.

### Evidence for these changes

- Everything was tested against fakes and against the recorded behaviour of the Kotlin program.
- D21–D28, D41, D42 and the stream-end fix of D25 also ran on Android emulators (API 33 and 36; D41 and D42 on API 36 only),
  with no real USB or TCP device. The merged Android fixes then ran together on emulators: 41 runs with adb server, adbd and
  emulator kills, reboots, a stale pushed folder and silent video, with no run exiting 0 with tests missing and no hang.
- D48's `adb reboot` and killed-emulator cases and D49's closed requests ran on an API 36 emulator, and the reboot also on
  API 33. D48's iOS recovery ran on the fake worker only.
- D29–D32 and D35–D37 ran against real OpenSSH on loopback.
- D43 ran with real rsync, ssh and `sshpass` against the fake worker on loopback, with openrsync on a Mac and rsync 3 on
  Linux; rsync 3 under `sshpass` was not run. D44 ran on the fake worker only.
- D38 and D46 were measured on iOS 18.6, 26.4 and 27.0; iOS 16 and 17 were not measured. D30's TERM timing was measured on one Mac only.
- D33, D34 and D40 had no real simulator, worker or analytics backend.
- D50 was measured through the real pool and queue on a stub device, with no real test APKs.
- D39's rule is reasoned from Xcode's formats and is not recorded from a real run.
- D52 ran through the built binary with real signals on the fake adb and fake worker, and with a test in flight on an Android
  API 36 emulator in both debug modes. It did not run on a real worker or Mac.
- D45 ran on real hanging tests: an iOS 18.6 simulator under Xcode 27.2, with allowances that print `1 minute` and `2 minutes`.
- D53 ran against the fake adb server and fake workers in each vendor's tests, and through the built binary with two fake iOS
  workers, one stalling its sends. It did not run on real devices.
- D54 ran with stand-in rsync and ssh processes in the bridge's tests, against the fake worker's real SSH keep-alive, and
  through the built binary with two fake iOS workers, one frozen with SIGSTOP, with openrsync on a Mac. It did not run on
  Linux, with rsync 3, or on a real worker.
- The release candidate 0.12.0-rc.1, the same code as this release, ran on real hosts: the macOS build on a Mac with Xcode
  27.2, with iOS 18.6 and 26.4 simulators and macOS tests, locally and over SSH to a second Mac as a real worker; the static
  Linux build on API 36 Android emulators. The results were as expected, including SIGTERM and SIGINT cancels, a simulator
  shut down mid-batch, a killed adb server and a killed emulator. A lost simulator's re-setup (D33, D44) and stalled or
  stopped transfers (D31, D43, D54) did not happen there.
- Outside this repository, the maintainer ran the release candidate and the JVM build side by side on private CI farms: the
  macOS build on 80 iOS simulators and the static Linux build on up to 32 Android emulators, with the same results apart from
  flaky tests seen on both sides. Those farms copy the app to the workers themselves, so marathon's rsync path did not run
  there. These runs are private and cannot be checked here.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container, and the release
  candidate ran on real emulators and a CI farm. musl's resolver asks DNS directly, so an adb host given as a name that only
  the system's own name service knows, such as a `.local` name, is not found; use `localhost` or an address.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The differential suites run it with a
  stand-in file, so what the library itself does is not covered by them. The archives do not contain it, and this project's
  licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of that evidence
  ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a disposable environment and keep your
  JVM distribution for [rollback](docs/migration.md#trial-and-rollback).
- An emulator that restarted during an adb outage (D22) repeats its failing batches until `uncompletedTestRetryQuota` runs
  out.
- A cancelled run on a worker whose SSH server ignores signals can leave xcodebuild running there, with only a debug line
  saying how long the close took (D30, U183).
- A failed readiness probe can cost a simulator 26 to 30 minutes with the defaults while another simulator keeps the pool alive.
  If only the probe hangs and needs KILL it takes about 33 minutes. On the last device the run ends 180 s (D48's wait) after the
  simulator left the pool, or after a test result it reports later, with "remaining tests will not run": about 9 to 11
  minutes after its first boot, or about 3 minutes after a loss if the probe starts failing at a re-setup (D38).
- A failed xcodebuild close retires its simulator, and a pool whose last device it was waits (D48), then ends (D30).
- An Android device that left its pool but not adb's list, after a failed prepare or D49's limit, returns only if the list changes,
  so a pool waiting for it uses up the limit for nothing (D48).
- A batch that D22's hold kept waiting is charged an attempt when its server is given up, so with `uncompletedTestRetryQuota: 0`
  tests can fail without having run (U160).
- A queue that ends for a reason other than a metrics failure, such as a broken console, leaves the run hanging (U168).
- A remote parse cut after other text, before its first status atom, can't be told from output that is not the
  instrumentation protocol: it gives no tests for that bundle and is not retried. With several bundles the run goes on
  without them (D25).

## 0.12.0-rc.1

The release candidate of 0.12.0, published on 2026-11-08 as a pre-release for trial runs. 0.12.0 has the same code; only
its documentation changed.

## 0.11.0

The first public release of marathon-rs, a native Rust rewrite of the marathon test
runner for Android, iOS and macOS.

### What it is

- **The same interface.** It keeps the Marathonfile and Marathondevices formats, the
  command line, exit codes and report formats of the Kotlin fork it was ported from, apart
  from the approved [differences D1–D20](docs/deviations.md). The Kotlin fork's bugs are
  deliberately kept too, and are listed in the [bug ledger](docs/known-kotlin-bugs.md) (D5).
- **No JVM.** Each archive holds one executable. `JAVA_OPTS` and `MARATHON_OPTS` are
  ignored (D2).
- **Two targets.** `aarch64-apple-darwin` for Apple Silicon macOS, and a static
  `x86_64-unknown-linux-musl` for x86_64 Linux on any distribution.

Coming from the JVM distribution, read the [migration guide](docs/migration.md) first. The
differences most likely to need action are user regexes, which now use Rust `regex` syntax
and reject Java-only constructs such as lookaround and backreferences (D6), and crash and
usage telemetry, which is not sent (D1). Configured InfluxDB and Graphite backends stay
active (D4). If your Marathonfile selects `testParserConfiguration: {type: xctest}`, you must
now supply the third-party `libxctest-parser` library yourself, because this program does not
contain it ([D20](docs/migration.md#the-xctest-parser-needs-a-library-you-supply)). The default
`nm` parser is not affected.

### Evidence

- The Kotlin tests are ported, or marked not applicable with a reason
  ([parity matrix](docs/parity-matrix.csv)), and differential suites compare whole runs of
  the JVM and native programs against a fake worker and a scripted ADB server.
- Real end-to-end runs compared native and JVM results for iOS simulators, macOS and an
  Android emulator using this repository's sample apps
  ([results](docs/wp/7.5-findings.md#closure-sweep-results)). They ran on an earlier commit
  and were not repeated on this release's exact commit.
- Outside this repository, the maintainer compared earlier builds with the JVM build on real
  multi-host CI farms: the macOS build on iOS simulators, and this static Linux build on Android
  emulators, with results matching apart from flaky tests seen on both sides. Those runs are
  private and cannot be checked here, and they predate the latest fixes.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests
  macOS 11.0 or later; versions older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine below.
- The static Linux binary is checked at release time with `version` and `--help` in an
  Alpine container. The real Android runs recorded in this repository used a glibc build, and musl's
  allocator and DNS resolver differ from glibc's, so try it on your workload.
- The `xctest` test parser needs a `libxctest-parser` library that you supply (D20). The
  differential suites run it with a stand-in file, so what the library itself does is not
  covered by them. The archives do not contain it, and this project's licence does not cover it.
- The real macOS UI XCTest scenario could not be completed on macOS 27 and is not part of
  that evidence ([U103](docs/found-bugs.md)).
- No isolated shadow or canary results are recorded in this repository. Trial it in a
  disposable environment and keep your JVM distribution for
  [rollback](docs/migration.md#trial-and-rollback).
