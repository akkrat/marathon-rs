# WP x79: Android code coverage without `run-as` (D81, C89)

Base: `573a962d` (tag 0.14.0-rc.1). Requested 2026-11-17 by the project owner, from an issue filed against the Kotlin
marathon. Deviation D81; Kotlin bug C89.

## Problem

With `isCodeCoverageEnabled: true` the Android vendor relies on `run-as <app>` twice:

- before each batch the runner runs `run-as <app> mkdir /data/data/<app>/coverage` and passes
  `coverageFile=/data/data/<app>/coverage/coverage-<batch>.ec` with `coverage=true`;
- after the batch an APP_DATA file-sync entry `coverage` (aggregation POOL) copies that folder out with `run-as … tar`.

`run-as` only works for a debuggable app. A CI build of a production-like app is often not debuggable, and making it
debuggable changes the build under test. Every step only logs and carries on, so the run is green and has no coverage.
The issue also asked for a loud failure and for each test's result to name its coverage file.

## Reproduction

API 36 emulator (Google APIs, arm64, private AVD and adb server on this Mac). A minimal app with JaCoCo offline
instrumentation (AGP 9.3 `enableAndroidTestCoverage`); AGP forces `debuggable=true` with coverage on, even over a
build-type manifest, so the built app's manifest flag was set to false and the APK re-signed with the same debug key.
Three tests, `fixed-size` batches of 1.

- `adb shell run-as com.example.cov mkdir …` → `run-as: package not debuggable: com.example.cov`.
- 0.14.0-rc.1 run: exit 0, 3 passed, `device-files/omni/coverage/` empty, no WARN or ERROR line. The runner's logcat:
  `FileNotFoundException: /data/data/com.example.cov/coverage/coverage-<batch>.ec: open failed: ENOENT`.

The androidx test runner (1.7.0, `InstrumentationCoverageReporter.dumpCoverageToFile`) opens the path with a plain
`FileOutputStream`: it creates no directory, so the directory must exist and be writable by the app's UID, since the
instrumentation runs in the app's process.

Candidate locations, tried on the emulator:

| Location | Result |
|---|---|
| `/data/data/<app>/coverage` | needs `run-as` to create and read |
| `/data/local/tmp` | the app's process cannot write there |
| `/sdcard/Android/data/<app>/files/coverage`, created by the shell | the folder belongs to `shell`, mode 2770, group `ext_data_rw`; the app gets EACCES |
| the same after `chmod 777` | the app writes, but its file is `app:app 0660` (setgid lost) and the shell cannot read it |
| the same after `chmod o+rwx` (setgid kept) | the app writes `app:ext_data_rw 0660`; the shell (in `ext_data_rw`) pulls and removes it ✔ |
| nothing created beforehand | starting the app's process does not create `Android/data/<app>` |

## Fix

New `artifact_listeners/coverage.rs` (no Kotlin source):

- `CoverageFile` per instrumentation: `<external storage>/Android/data/<app>/files/coverage/coverage-<batch>.ec`; the
  n-th instrumentation of a batch (D28, n ≥ 2) gets `coverage-<batch>-<n>.ec`, because two instrumentations of the
  same app used to write one name.
- The runner passes it as `coverageFile` and, after `pm clear` and after the listeners' setup hooks (file sync removes
  and recreates its pull folders there, which can enclose this one; review S-x79-1), just before sending the
  instrumentation, runs one shell
  command: `rm -rf <dir>; mkdir -p <dir> && { chmod o+rwx <app dir> <app dir>/files <dir> 2>/dev/null; true; }`. Its
  exit code is `mkdir`'s: the `chmod` fails harmlessly on folders the app already owns and on storage that derives
  permissions (API 29 and older), and toolbox `chmod` on API 21–22 has no symbolic modes, where it is not needed either.
  A non-zero exit code or a failed command is an ERROR; the tests still run.
- New listener hook `on_coverage_in(file)`, called by the runner after each instrumentation that was sent, before the
  next one's `pm clear` and before `after_test_run`. `CoverageTestRunListener` (installed whenever coverage is on,
  outside the test-injection slots) pulls the file over sync into `device-files/<pool>/coverage/` (the folder the
  file-sync entry used), removes the folder on the device, and attaches the file as `AttachmentType::Coverage`
  (`COVERAGE`, name `coverage`) to every test started since the last pull, plus the merged name of a parameterised test,
  which `TestResultsListener` looks attachments up by. A missing, empty or unpullable file is an ERROR:
  `Code coverage of batch <id> is missing: <path> was not written or could not be pulled (<reason>)`.
- The hook runs when the instrumentation ended normally or failed, not when the batch was cancelled or never sent. Its
  only error is cancellation or a lost transport on the cleanup command; after a failed instrumentation its error is
  dropped so the instrumentation's error wins.
- `effective_file_sync` no longer adds the coverage entry, so coverage uses no `run-as` at all, debuggable apps included.
- `AttachmentType::Coverage` is rendered as `COVERAGE` in per-test JSON and as `Coverage`
  (`application/octet-stream`) in Allure, which takes the absolute path as other attachments do.

## Contract changes (PLAN §3.12)

- Commands sent to workers: the coverage `run-as` commands go; the prepare and remove commands and one sync pull per
  instrumentation come instead. The `coverageFile` argument (§3.9) changes value; `coverage true` is unchanged.
