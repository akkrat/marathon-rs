# WP x98 findings: one warning for a refused `run-as`, and the 0.15.0 release docs

## What the trial showed

0.15.0-rc.1 ran on a private CI farm whose Android app is built non-debuggable for CI. Its APP_DATA file-sync entry
then logged `Artifact folder /data/data/<app>/<folder> was not delivered: archive extraction failed` after every test.
Android refuses `run-as` for an app that is not debuggable, so the folder was never readable: Kotlin and 0.14.0 were
refused the same way and collected nothing, silently. D97's extraction command treated the refusal as a failed
extraction, which misled the reader into looking for a broken archive.

## Change

- `file_sync.rs`: the extraction command's check runs inside `run-as` and can only end with 0 (the folder exists) or 44
  (it is absent). Any other status is `run-as` failing, and the command now prints `marathon-app-data-refused` for it
  instead of running the copy and printing nothing. A copy that fails still prints nothing and keeps its
  `archive extraction failed` warning.
- `device.rs`: the device remembers which applications' refusals it has reported. The first refusal per device and app is
  a WARN naming the folder, the app and the reason (`run-as` works only for a debuggable app); later ones are DEBUG
  lines. The listener is created per batch, so the memory lives on the device. A device object made again after a
  disconnect (D24) reports once more.
- Nothing is deleted from the app on a refusal, as before (only the extraction marker permits it).
- `docs/deviations.md`, D97: one sentence on the refusal report.

## Tests

- `a_refused_run_as_is_reported_once_per_device_x98`: two batches on one fake-adb device with a refusing app give the
  same commands per batch, one WARN and one DEBUG line, and no `archive extraction failed`.
- `the_app_data_command_tells_a_refusal_from_an_absent_folder_x98`: replays the device command with `bash` and a
  stand-in `run-as` on `PATH`. A refusing `run-as` (exit 1, as Android's) gives `marathon-app-data-refused` whether the
  folder exists or not; a debuggable one gives `marathon-app-data-absent`, then `marathon-app-data-extracted` with the
  file copied. With the previous command the refusing cases print nothing, so the test fails.
- The existing file-sync tests take the new command text through `extract_fixture_app_data`.

## Release docs

- `CHANGELOG.md`: `## 0.15.0-rc.1` became `## 0.15.0` with the x98 entry under Android, the rc.1 real-device runs and
  the private farm trial as evidence, and a known limit for the iOS recorder state the real runs found; a short
  `## 0.15.0-rc.1` section points to it.
- `README.md`, `docs/deviations.md`, `docs/migration.md`, `docs/releasing.md`: 0.15.0 is the current release.

## Incidental bugs

- iOS: stopping a simulator's screen recorder before it has printed that recording started can leave the simulator
  refusing every later recording until it is restarted. Marathon logs that recording started before that point. Left
  for a later change (wait for the recorder's start, or take such a simulator out of use); recorded as a known limit.
