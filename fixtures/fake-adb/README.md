# fake-adb scenarios

Differential scenarios for the Android side of PLAN.md §6.4: the Kotlin marathon (and later the Rust one) runs against
`tools/fake-adb` instead of real devices. These files are not copies from the Kotlin repository; they were written for
WP 0.5 part B and recorded with the Kotlin reference build (`tools/oracle`, commit `e47352f2`).

Each scenario directory holds:

| Path | What |
|---|---|
| `in/` | the input directory marathon runs in: the Marathonfile (the fake-adb port arrives as `${FAKE_ADB_PORT}`) and an empty `android-sdk` so that marathon never starts a real adb. The fixture APKs from `fixtures/vendor-android/apk` are copied in at run time |
| `script/` | the fake-adb device script (`devices.yaml`, strict mode) and the files it streams |
| `expected-exit-code` | marathon's exit code for the scenario (1 when a test fails) |
| `expected/transcript.jsonl` | the fake-adb transcript of the recorded run (`docs/design/fake-adb-script.md`) |
| `expected/results.json` | the semantic snapshot: per test its status, the role of the device that ran it and the failure message, and the JUnit totals |
| `expected/report-manifest.json` | marathon's exit code and every path it wrote under `outputDir` |
| `expected/marathon.stdout`, `expected/marathon.stderr` | marathon's output |

`tools/fake-adb/scenario_report.py` normalises everything under `expected/` except the transcript: the run directory
becomes `@RUN@`, the port `@PORT@`, each UUID a distinct stable `<uuid-N>`, JVM identity strings `…;@<hash>`, and log
times, durations and the JVM properties logged at start-up are masked. Serials become roles: `device-1` ran the first
test in `test_result/raw.json`, the other devices follow in serial order, so it doesn't matter which emulator marathon
borrowed (it picks one at random).

Run or re-record a scenario with:

```sh
tools/fake-adb/run-scenario.sh fixtures/fake-adb/android-two-devices [--update]
```

It builds fake-adb, starts it on a free port before marathon, and runs the reference binary through
`tools/oracle/run-isolated.sh`. It fails unless marathon exits with the expected code, fake-adb records no warning,
and (without `--update`) `results.json` and `report-manifest.json` match `expected/`. The transcript and the logs are
kept for the comparison harness but not compared here: the two devices' requests and log lines interleave differently
from run to run.

## android-two-devices

Two emulators (`emulator-5554`, `emulator-5556`, API 33, features `shell_v2, cmd, stat_v2, ls_v2, abb_exec,
sendrecv_v2`). Marathon parses the test APK locally (DEX), installs both APKs on both devices through `abb_exec`
streaming, and runs one batch on one device: `MainActivityTest#testText` passes and `MainActivityAllureTest#testFailure`
fails. `ClassIgnoredTest#testAlwaysIgnored` is ignored without reaching the device: the DEX parser extracts it with
its `@Ignore` annotation, `AndroidDeviceTestRunner.execute` skips it through `isIgnored()`, and reports it locally
through `notifyIgnoredTest`. The scripted instrumentation output therefore holds only the two tests that are sent
(`numtests=2`). Screenshots and video are disabled; there is no file sync.

Recorded result: 1 passed, 1 failed, 1 ignored; marathon exit code 1.
