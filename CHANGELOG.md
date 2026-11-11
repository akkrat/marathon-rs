# Changelog

The section for a version becomes the text of its GitHub release, which carries the date.

## Unreleased

- Diagnostic logs describe recoverable batch failures plainly, label startup telemetry values as CLI flags, and show timeout durations in seconds. Stored failures, configuration dumps, results and progress/summary text retain their existing formats.

- Ordinary console completion now shares one 60-second deadline for flushing and draining output (D67), avoiding premature flush failures on loaded hosts. Signals during the run keep the one-second output watchdog, immediate second-signal exit and D66 SIGQUIT behavior. After the run finishes, SIGINT/SIGTERM are not acted on during console completion; this existing window grows from about two seconds to up to 60 seconds on a stalled console. A wrapper needing a hard stop then must use SIGKILL.

## 0.13.0-rc.2

Second release candidate of 0.13.0. It contains everything in 0.13.0-rc.1, which ran without regressions on real iOS,
macOS and Android hosts (see its section in CHANGELOG.md or its GitHub release), plus the changes listed here. Upgrading from 0.12.0 needs the checks in 0.13.0-rc.1's
"Check before you upgrade".

- Apple batches with no test results now log one warning with the device, batch size, session duration in seconds and exit, launch-failure or timeout reason, visible with `debug: false`. Cancelled runs and partial batches do not produce this warning.

- Diagnostic logs use plain values: run failures no longer say `kotlin.Unit`, cancellations are no longer reported as stuck tests, and Kotlin class renderings are removed from log lines.

- **SIGQUIT no longer ends a run** (D66). It logs a bounded dump of the run's state, at WARN on `marathon_core::diagnostics`:
  the scheduler's phase, each pool's queue and devices, and the tests each device is running and for how long. The run then
  carries on. A wrapper that sends SIGQUIT, waits and then sends SIGTERM gets the dump, then D52's reports and exit status;
  before, the first signal ended the run with no reports. A wrapper that signals the whole process group still ends local
  child commands such as `xcodebuild`, as with the JVM build.
- A flaky analytics test that sometimes connected to a full accept queue is fixed (U194).

## 0.13.0-rc.1

Release candidate of 0.13.0: removes the JVM artefacts required by Kotlin parity under [PLAN §3.12](PLAN.md#312-phase-8-contract), keeping the inputs, CLI, exit-code policy, result-data and worker-command contracts except for the approved differences below (D55–D65).

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
- Real-device and farm runs of **this release candidate are pending**. Their results will be added before 0.13.0.
  The real-host and private-farm evidence under 0.12.0 below applies to that release, not to this rc.

### Known limits

- There is no Windows, Intel Mac, Linux on ARM or universal build. The macOS binary requests macOS 11.0 or later; versions
  older than the build and test hosts are not tested.
- The macOS binary is ad-hoc signed and not notarised; see the note on quarantine in the
  [migration guide](docs/migration.md#install-the-native-entry-point).
- The static Linux binary is checked at release time with `version` and `--help` in an Alpine container, and the release
  candidate of 0.12.0 ran on real emulators and a CI farm; this rc's trials are pending. musl's resolver asks DNS directly,
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
