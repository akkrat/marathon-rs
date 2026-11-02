# Plan review, round 1: Muse (muse-spark-1.3, high), coverage audit

## 1. Files and features with no work package, or only a vague one

| Area | Gap | Proposed WP |
|---|---|---|
| Koin wiring | `di/Modules.kt` (core + adam) not mentioned | 3.10 / 6.6 |
| Internal analytics | `ExecutionReportGenerator`, `Summary`, `PoolSummary` | 3.4 |
| Per-test logs | `report/logs/LogWriter`, `LogProducer` | 3.9 |
| Time | `time/Timer`, `SystemTimer` | 0.3 |
| Config I/O | `environment/*`, `FileListProvider`, `RegexSerializer`, `SerializeModule` | 2.1 |
| SCP path | `cmd/remote/scp/ScpFileBridge` | 4.3 |
| sshj patches | `PatchedSSHClient`, `PatchedExecChannel`, `PerformanceDefaultConfig` | 4.2 |
| Tool environment | `bin/AppleBinaryEnvironment` | 4.4 |
| Device base | abstract `apple/AppleDevice` | 5.2 |
| Dylibs | `libxctest-parser` dylibs: how they are shipped | 4.8 |
| iOS helpers | `ios/extensions/ByteBuffer`, `RecorderTypeSelector` | 5.5a / 5.3 |
| Android | `DeviceStateTracker`, `ApkParser` + `InstrumentationInfo` | 6.6 / 6.5 |
| JS bundle | `app.min.js` rebuild vs embed | 3.9 / 7.3 |

## 2. Fork change-note behaviours with no explicit WP owner

- `maxBatchesWithoutProgress`
- No `cp -R` of the xctest into `PlugIns` (macOS still copies)
- `xcresulttool get --legacy` (Xcode ≥ 16) belongs in 4.4
- Lost-worker teardown in parallel, outside the provider loop; an update leaves devices to their recovery
- `Timings` lines in Apple startup
- macOS cancelled-batch wait (1 min + 10 s)
- `provisioningConcurrency`
- `SharedTransfers` pushes once per worker and reuses the copy after an existence check

## 3. Factual errors claimed

- "plist `Dictionary` is backed by a BTreeMap" — **rejected**: plist 1.10 depends on `indexmap` and has `sort_keys`; the claim cites the 0.x API.
- §3.1 log pattern incomplete. Startup uses the CLI logback pattern — **adopted**. "Android uses the CLI pattern" — **rejected**: `AndroidLogConfigurator` has its own pattern.
- The exit message for failures with `ignoreFailures` is missing — it was already listed; it is now spelled out in §3.1.

## 4. Ecosystem notes

- **russh:** it has no `~/.ssh/config` or known_hosts file handling, so the verifier has to be written. Passphrase-protected keys and ssh-agent need explicit wiring. Pin the version.
- **saphyr-parser:** it is YAML 1.2, so the 1.1 rules have to be written by hand. Cap alias expansion.
- **fancy-regex:** it has no possessive quantifiers, atomic groups, `\G` or POSIX classes, and its `\d` is Unicode. Set its backtrack limit explicitly.
- **plist:** it reads XML and binary, but not OpenStep.
