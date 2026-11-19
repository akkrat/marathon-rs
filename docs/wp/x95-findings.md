# WP x95 findings — process ownership

## Scope

D96's controller step and U227 cover native local launches, the exit sweeps and the
run-scoped process reaper. The password transport, recorder ownership and worker
supervisor are follow-up work within x95. The controller step changes no result
goldens, Kotlin recordings or configuration fields.

## Evidence at the base

| Path | Verified failure or boundary |
|---|---|
| Exit sweep | The first signal can reach the D52 deadline while provider termination waits in the iOS artifact join. The hard exit and second signal both reach `exit_once`, which exits without sweeping processes. R02a and R02c left the stopped controller rsync and SSH processes. They also left, respectively, a worker recorder and xcodebuild. The artifact join and signal cutoffs belong to x96. |
| Rsync and password authentication | The bridge starts a Tokio child with `kill_on_drop`, escalates with `start_kill`, and looks for descendants through periodic process listings. A child can be orphaned before the first listing. In R02b an SSH process survived while the device was correctly quarantined. The password wrapper creates a new session for its child, so an outer private group alone would still miss the inner rsync and SSH. |
| ADB | The provider probes the configured endpoint and calls `adb start-server` on a loopback connection failure. The endpoint supplies host and port only. A successful client exit and a subsequent successful version probe establish reachability. They give no server process identity or disposal policy, and a different client can start the server in between. |
| SSH | D93 ends the handed-command receipt on channel close or transport end. Those events can occur while the worker still owns a live process. The rsync SSH transport is separate from the command connection. Cleanup needs its own bounded connection and worker-local expiry. |
| Recorders | The recorder starts with a detached `nohup` command and a shared pidfile. The unhealthy-device branch returns before stopping it, and the remaining stop path reads the pidfile using the cancelled batch context. L06e left a recorder after simulator shutdown and runner exit. |

## Launcher experiments

The discarded macOS prototype used a shell as group leader until a pipe acknowledged registration. It observed leader exit with `waitid(WNOWAIT)`, so the leader still pinned the group during the kill. The direct group witness waited for the leader to exit, stopped a real grandchild in that group, and checked a separate process survived. It passed 50 times and once under CPU load.

The executor-level probe started a shell that exited while leaving a real grandchild with its own output redirected. Against a scratch copy of the base, the session returned while the grandchild was still addressable. A kill in the assertion also served as teardown. The prototype could still return before group disappearance. A KILL request and direct-child reap alone are insufficient for the D93 receipt.

A separate native probe reproduced macOS returning `EPERM` from group KILL when the sole group member was an unreaped, exited leader. Ignoring that error globally would also mask a real permission failure. The prototype bench used that shortcut to measure overhead. The existing `execute_start_errors_propagate_and_signed_timeouts_remain_signed` test passed at the base and failed against the prototype because the shell reported a missing executable as a payload exit.

| macOS microbenchmark | Direct, 1,000 commands | Candidate, 1,000 commands |
|---|---:|---:|
| Tokio, sequential short commands | 0.827 s | 3.936 s with the shell gate |
| Native spawn probe, sequential short commands | 0.692 s | 0.727 s with a private group and suspended start |

The native suspended-start probe preserved the launch error for a missing executable. In a separate probe, the launcher exited before resuming the child. The child had disappeared by the time it was checked and no payload output was observed. This single probe gives no cross-platform guarantee about the registration race, PTY setup, or waiting for the group. A production launcher needs to retain the exact group identity until cleanup and serialize retirement with the sweep, including launch failure. Shutdown also needs to reject work in the launch-to-registration gap without depending on another async poll.

## Worker and recorder design

The foreground supervisor needs to establish its token and group before payload execution. Each token's worker-local deadline must expire independently, with confirmation tied to the group's exit. On a lost exec acknowledgement, use the already-known token over the independent control path. On a lost receipt, keep the affected device quarantined and allow siblings to continue. Measure the worst partition-to-kill interval from the chosen keep-alive-derived lease. A frozen worker cannot execute its expiry code until it runs again.

The default system shell in the macOS probe did create a separate group with noninteractive job control. The stock dash alternative turned job control off without a terminal. Depending on the default shell's behavior would leave that worker configuration uncontained. The stock Perl interpreter and standard modules were present on the test machine. The migration prerequisites and Kotlin marathon at `e47352f2` do not list Perl as a worker requirement. The owner needs to choose whether stock Perl is an acceptable prerequisite or specify another permitted supervisor runtime.

