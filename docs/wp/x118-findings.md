# x118 — Stream iOS screenshots over SSH

## Audit and decision

Confirmed on 0.15.6. The iOS worker captured to a per-device file using `IoService::screenshot`, then called the
ordinary artifact receive bridge. Its rsync `--rsh` launched OpenSSH with `-F /dev/null`; each sender ran on a
distinct worker login and registered its own D96 token. D54 did not pass the capture job's cancellation into that
receive. D91 bounded cleanup when another condition stopped it, not each normal test-end wait.

The initial three runs with the SHA256SUMS-verified published 0.15.6 had a median of 361 ms inside simctl and 1,398 ms
in the controller-measured rsync leg. The two medians come from 37 simctl invocations and 34 successful pulls
respectively. There were 34 screenshot logins and 19.0–34.2 total logins/minute across those runs. Observed in-flight
pulls alone put up to 1,122 ms on a test end. This was a material cost before implementation started.

## D100 design

- Remote capture opens one exec on the existing data connection. The normal D96 `run` envelope supervises the shell.
  The shell gives simctl a unique path per frame, preserves a non-zero simctl status, and unlinks the file after opening
  it for base64. Before that point, normal exit and TERM clean the name. The path is never reused, including when a cleanup
  is still being confirmed as a later test starts.
- The configured screenshot deadline starts before session open and covers exec acknowledgement and the complete
  transfer. Both private streams are byte-bounded before buffering in the capture consumer. Stdout accepts at most
  24 MiB of image data, plus base64 expansion and line breaks. Stderr is drained and discarded after at most 64 KiB.
  There is no line-listener copy, no output in a command error, and no image data in SSH debug. Decode runs off core.
- Dropping the in-flight frame stops the caller promptly. The cancellation still reaches U271's gated exec; after
  acknowledgement an owned close starts without holding the frame waiter. Its D93 command receipt is not discarded.
  If no payload can have started, the frame caller does not wait for the channel's CLOSE acknowledgement either.
  The frame is eligible only with a successful exit, complete output and valid base64. Ordinary sshj shell failures
  still use their usual error classification rather than looking like caller cancellation.
- Local capture still uses the existing file bridge. The GIF assembly, artifact paths, publication and listener order
  stay as before. Videos and xcresults still use their own artifact pipeline.

## Real worker A/B

The same six-test UI suite ran on two warmed simulators over a shared data connection to stock OpenSSH, via a
disposable key, forced command and dedicated forward. The worker ran Xcode 27.2 with iOS 18.6. The same harness
alternated published 0.15.6 and an optimized Phase 1 build. It configured the screenshot recorder with JPEG frames,
`xcresult.pullingPolicy: ALWAYS`, `remoteClean: true`, and `preferredScreenCaptureFormat: SCREENSHOTS`.

| Build | Wall s | All SSH logins/min | Frame logins | Median simctl / rsync ms | Test-end join ms | Frames per test | GIFs / xcresults | Median worker CPU busy |
| --- | ---: | ---: | ---: | ---: | --- | --- | --- | ---: |
| 0.15.6 A1 | 31.30 | 63.26 | 23 | 260 / 550 | 160–579* | 2, 3, 5, 4, 5, 4 | 6 / 6 | 100% |
| Phase 1 B1 | 33.63 | 17.84 | 0 | 254 / — | 0–2 | 3, 9, 6, 6, 10, 6 | 6 / 6 | 100% |
| 0.15.6 A2 | 31.58 | 57.00 | 20 | 274 / 624 | 622–1,033* | 2, 4, 4, 3, 3, 4 | 6 / 6 | 100% |
| Phase 1 B2 | 30.10 | 21.93 | 0 | 256 / — | 0 | 4, 7, 5, 6, 8, 7 | 6 / 6 | 100% |
| 0.15.6 A3 | 32.91 | 52.87 | 19 | 302 / 635 | 550–870* | 2, 4, 3, 3, 3, 4 | 6 / 6 | 100% |
| Phase 1 B3 | 32.78 | 18.30 | 0 | 295 / — | 0–1 | 4, 8, 6, 5, 8, 6 | 6 / 6 | 100% |

Frames are in order AlwaysFailing, Button, DisabledButton, DismissModal, Label, PresentModal. `ffprobe -count_frames`
counted frames and `ffmpeg -v error` decoded every GIF. All runs exited 1 with the expected three passes and
three failures, no ignored or incomplete tests, and the same test identities and outcomes. `xcresulttool` opened all
36 bundles from the A/B runs. No new WARN or ERROR categories appeared. Both builds logged the same worker shell
startup diagnostic, symbol-table fallback, and Xcode's test stderr at debug verbosity.

