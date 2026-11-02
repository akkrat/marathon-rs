# Plan review, round 2: Muse (muse-spark-1.3-contributor, xhigh), completeness and feasibility

Source checked: `e47352f2`, 505 main Kotlin files (including buildSrc and sample).

## 1. Files with no clear owner in §7

- **Dead or non-ported code, to be marked `n/a` explicitly:**
  - `apple/cmd/remote/scp/ScpFileBridge.kt`: nothing references it.
  - `sample/*` and `buildSrc` (`Deployment`, `ProjectExtensions`, `Versions`).
  - The buildSrc xcresulttool codegen generator (7 files). Give it to 4.6 or mark it `n/a`.
  - `ios/extensions/ByteBuffer.kt`: a JDK 8 compatibility shim.
- **Vendor wiring:**
  - `core/di/Modules.kt` → 3.10
  - `IosVendor` → 5.5a
  - `MacosVendor` → 5.6
  - `AndroidVendor` and `adam/di/Modules` → 6.5
- **Apple base:**
  - `test/TestEvent`, `TestRequest` → 4.7
  - `AppleTestBundleIdentifier` → 4.8
  - `DistributedDerivedData` → 4.10
  - `device/ConnectionCounter`, `ConnectionFailures` → 4.3
  - `extensions/{ConfigurationExtensions, Duration, Reader, StringExtensions}` → 4.1
  - The tool wrappers hidden behind "…" in 4.4 (`Chmod`, `Codesign`, `Csrutil`, `Getconf`, `Ioreg`, `SwVers`, `Xcodeselect`, `Xcrun`) → 4.4; `Csrutil` is also used by 5.6
  - `ArtifactProcessor` internals → 4.9
- **iOS:**
  - `AppleSimulatorApplicationInstaller` and `AppleSimulatorMediaImporter` → 5.2
  - `device/{Detached, PendingWorker}` → 5.5b
  - `model/{Simulator, Udid}` and `extensions/FileExtensions` → 5.2 / 5.4
- **Android:**
  - `AndroidLogConfigurator` → 1.6
  - models (`AndroidTestBundle`, `AndroidTestStatus`, `TestIdentifier`, `ShellCommandResult` ×2) → 6.5
  - `logcat/{LogcatListener, LogcatProducer}` → 6.6a
  - `adam/log/*` → 6.3
  - `exception/*` → 6.1 / 6.5
  - `BaseAndroidDevice`, `AndroidDevice`, `RemoteFileManager`, `RecorderTypeSelector` → 6.5 / 6.6b
- **Core:**
  - `config/*`, `device/*`, the `execution/*` models and traits, `exceptions/*` (including the Bugsnag and no-op reporters), `extension/*`, `json/FileSerializer`, `time/*`, `log/*`, `integrations/ci/*`, `coroutines/ThreadPoolDispatcher`, `ShutdownHook`, `Vendor`, `execution/command/parse/*`, and the report modules. They need rows in the inventory; 0.6, 3.x and 1.x are the likely owners.
- **Configuration:**
  - `MutableConfigurationFactory`, `ConfigurationValidator`, `analytics/Defaults`, `vendor/apple/DeviceProvider`, `vendor/apple/TestType`, `environment/EnvironmentConfiguration` → 2.x

**Bottom line:** the "no unowned file" rule can't be enforced until `source-inventory.csv` exists. Publish it, with these owners and the `n/a` reasons, before implementation starts.

## 2. Fork change-note behaviours still only partly owned

- The `skipCopyingDerivedData` checks inside the nm and XCTest parsers → 4.8
- A replaced simulator's connection is closed only at the end of the run; a pool whose last device is lost stops → 5.5b
- The late-join re-check before the first batch, and the "provider doesn't see the queue" edge case → 5.5b
- `execution-time` sorting keeps the position a test got when it was queued → 3.2
- The core-side `Timings` lines (provider init, test parsing and device wait, shard preparation, scheduling start) → 3.10
- The log line with the effective xcresulttool timeout → 4.4
- An SSH executor test against a containerised sshd (the `SshjCommandExecutorTest` equivalent) → 0.3a / 4.2
- The `ffmpeg -version` worker-facts probe and the `supportsTranscoding` check → 0.4b / 7.1

## 3. Feasibility of §6.4: what runs on the local host even with SSH transport

- **Local rsync, ssh and sshpass.**
  - The rsync client, its ssh rsh and sshpass run locally (`RsyncFileBridge.kt:49-84,104-139`), and debug mode runs a local `rsync --version` (`:93-97`).
  - Fix: pin the local rsync and OpenSSH versions; the fake server must support password and key auth and have a session limit like `MaxSessions`; normalise the `--version` line.
- **Bundle work happens locally at parse time.**
  - Extraction and validation, the `Info.plist` reads and the Mach-O magic scans (`AppleTestBundle.kt:39-120`) all happen locally.
  - Fix: isolated filesystems per run, with identical fixture bundles.
- **Files staged locally, then pushed.** The xctestrun and dylib are written to local temp files before the push (`TestRootFactory.kt:54-64`, `XCTestParser.kt:60-72`). Fix: compare them through the manifest and the pushed content.
- **Pulls, then a local rename.** xcresult pulls end with a local rename (`ResultBundleRunListener.kt:91-97`). This is covered by the manifest comparison, not by transcripts.
- **ffmpeg runs on the worker** (`ScreenRecordingListener.kt:167-186`), so the fake world must script `ffmpeg`. It must also script `chmod`, `codesign`, `csrutil`, `getconf`, `sw_vers` and `xcode-select`.
- **Both push paths for small files.** `WriteFile` over exec, and the rsync fallback (which `ssh.debug` forces).
- **One path-rewriting rule.** `/tmp/marathon` appears in exec argv, in `WriteFile` payloads and in xctestrun content, not only in rsync. Either one rewriting rule covers all three, or the fake filesystem is rooted at `/tmp/marathon`.
- **Local-transport suite.** It needs a hermetic `PATH`: decide whether it uses real Xcode tools or fakes, and keep the CI version reproducible.

## 4. Factual errors

- None found among the checked claims.
- D2 should cite the Gradle-generated `.bat`, not a checked-in file.
- Record that `ScpFileBridge` is dead code, so the inventory doesn't bring it back.

**Verdict:** revision 2 resolves the round-1 findings it claims to resolve. Phase 0 still needs the inventory and the WP briefs, and §6.4 needs the additions in section 3 above.
