# WP x95 findings — process ownership

## Status

Stopped before migration on `9ce312ff`. The launcher prototype changed spawn errors, added measurable overhead, and could confirm cleanup while a descendant was still addressable. The prototype was removed. D96 and U227–U234 remain available for the implementation. No result golden, Kotlin recording, configuration, command, or release documentation was updated.

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

## Incidental bugs

No additional bug outside x95 was pursued.
