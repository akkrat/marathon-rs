# WP x98 findings: one warning for a refused `run-as`, and the 0.15.0 release docs

## What the trial showed

0.15.0-rc.1 ran on a private CI farm whose Android app is built non-debuggable for CI. Its APP_DATA file-sync entry
then logged `Artifact folder /data/data/<app>/<folder> was not delivered: archive extraction failed` after every test.
Android refuses `run-as` for an app that is not debuggable, so the folder was never readable: Kotlin and 0.14.0 were
refused the same way and collected nothing, silently. D97's extraction command treated the refusal as a failed
extraction, which misled the reader into looking for a broken archive.

## Change

- `file_sync.rs`: the extraction command keeps the output of its folder check (`run-as <app> sh -c '[ -d … ] || exit
  44'`). When that check fails with another status and its output says `not debuggable`, as Android's `run-as` says in
  every version, the command prints `marathon-app-data-refused` instead of printing nothing. Any other failure (an
  unknown package, a missing `run-as`, a shell error) and a copy that fails after the folder was found still print
  nothing and keep the `archive extraction failed` warning.
- `device.rs`: the device remembers which applications' refusals it has reported. The first refusal per device and app is
  a WARN naming the folder, the app and the device; later ones are not logged. The listener is created per batch, so the memory
  lives on the device. A device object made again after a disconnect (D24) reports once more. Like other Android
  warnings, it shows only with `debug: true`.
- Nothing is deleted from the app on a refusal, as before (only the extraction marker permits it).
- `docs/deviations.md`, D97: one sentence on the refusal report.

## Tests

- `a_refused_run_as_is_reported_once_per_device_x98`: two batches on one fake-adb device with a refusing app give the
  same commands per batch, exactly one WARN, and no `archive extraction failed`.
- `the_app_data_command_tells_a_refusal_from_other_failures_x98`: takes the command marathon sent to fake adb and
  replays it with `bash` and a stand-in `run-as` on `PATH`. A `run-as` that refuses as Android's does (`package not
  debuggable`, exit 1) gives `marathon-app-data-refused` whether the folder exists or not; an unknown package, a missing
  `run-as` and a refusal only of the copy give no marker; a debuggable one gives `marathon-app-data-absent`, then
  `marathon-app-data-extracted` with the file copied. Every marker comes with exit status 0, which marathon requires, and
  a refusal of the copy exits nonzero.
- Both tests fail with the product code of `origin/main` restored (checked).
- The existing file-sync tests take the new command text through `extract_fixture_app_data`.

## Review

Sol, round 1 (all taken):

| # | Finding | Resolution |
|---|---|---|
| 1 | Any status other than 0 or 44 counted as a refusal, including a missing `run-as`, a shell error and an unknown package | The command checks `run-as`'s own "not debuggable" message; other failures stay failed extractions. A refusal only of the copy, after the check passed, stays a failed extraction (Android cannot refuse one call and allow the other for the same app). The folder path stays unquoted, as in every other file-sync command; quoting them all is out of scope. |
| 2 | Notes overstated visibility: Android warnings show only with `debug: true`, later refusals still logged at DEBUG, reconnects warn again | Later refusals are no longer logged; the changelog says `debug: true` and the reconnect case |
| 3 | The shell test replayed the test helper's string, not the command marathon sent | It now replays the command captured from fake adb, with cases for an unknown package, a missing `run-as` and a refusal of the copy |
| 4 | Evidence wording: Allure and per-test JSON differ in video references; xcodebuild first absent at +5.2 s; "only new message"; CI on the release commit | Wording follows the results files; CI must pass on the release commit before the tag |
| 5 | Known limit named the wrong recorder phase | Now "after it was launched but before `simctl` confirmed that recording had started", observed on the SSH worker |

Sol, round 2 (no blocking findings; all taken):

| # | Finding | Resolution |
|---|---|---|
| 1 | The warning said "on this device" without naming it | It names the device's serial |
| 2 | The replay ignored the exit status, which marathon requires to be 0 for a marker | Every marker case asserts status 0; the copy refusal asserts a failed status |
| 3 | `debug: true` was attached to the rc.1 sentence | The changelog says both show only with `debug: true` and a reconnected device instance can warn again |
| 4 | The notes say CI passes on the release code | Kept: the tag is made only after CI passes on the exact commit to be tagged |

## Release docs

- `CHANGELOG.md`: `## 0.15.0-rc.1` became `## 0.15.0` with the x98 entry under Android, the rc.1 real-device runs and
  the private farm trial as evidence, and a known limit for the iOS recorder state the real runs found; a short
  `## 0.15.0-rc.1` section points to it.
- `README.md`, `docs/deviations.md`, `docs/migration.md`, `docs/releasing.md`: 0.15.0 is the current release.

## Incidental bugs

- iOS: stopping a simulator's screen recorder before it has printed that recording started can leave the simulator
  refusing every later recording until it is restarted. Marathon logs that recording started before that point. Left
  for a later change (wait for the recorder's start, or take such a simulator out of use); recorded as a known limit.