For ADB, every configured server needs to stay available under the current endpoint policy. If a private-server stop case is required, the owner needs to identify which existing setup may opt into it. The implementation must then own a foreground server handle from launch, including when another server already owns the port.

The recorder handle needs to be created before launch, with a protected, shared stop that disposal and batch cancellation both join. INT starts finalization. TERM and KILL use the time remaining after that attempt. The recorder stop result should tell x96 whether finalization succeeded. The producer must not make the publication decision.

## Verification limits

The base CLI iOS integration passed through real SSH to a fake worker in 2.65 s of test time. It establishes a healthy baseline only. Linux, both signal races, PTY authentication, ADB policy witnesses, SSH partition and lost acknowledgement, and real-device reruns were not verified. The prototype never reached healthy local and SSH run comparisons. No comparator allowance, parity matrix change, or golden diff was produced.

## Native launcher implementation

The subsequent owner decision approved stock Perl with a once-per-worker fallback warning, and retained the
existing ADB server policy. The launcher tracks ADB clients by PID so a cleanup racing a daemon's startup
cannot send KILL to the new server before it establishes its own session. There is no private-server policy.

The native launcher creates a private group through `CommandExt::process_group(0)` and keeps native launch
errors. Admission increments the in-flight count before spawning, then publishes the PID before any pipe conversion.
The sweep seals admission, signals registered targets and shares one 200 ms wait for in-flight launches. Late
registrations send KILL immediately when sealed. A child may execute between the kernel spawn and registration.
If a kernel spawn does not return within 200 ms, synchronous exit can beat its registration. This is the
residual window of the approved native launch approach.

The initial implementation kept the leader unreaped while its group was signalable. Retirement and the
emergency sweep shared the registry lock. It then reaped the leader and checked for group disappearance
using signal 0. D93's local command receipt also waited for both readers. Round 1 moved the group check before
retirement so an unresolved group can stay registered without releasing its identity.
At the D52 deadline, core notifies the existing watchdog thread before flushing output. The watchdog sweeps
synchronously without waiting on the console. The second signal sweeps synchronously in the immediate exit path.
The signal cutoffs and publication policy belong to x96.

The native launcher measured **1.370 s per 1,000 sequential launches** versus **1.367 s** directly on macOS.
The CLI iOS integration using real SSH and a fake worker passed in 1.69 s and 2.69 s in separate runs, compared
with 2.65 s at the base. These timings include test fixture setup and give a smoke check rather than a measured
production throughput change.

The registry late-registration witness, exited-leader witness and ADB client policy witness each passed 50 times
and once under CPU load. The exited-leader witness failed with a group disappearance timeout in a scratch copy
where retirement omitted KILL. All 16 initial CLI signal window tests passed. The original stopped-child exit checks were
superseded in round 1 because kernel orphan handling also killed those children.
The CLI vendor differential comparison passed without changing recordings.
Linux execution remains for the remote gate.

The local launcher step preserves the legacy password wrapper's extra session and the detached recorder's
pidfile handoff until their focused migrations. Both paths still have containment gaps. Worker commands
and D93 remote command receipts still use the previous protocol.

## Password terminal investigation

The isolated PTY test passed. It answered a split password prompt separately from the payload's input and output,
and returned the payload's own nonzero status. The actual rsync roundtrip with OpenSSH and a fake worker did not
complete. The controller detected the prompt and wrote the credential and line terminator, but the worker
observed no authentication response. A native PTY probe reached the same pending state. Changing terminal
mode and opening the slave from within the child session did not produce a reliable roundtrip. The PTY draft,
the comparator prototype and its test migrations were removed. Step 2 remains open.

The focused local executor, legacy recorder, and rsync suites passed on the completed launcher step, with 22,
1 and 84 tests respectively. The rsync tests now assert that inherited pipe holders are gone when the
transfer returns, and require the full TERM grace only while the leader is alive. A first broad run still
used assertions for the former behavior, and its differential run detected files changing during the PTY
investigation. The final package-scoped run passed 2,508 tests with 35 ignored and zero failures. That run included the CLI
vendor differential comparison, all 16 signal tests, the real-SSH iOS smoke test with a fake worker at 1.85 s,
and the actual local command and file differential probe at 0.19 s. Formatting, targeted Clippy, the source
gate, the plan graph and the Python evidence validators passed. Full workspace and remote gates are reserved
for the orchestrator.

