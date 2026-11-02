# Fake-ADB scenario backlog

These scenarios run the Kotlin marathon, and later the Rust port, against `tools/fake-adb` (PLAN §6.4). The first one, `android-two-devices` (WP 0.5 part B), covers:
- provider start-up;
- device setup;
- streaming installs on two devices;
- one instrumentation batch: one pass, one failure, and one test ignored locally.

Each Android porting WP has to add the scenarios for the behaviour it ports, recorded first against the Kotlin reference. The list below comes from Muse's audit of part B, ranked by value. The WP column is the owner.

| # | Scenario | Marathonfile delta | Script delta | Validates | Owner |
|---|---|---|---|---|---|
| 1 | `android-video` | `videoConfiguration` on; keep one failing test | `screenrecord …` (hangs until killed), `ps \| grep screenrecord`, `kill -2`, an mp4 file to pull, an `md5 <file>` rule for the `waitForRemoteFileSync` poll | Screen recording, the md5 poll loop, mp4 sync pull, video attachments | 6.6b |
| 2 | `android-screenshot` | `preferableRecorderType: SCREENSHOT`, screenshots on | Framebuffer data (`fixtures/vendor-android/framebuffer/screencap_1.bin`) | The `framebuffer:` service, GIF output | 6.6b |
| 3 | `android-filesync-coverage` | `fileSyncConfiguration` pull (EXTERNAL_STORAGE) and push entries; both `pmClear` true; coverage on | Sync files, `pm clear`, `appops set … MANAGE_EXTERNAL_STORAGE --uid`, `run-as … mkdir`/`tar`, the coverage pull | Sync STAT/SEND/RCV2, `run-as` flows, coverage arguments, `pm clear` | 6.6a, 6.6b |
| 4 | `android-install-matrix` | `autoGrantPermission: true`, `installOptions`; one device without `abb_exec`/`cmd` | `pm list packages` already lists the packages (uninstall path); legacy push + `pm install` + `rm` | Every installer branch (streaming, legacy, uninstall, `-g`, options) | 6.5 |
| 5 | `android-chaos` | – | A timeline device disconnect mid-batch; an instrumentation stream that stalls past `testOutputTimeoutMillis` | `DeviceLost` translation, `ERROR_STUCK`, rescheduling, exit codes | 6.5, 6.6a |

**Later:**
- the remote test parser (`AmInstrumentTestParser`, with the annotation-producer `am instrument` run);
- several `adbServers` (a second fake-adb port);
- an `md5sum`-only device;
- test-access arguments (adb, console and gRPC ports);
- split APKs.

**Comparison harness (WP 7.1).** Muse and Sol note that report normalisation must eventually cover *file contents*, not just paths:
- timestamps, durations and UUIDs inside `raw.json`, the JUnit XML and the HTML data;
- device serials mapped to roles;
- transcript connection ids and interleaving;
- stdout and stderr;
- the state the run leaves on the device.

WP 0.5 part B adds the first semantic snapshot. WP 7.1 completes it.


WP 6.5 records `android-install-matrix` and `android-provider-multi-server` under
`fixtures/fake-adb/`, using the actual Kotlin provider/device/installer. The latter
covers two adbServers and a disconnect after setup; mid-batch failures remain for
6.6a. `crates/marathon-android/tests/scenarios_test.rs` compares their per-device
request sequences and the existing `android-two-devices` preparation prefix.

WP 6.6b records rows 1/2 and the file-sync/coverage listener half of row 3,
plus promoted perfetto profiling, under `fixtures/fake-adb/android-artifacts/`.
`tools/oracle/android-artifact-probes/scenarios` records Kotlin first;
`android_artifacts::kotlin_first_scenarios` compares full ordered services
(excluding transport selections/logcat's independent startup), attachment names
and paths, MP4 bytes, decoded single-frame GIF metadata/content, and trace bytes.
The runner's coverage argv/pm-clear portion remains the 6.6a corpus. The existing
fake does not implement shell process-kill or rm/tar filesystem effects; these
are exact command witnesses, not emulated Android process/final-filesystem proofs.
