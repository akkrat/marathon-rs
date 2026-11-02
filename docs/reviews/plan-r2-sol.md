# Plan review, round 2: Sol (gpt-6.1-sol, high), on revision 2

**Verdict:** revision 2 is not yet acceptable as the implementation baseline. Seven round-1 findings are only partly resolved, including the blocking worker-isolation one.

## Status of the round-1 findings

**Resolved:** 2 (regex), 3 (lifecycle), 6 (test inventory), 7 (validation stages), 8 (devices mapper), 10 (`timeLimit`), 11 (dynamic protocol), 12 (substitution), 13 (CLI), 15 (heap and model), 16 (ownership), 17 (analytics), 18 (SSH spike), 19 (Android contracts), 20 (artifacts), 22 (macOS end-to-end), 24 (review closure), 25 (artifact details).

**Partly resolved:**

| # | Topic | Fix requested |
|---|---|---|
| 1 | Shadow isolation | Disjoint workers, or sequential runs that restore the worker filesystem and device state |
| 4 | Determinism | Ordered event delivery, actor-quiescence barriers, identical random draws |
| 5 | Fake toolchain | Host-process transcripts, valid local bundle fixtures, an executable local-transport harness |
| 9 | YAML/Jackson | The blanket explicit-null rejection is wrong: distinguish primitive, reference and nullable fields |
| 14 | Logging | Levels are set on every lookup, not only at creation; ANSI on redirected output depends on Jansi's mode |
| 21 | Oracle controls | List the hook sites; separate wall, monotonic and scheduler clocks; validate patched against unpatched builds |
| 23 | Dependency graph | 7.5/7.6 cycle; `TestRootFactory` placed in 4.5 although it needs device APIs; 3.7 missing 3.4a; no enforced Phase 0 barrier |

Agrees with both rejected Muse claims:
- **plist:** 1.10.0 stores an `IndexMap`. Pin the version, because a minor release may change the backing store.
- **Android:** `AndroidLogConfigurator.kt:17` sets the pattern, and lines 34–36 replace the appenders.

## Remaining problems

1. **Blocking: shared workers.** `/tmp/marathon/shared/` has fixed names (`RemoteFileManager.kt:18,59`), and the locks and transfer caches are local to each process (`SharedTransfers.kt:12`). Concurrent isolation therefore needs disjoint workers.
2. **Major: the SSH harness doesn't see "every command".** It does see executor commands (`SshjCommandExecutor.kt:47`). It doesn't see:
   - the rsync client and the OpenSSH it spawns on the host (`RsyncFileBridge.kt:54,110`);
   - host `Info.plist` and Mach-O reads (`AppleTestBundle.kt:39,70`);
   - the host temp file for the xctestrun (`TestRootFactory.kt:49`).

   ffmpeg, on the other hand, runs remotely (`ScreenRecordingListener.kt:175`).
3. **Major: oracle patches under-specified.** Time sources are scattered: `Modules.kt:48`, `MappingTracker.kt:17`, `TemporalTestResult.kt:7`, `HtmlSummaryReporter.kt:55`. So are random draws (`RandomOrderSortingStrategy.kt:13`, `AppleSimulatorProvider.kt:449`) and UUIDs (`TestBatch.kt:5`). Readiness is launched asynchronously (`DeviceActor.kt:154`). A shared seed is not enough: replay needs draw tapes and quiescence-based event ordering.
4. **Major: explicit null.** `FAIL_ON_NULL_FOR_PRIMITIVES` is off (`ConfigurationFactory.kt:33`). A primitive `Int` with a default, such as `ThreadingConfiguration.kt:8`, binds `null` to 0.
5. **Major: logging.** `MarathonLogging.logger()` sets the level on every call (`MarathonLogging.kt:20,45`). Jansi's default mode strips escapes when output is not a terminal.
6. **Major: dependency graph.**
   - The 7.5 ↔ 7.6 cycle.
   - `TestRootFactory` needs device, tool and bundle APIs (`TestRootFactory.kt:30`), and the installer constructs it (`AppleApplicationInstaller.kt:67`).
   - QueueActor uses analytics (`QueueActor.kt:46,111`).
   - The Phase 0 barrier is not enforced.

Re-review the amended isolation, harness, oracle and dependency graph before approving the baseline.