Recorder handles and the worker supervisor remain pending. The owner already authorized the stock Perl
probe, fallback and warning. The worker protocol, migration prerequisite and real-device reruns still need
implementation and verification.

## Round 1 review

The CLI starts one reaper before installing its run signal handlers or creating threads. It restores the default
SIGCHLD disposition so the kernel cannot auto-reap a leader while its registration is live. The reaper has
its own process group, closes every inherited descriptor except its pipe reader, and ignores INT, TERM,
HUP and PIPE. The controller sends fixed-size add and remove records through a nonblocking write end with
close-on-exec. The helper drains all records before it acts on EOF. Removal is serialized with the synchronous
sweep and published before the controller reaps the leader. Publishing a removal frees its slot for reuse.
A full pipe or table rejects a new launch after sending KILL and retaining the target for in-process cleanup.
A failed removal keeps the leader pinned for deferred cleanup. Reaper setup or pipe loss logs a warning and
leaves the synchronous sweeps active. The parent verifies the helper's private group even if the
child gets there first. Startup warnings go to stderr so configuration dumps still emit only JSON on stdout.

The ordinary exit sweep, the D52 expiry sweep and the immediate second-signal sweep have independent
witnesses using live sleeping children and no reaper. The expiry witness observes the tool's SIGKILL and
reap while the controller is still waiting on a blocked console. A separate test kills the built CLI's
group with SIGKILL while it owns a running local tool. The tool fills its stdout pipe before announcing
readiness, which establishes that launch registration completed. A group-directed SIGHUP finishes the
provider and exits 129. The CLI report test also exercises SIGHUP in both debug modes.

The local wait observes the leader's exit status with WNOWAIT, sends KILL and checks membership before
reaping. macOS uses the native process-group query, and Linux checks procfs on the blocking pool.
If group confirmation expires or fails, a warning accompanies the actual exit status. The unconfirmed
receipt remains distinct from the status, with deferred cleanup retaining the leader and registry entry.
The command handle retains ownership of its output pipes after reaping so readers can still drain them.
The injected timeout checks both zero and nonzero status, and the pipe seam covers queued removal,
slot reuse and saturation. The ADB client uses null standard streams, with a fake binary checking all
three descriptors. ADB server ownership stays independent.

The transfer from native spawn to registration still has a coverage window. A successful removal
excludes that entry from the reaper's kill loop even if the kernel later reuses its number. On abrupt
controller death the kernel can reclaim an unremoved group's ID if the group has already emptied
and its orphaned leader is reaped before the helper runs. The portable KILL call takes a numeric PGID.
Processes that leave their group and the legacy recorder handoff remain outside this controller step.

The five real-process exit and signal witnesses, the removal and saturation witnesses and the
unconfirmed-status witness each passed 50 isolated runs and one under CPU load on macOS. Scratch
mutations dropped reaper startup, each sweep, HUP registration, remove publication, exit-status
preservation, ADB standard-stream redirection and the added spawn-gate checks. Each corresponding
witness failed at its expected assertion. Linux process-crate Clippy cross-checks passed. Linux
execution and real-device acceptance were unavailable locally.

The wider macOS package pass exposed a dropped-pipe regression and a startup warning that broke
configuration JSON. After fixing both, the pipe-drain test, config corpus and replay of all 4,299
configuration cases passed. A 60-run configuration smoke check returned valid JSON every time, with
zero reaper startup warnings. The vendor differential comparison passed. The evidence validator passed
110 tests after rebuilding its cached YAML helper. The local tool smoke test ran in 0.66 s and the
built CLI's iOS run and parse through real SSH to a fake worker ran in 1.74 s.

The adjacent 1,000-launch comparison measured 1.506 s direct and 1.711 s registered on the
pre-review launcher, then 1.831 s direct and 1.874 s registered with this change. These measurements
were taken during the package checks. They exercise native launch and group confirmation with the
library reaper disabled, and the variation in the direct runs limits the comparison.

The askpass transport limits OpenSSH to one password prompt and scans each
line of the version response. The real password rsync round trip passed in
1.75 s. Its argv witness also verifies that the key-authenticated path carries
no password prompt limit.

## Incidental bugs

No additional bug outside x95 was pursued.
