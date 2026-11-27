# x122 · Test harness sweep and ADB idle diagnostics

| Item | Claim checked against | Change and invariant |
| --- | --- | --- |
| U202 | The entry-cancellation replay could see an idle instant between Tokio's final vendor poll and destruction of its tracked future. Logical completion does not join that destruction. | In test-support builds, arm a capture at each drained boundary and join the physical prepare and execute tasks before recording the row and asserting quiescence. No production runtime or trace change. |
| U203 | The snapshot race was driven by a connected-check rendezvous, but the final negative assertion used a wall-clock sleep. Joining the scan alone does not join publication: `publish` dispatches separate tasks. | Hold the stale check until replacement registration returns, consume the replacement connect explicitly, then terminate to join background publishers before checking for stray events. Only deadlines that catch a hung test remain. |
| U177 | The existing gated bridge test discussed `connections/deadlines.jsonl` but never loaded it. | Load and check the three ordered JVM rows in the existing test. The fake transfers are held across the old 30 s checkpoint. D43 changes the receive's `false` there to a still-pending result; the send stays pending and succeeds after release as recorded, and the receive now succeeds after release as well. This retains the pre-existing compressed deadline test, not a new production timer. |
| U173 | An ordinary shell can reproduce the password fallback failure. | No wrapper or product change. See the evidence below. |
| U281 | Background shell jobs can pass ignored SIGINT/SIGQUIT through exec, so the CLI test cannot rely on either default disposition. | Reset the two signals in the test child before exec. The test runner and CLI production code are unchanged. |
| U282 | Killing the Perl owner does not kill or reap the anchor, which has its own process group and keeps stdout open. | Foreground witnesses own a lifetime pipe. Drop closes it and resumes the owner if the stop hook paused it. The test copy wakes from select and reaches the shipped kill/waitpid cleanup, so only the owner ever reaps its anchor. The test launcher reaps the owner and isolates its stdio. At the hang guard, leave cleanup with the owner instead of killing it first. Detached recorders already redirect stdio and are not changed. |
| U162 | Android's final setup property read uses the idle-limited client and the shell v1 parser observes idle closure as EOF. | Record the first socket close cause without changing the EOF API. Only a shell-v1 rejection on a property read after an idle close gets new wording. The error kind, error propagation, plain EOF, and successful parsing of completed output stay the same. |

## Password fallback evidence

Plain shell loops on macOS used sshpass 1.06, the system OpenSSH 10.5 client and openrsync.
The server was a separately launched loopback SSH daemon with a fresh host key and randomly
generated fixture password. It ran the real rsync sender. No child-test command, wrapper,
retry, askpass helper or system account change was involved. The standalone fixture daemon
substituted for stock sshd so a disposable password did not require changing a local account.
The daemon was stopped and its short-lived key deleted at the end.

The first loop copied and verified 66 of 100 payloads. The other 34 were denied
authentication. With verbose SSH output, a second loop copied 82 of 100. All 18 failures
reported `read_passphrase: can't open /dev/tty: Device not configured` before authentication
was denied. A third 100-pull loop with a controlling terminal copied 76 payloads and had
24 matching failures. This rules out the test wrapper and an initially absent controlling
terminal as necessary triggers but does not identify the
underlying terminal race. The host's OpenSSH supports forced askpass, so the experiment forced
the legacy sshpass fallback rather than exercising current default selection. It did not
test a legacy client build or a production worker. Any runtime retry would change observable
outcomes and needs an owner's decision and a deviation.

## Verification

- U202 did not reproduce on the scratch base in 50 executions under CPU load
  (100,000 replay traces). With the fence, 50 executions plus one with four CPU burners
  passed, totaling 102,000 replay traces. The fence relies on the same Tokio
  future-destruction-before-join ordering already documented in the replay driver.
- U162, U177, U203, U281 and U282 each passed 50 normal executions and another with four
  CPU burners.

- U162 failed against the unmodified Android request in a scratch worktree (old delimiter
  message), and the fixed test also checks real EOF and a complete reply followed by idleness.
- U177 failed in scratch with the receive stall check moved back to 30 s. The first live
  checkpoint was then the recorded JVM failure instead of D43’s pending receive.
- U203 failed in scratch with the stale-handle check removed, leaving a disconnect queued.
- U281's pre-fix background integration run timed out waiting for SIGQUIT's dump. All seven
  shutdown-report tests subsequently passed in a background shell. The focused witness
  verifies both dispositions even when the enclosing shell did not ignore them.
- U282's baseline owner was killed and reaped with its payload gated; its stdout pipe stayed
  open until the experiment killed its own group. The fixed regression drops a stopped owner
  and checks the terminal receipt, which is written only after reap and group disappearance.

- Formatting, changed-crate all-target Clippy, the process-ownership check and its tests,
  the differential registry check, the 117 comparator tests, and offline differential
  validation all passed. Full workspace and remote gates are reserved for the orchestrator.

## Contract, goldens and incidental bugs

No deviation or golden update. Only U162 changes user-visible text, not the decision to
succeed, fail or retry. The JVM recording and published worker script were not edited.

No other product issues found. U283 remains unused. No real Android device, simulator, farm
worker or remote gate was tested for this harness-only and diagnostic work.

## Review round 1

One review by another model (REQUEST_CHANGES, one P2): U162's complete-reply case used the 1 s idle limit, which runs on
the transport's clock from the socket's creation and is checked before buffered data is read, so a test stalled for a
second on a loaded runner would see a timeout instead of the reply. That case now uses a 5 s limit; the two failing cases
keep 1 s, since they end at the limit whichever way they are scheduled.
