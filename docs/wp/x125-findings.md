# x125 — iOS inter-batch work for opt-in features

## Audit and decisions

Read-only search of 82 consumer Marathonfiles, excluding both runner checkouts, found no use of `deviceLog`,
`dataContainerClear`, or permission grants at `BEFORE_EACH_BATCH`. The last value was checked in both its serialized
and source-code spelling. The worker check nevertheless exercised `deviceLog` on two iOS simulators.

1. **Device log.** `DeviceLogListener::after_test_run` stops the recorder, pulls inline, and returns any receive
   exception. `RemoteFileManager::remote_log` really is one fixed worker name per simulator. Starting a subsequent
   `log stream` while an earlier transfer still reads that name could truncate the earlier log. A unique name would
   prevent that, but would not preserve failure semantics. `CompositeTestRunListener` stops running later callbacks
   on an error, including handing the xcresult to the artifact queue. `AppleDeviceTestRunner` can turn that cleanup
   error into the batch error even after committing the test results, and `AppleSimulatorDevice::classify` can mark
   the device lost. `ArtifactProcessor` only reports a failure after admission to the queue, by which point the
   next batch may already be using the device. Rejoining before classifying the batch would keep the wait.
   **D106** instead compresses the log on the existing rsync receive path, so both the fixed worker filename and
   the fallible batch boundary stay unchanged.
2. **Data container clear.** `clear_app_container` sends `terminate`, calls `get_app_container … data`, then runs
   `rm` on the returned path. Each distinct application in the batch gets its own sequence. The real worker's simctl
   exposes one bundle ID per `terminate` and per `get_app_container` call. Terminate is a safe call, lookup is
   critical, and the removal logs errors without throwing them. A shell `&&` would make a failed terminate skip the
   lookup. An unconditional wrapper would collapse the transport errors and independent budgets. No native batch
   form was available, so the sequence stayed unchanged.
3. **Permissions.** `run_batch` awaits each grant before starting xcodebuild. The worker's `simctl privacy grant`
   takes one service, or `all` (which grants unrequested permissions for a subset). Tracking and pasteboard are
   separate critical SQLite calls, not simctl grants. A shell wrapper would replace the per-command time budget
   and error attribution. No native batch form was available, so the grant loop stayed unchanged.

## D106 invariants

The listener explicitly selects the device-log receive. Local or fake file bridges default to their ordinary receive.
The rsync bridge adds `-C` only to the controller's OpenSSH `--rsh` for that receive. The SSH protocol negotiates
compression, so an uncompressed peer can still be used. It does not put `--compress` in rsync's own argv, which
would change the sender command a forced-command filter sees. Sends, videos, xcresults and other file receives
do not opt in. The D54 cancellation behavior, D91/D93 cleanup receipts, D96 worker sender and D97 stage and
atomic publication are shared with the ordinary file receive. Neither log retention nor the report path changes.

The production-listener fake distinguishes a false receipt from an `IllegalState` receive error. Both runs publish
the test result. The first completes its batch. The second returns `DeviceLost` from that same batch with the
original cause and marks the simulator lost. This guards the failure distinction that a detached pull would lose.

## Real-worker measurements

The harness reused the same six one-test UI batches, two warmed simulators, Xcode 27.2, iOS 18.6, public-key auth,
shared worker data connection and rsync receiver on the controller. It used a disposable key, forced command and
dedicated forward. Video, screenshots, and xcresult delivery were off to isolate the log transfer. The controller
logged the pull's own duration, and the interval from an XCTest result callback to the next xcodebuild dispatch
on the same simulator gives the between-batch gap. There are four such gaps and six log pulls per run.

In the initial three pairs (before implementing D106), the median gap was 3.349 s with device logs versus
0.596 s with logs disabled. The median log receive took 2.081 s across 18 files, ranging from 0.572 to 6.139 s.
The high end included files up to 7.99 MiB. No other audited feature was in use in the consumer configs, so no
timing was collected for container cleanup or permission grants.

As a separate controlled check, rsync copied the same 6,679,277-byte log over stock OpenSSH twice with and twice
without `-C`. The four SHA-256 digests agreed. The worker negotiated delayed zlib only with `-C`. OpenSSH counted
6.69 MB received without it and 0.83 MB with it (about 88% fewer bytes). Those copies took 0.90–3.10 s plain and
0.38 s compressed. This controls for noise in live log volume, rather than treating the live logs as identical.

The final check alternated fresh release builds of the exact x118 base (A) and D106 (B), starting with B.
Here are all four between-batch gaps in dispatch order, and all six log receive times in completion order.

| Run | Wall s | Between-batch gaps in ms | Log receives in ms |
| --- | ---: | --- | --- |
| B1 | 32.70 | 1922, 1217, 979, 1369 | 880, 685, 430, 715, 958, 1421 |
| A1 | 39.01 | 4285, 1788, 1123, 2008 | 3344, 1572, 605, 1250, 1161, 1897 |
| B2 | 32.74 | 1766, 1368, 1184, 1469 | 708, 787, 579, 719, 870, 701 |
| A2 | 33.39 | 3198, 1534, 1534, 1118 | 1923, 1247, 914, 448, 576, 1778 |
| B3 | 31.12 | 1433, 991, 1104, 1423 | 524, 441, 529, 738, 549, 391 |
| A3 | 32.02 | 2097, 2761, 1102, 1883 | 1137, 2120, 559, 1176, 872, 762 |

Across the final A/B, median log receive fell from **1.169 s to 0.705 s** (40%), median between-batch gap fell
from **1.836 s to 1.369 s** (25%), and median run wall time went from 33.39 s to 32.70 s. The run-wide measure is
noisier because the two devices can overlap. The files ranged from 1.85 to 2.25 MiB. All 36 appeared under
their unchanged `<batch-id>.<batch-id>` report names. The runs had the same test identities and outcomes in the raw
report, three passed and three failed, with no ignored or incomplete tests and the expected exit 1. Forced-command
traces showed six sends of `device.log` per run and identical rsync sender arguments after removing supervisor
tokens and paths. No new WARN or ERROR categories appeared. The worker's shell-startup, symbol-table fallback and
Xcode stderr diagnostics were the same in both builds.

The two owned simulators, staging directory, worker authorization, local key and dedicated forward were removed.
The shared SSH connection was left running. The harness observed no surviving controller processes and no worker
supervisor directories after these runs. Raw logs are retained outside the repository.

## Contract and limits

The D106 allowance compares the exact controller rsync remote-shell option and corresponding OpenSSH argv for a
staged iOS `device.log` receive. It rejects missing, misplaced or duplicated compression, and does not mask an
option added to another transfer. It still compares the worker command, file bytes, paths and reports. There are
no result-file golden changes and no JVM recordings, copied fixtures or released changelog sections changed.
No Kotlin-oracle test was removed or reclassified.

In a scratch checkout of the exact x118 base, the fresh iOS media differential with the new comparator failed
on the missing device-log compression option. The batch failure witness passed 50 repetitions and one more
under load from two CPU burners. It uses an event-driven fake with virtual time, not a race window. The full
vendor differential passed against the immutable evidence. The parity matrix is current against the source in
`docs/release-source.sha`. The older frozen checkout lacks the coverage-test rows on the base as well.

The real run does not show the cost or worker CPU use of compression at farm scale, on fast LANs, with password
authentication, or under network faults. The first two items can affect the wall-clock benefit. There is still
one rsync SSH login per log. Deferring a fallible log until after the next batch begins would need a decision to
change its error contract, not just a per-batch filename.

**Incidental bugs.** None newly attributed to this WP.