The forced-command trace counted distinct authenticated connections using the peer endpoint in `SSH_CONNECTION`.
All 127 Phase 1 frame execs used that run's existing data connection. Other commands still used 10–11 logins/run,
including any D96 control connection. The worker logged simctl elapsed/user/system time and the controller logged
the baseline rsync receive. A light one-second aggregate CPU sample was saturated in both groups, so it cannot
attribute a worker CPU saving. Observed simctl CPU alone was about 159 ms/invocation with 0.15.6 and 162 ms with
Phase 1. The candidate attempted more frames, so this is not a CPU regression per frame. The wall time shows no
clear improvement in this short suite.

\*The published build has no exact join probe. Its per-run maximum is bounded below by the longest observed
post-test rsync completion for the same simulator and above by the longest test-end to same-device result-bundle
registration, which includes other callbacks. Phase 1's debug timing is directly around the cancel-and-join.
An unoptimized build had joins in the hundreds of milliseconds during GIF work, so it was not used for the
Phase 2 decision. The optimized build's maximum was 2 ms across 18 test ends. **Phase 2 was not needed and
D101 was not used.**

0.15.6 left a JPEG under each simulator's worker directory until the next preparation. Phase 1 left no frame or
supervisor directory after each healthy run. The two test simulators, worker staging directory, authorization,
local key and forward were removed. The shared SSH connection was left open. Raw evidence is retained outside
the repository.

## Contract and scope

The loopback OpenSSH test exercises the forced command with a real D96 bootstrap and retirement, and checks one
accepted TCP connection for exactly two frame execs. The protocol tests exercise valid and invalid output, a
non-zero exit, mid-frame stop, output bounds, a deliberately withheld exec reply, and a rejected exec whose peer
withholds CLOSE. The five SSH protocol tests and the loopback witness each passed 50 repeats and one more with
two local CPU burners. In a scratch checkout,
disabling the gated reply stop reproduced the 30 s SSH failure. Disabling the mid-frame stop or the reader cap
made the corresponding witness hang until its 60 s guard, as did restoring the legacy CLOSE wait for a rejected exec.

The whole-vendor differential uses the existing Android and Apple fixtures unchanged. D100 compares the exact
fake frame command and bytes,
requires the second capture's cancellation, rejects any screenshot rsync and checks that the legacy frame is
absent. It still compares the decoded GIF, reports, other commands and worker files. No JVM recording, fixture,
result-file golden or released changelog section changed.

**Not measured:** 40 concurrent devices, standalone worker CPU attributable to sshd, very large real frames,
password authentication, or replay on older Xcode. The private reader has an oversize witness. A hard KILL before
the screenshot script unlinks its file can leave that unique name until the next preparation. Network faults
were not injected on the real worker.

**Incidental bugs:** none new. The pre-existing worker shell and symbol-table diagnostics were left unchanged.

## Review round 1

- P1 — The shell-script unit test now uses the async `marathon_process::Command`. The process-spawn checker,
  formatting check and Apple all-targets Clippy passed.
- P2 — Two new `ssh_supervisor` witnesses use `connect_worker`, `SupervisorWorker` and `DeviceCommands`.
  Cancellation is ordered after a partial frame or after an exec whose reply is withheld. In both cases the caller
  returns its own cancellation with no ERROR log. The independent control connection carries a stop for the same
  worker token (TERM mid-frame, KILL when the exec reply is missing). The worker holds its proof behind a gate.
  D93's `wait_handed` stays pending until that gate opens, then the receipt is confirmed. Both witnesses passed
  50 repeats and one extra run with two local CPU burners. The full supervisor binary passed 52 tests, and the
  existing SSH screenshot binary passed 5.
- P3 — The U272 ledger row now has its `fixed` status cell.

Free disk space fell below the work package's 20 GB threshold after Clippy. The broader Cargo test build and
scratch counterexample build were not run. This round changes tests and documentation only.

## Integrated gate follow-up

The catalogue test failed on x118 alone as well as on the integrated stack. It booted its simulator in the
video scenario, which does not serve the catalogue's file-writing screenshot command. The screenshot row now
uses a booted simulator in the screenshot scenario on both transports, invokes the production file-writing
wrapper and checks the written fixture bytes. The D100 streaming model and the recorded catalogue stay unchanged.