- Result data: per-test result files and Allure gain a `COVERAGE` attachment. `raw.json` is unchanged (it has no
  attachments and no batch id; see Open questions).
- Logs: new ERROR lines when coverage is missing.

## Tests

- `android_runner::coverage_is_written_to_shared_storage_pulled_and_attached_without_run_as_d81`: the whole executor
  with `pm clear`: order `pm clear` → prepare → `am instrument … -e coverageFile <shared path>` → remove; no `run-as`;
  the local file's content; one `COVERAGE` attachment on the result.
- `android_runner::missing_coverage_is_an_error_and_attaches_nothing_d81`: one ERROR with the path, no attachment.
- `android_runner::a_coverage_directory_that_cannot_be_created_is_an_error_d81`: a failed prepare is its own ERROR.
- `android_runner::each_instrumentation_of_a_batch_writes_and_pulls_its_own_coverage_file_d81_d28`: the whole
  executor with two instrumentations of the same app and `pm clear`: per instrumentation `pm clear` → prepare →
  instrument → remove, the first done before the second's `pm clear`; two local files; each result names its own.
- `android_runner::coverage_is_prepared_after_a_file_sync_setup_that_contains_it_d81`: a file-sync pull of
  `Android/data/<app>/files` is set up before the coverage folder is prepared.
- Both review regressions were checked against mutations: preparing before the listener hooks fails the second, and
  skipping the pull of a batch's non-last instrumentation fails the first.
- `android_artifacts::coverage_is_attached_to_every_started_test_and_merged_parameterised_result_d81` and
  `coverage_file_names_and_commands_d81`.
- `android_runner::request_escapes_only_ascii_spaces_and_keeps_null` now checks the new `coverageFile`.
- Rewritten: Kotlin's `BaseAndroidDeviceTest.testCodecovConfiguration` pinned the APP_DATA entry; it is now
  `device_test::coverage_is_not_a_file_sync_entry_d81` and its parity row is `n/a` (`x79: D81`).
- Allowance: `android_artifacts::kotlin_first_scenarios` drops the recorded Kotlin coverage rows (shell commands that
  name `coverage` and the two sync sessions of the coverage pull) from the `filesync-coverage` reference; the fixture is
  unchanged.

## Verified on a real emulator (API 36)

| Run | Result |
|---|---|
| 0.14.0-rc.1, non-debuggable app with JaCoCo | exit 0, 3 passed, 0 `.ec`, no WARN/ERROR (the bug) |
| this branch, same app | exit 0, 3 passed, 3 `.ec` (328 bytes, JaCoCo header `01c0c010`, `com/example/cov/Calc` inside); each per-test JSON has a `COVERAGE` attachment for its own `testBatchId`; the device folder is removed after each pull |
| this branch, `applicationPmClear` and `testApplicationPmClear` on | 3 `.ec`, no ERROR |
| this branch, debuggable fixture app (`app-debug.apk`) | `.ec` per batch, as before |
| with a file-sync pull of `Android/data/<app>/files` (S-x79-1) | before the review fix: 0 `.ec`, 3 ERROR lines; after it: 3 `.ec`, no ERROR |
| this branch, non-debuggable app built without JaCoCo | exit 0, 3 ERROR lines (`Code coverage of batch … is missing: … because it doesn't exist`), no `.ec`, no `COVERAGE` attachment |

## Not verified

- API 30–35 and API 29 and older on real devices; only the API 36 image is installed here. API 30+ share the
  shared-storage layout seen on 36; on 29 and older shared storage derives ownership from the path, so the app owns
  the folder whatever created it.
- A real batch with two test APKs (D28); covered by fakes only.
- Work profiles and secondary users: `$EXTERNAL_STORAGE` is the shell's user 0 storage.

## Open questions for the owner

- `raw.json` lists every run (retries included) but has no batch id, so a retried run's coverage can only be found from
  the per-test file of the final run, or from the log file name. Adding `testBatchId` (or the coverage path) to each
  `raw.json` entry is a result-data change touching many goldens; not done here.
- The ERROR does not change the exit code. A flag that fails the run when coverage is missing would be a new
  configuration field.

## Review round 1 (Sol, gpt-6.1-sol)

Verdict: merge after fixes. Both findings taken.

- S-x79-1 (Medium): an external-storage file-sync pull of `Android/data/<app>/files` removed and recreated that folder
  in its `before_test_run` setup, after the coverage folder had been prepared in `clear_data`, so the instrumentation
  could not write its file. The coverage folder is now prepared after the listeners' setup hooks, still after
  `pm clear`. Regression `coverage_is_prepared_after_a_file_sync_setup_that_contains_it_d81`.
- S-x79-2 (Low): the D28 test used a no-op listener and checked only the file names. It now runs the real executor and
  coverage listener and checks pulls, order against `pm clear`, and each result's attachment.

Sol also checked: the error folding keeps `after_test_run`; attachments reach ordinary, merged parameterised and
started-but-incomplete results; JSON and Allure handle `COVERAGE` and HTML only filters videos; no new blocking I/O on
the core executor. It agrees that `testBatchId` in `raw.json` is a separate follow-up.

## Incidental bugs

None outside the Kotlin marathon. (AGP forcing `debuggable=true` with `enableAndroidTestCoverage` is documented AGP
behaviour, not a bug.)
