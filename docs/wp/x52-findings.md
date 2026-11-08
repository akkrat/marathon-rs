# x52 — release 0.12.0

Documentation only. 0.12.0 has the code of its release candidate 0.12.0-rc.1 (commit 21c4d67b). This pass records what the
release candidate's real runs showed and turns the changelog's section into 0.12.0's.

## The release candidate's real runs

Each ran the archive the release workflow built, checked against its `SHA256SUMS`.

| Where | What ran | Result |
|---|---|---|
| A Mac with Xcode 27.2, local transport | iOS unit and UI tests on 18.6 and 26.4 simulators, with video; macOS unit tests; SIGTERM and SIGINT mid-test; one of two simulators shut down mid-batch | As expected. The readiness probe was `launchctl print user/foreground` (D46). Cancels exited 143 and 130 with the finished tests in the reports (D52). The interrupted test was retried and all passed |
| The same Mac with a second Mac as an SSH worker | Unit and UI tests on one and two simulators, remote `nm` parsing, SIGTERM mid-test, a simulator shut down mid-batch | As expected. Every send carried `--timeout=300` (D31), xcodebuild ran as `env NSUnbufferedIO=YES xcrun xcodebuild` (D32). Both Macs use openrsync. xcodebuild booted the shut-down simulator again by itself, so D33 and D44 did not run |
| A Linux host with API 36 emulators | The static Linux build: the sample's tests with the local and the remote parser, video and file sync, `parse`, a killed adb server, a killed emulator, SIGTERM | The same results as the glibc build's earlier runs. Memory stayed about 30 MB over a 166 s run |
| Private CI farms, beside the JVM build | 80 iOS simulators; up to 32 Android emulators on 4 hosts | The same results apart from flaky tests seen on both sides. The farms copy the app themselves, so rsync did not run |

## Found by these runs

- [U183](../found-bugs.md): a worker that ignores TERM and KILL but confirms the close gives no error line, and its process
  can keep running. D30 now says so.
- [A105 and A106](../known-kotlin-bugs.md), recorded in x50's follow-up: a local copy leaves the previous app's files in the
  shared app, and the `xctest` parser's discovery can run on a simulator that isn't booted yet. Both are Kotlin's.
- musl's resolver asks DNS, so an adb host given as a `.local` name that only the system's name service knows is not found.
  The changelog's Known limits now says so.
- Not a bug: one note said `marathon parse` exited 0 after `Marathon execution failed`. The CLI returns 1 there
  (`crates/marathon-cli/src/application.rs`, the `Ok(false)` arm), as Kotlin's `PrintMessage` does, and
  `crates/marathon-cli/tests/run_outcome.rs::run_failures_exit_1` covers that path. The 0 was most likely the status of a
  pipeline the command ran in.
- Seen on the farms and not marathon's: a GitLab cancel ends the job about 3 s after SIGTERM, so D52's cleanup of up to 25 s
  can't finish there; the JVM build is cut off the same way.

## Changes

- `CHANGELOG.md`: the section is 0.12.0's; its evidence and known limits now include the runs above. A short 0.12.0-rc.1
  section follows it.
- `README.md`: the release paragraph names 0.12.0.
- `docs/deviations.md`: D30's sentence on a close that succeeds while the process runs on.
- `docs/found-bugs.md`: U183.
