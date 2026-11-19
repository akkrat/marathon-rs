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

## x95b password transport

The controller probes the SSH client selected by rsync once, on first password transfer.
OpenSSH 8.4 or newer can force askpass without `DISPLAY`. The CLI uses its own binary as
the helper, with the secret in the child environment and no public command or flag.
Embedded callers use a mode-0700 helper inside a private temporary directory. The
helper accepts password prompts and rejects confirmation prompts. Rsync remains the
owned leader and retains its exit status. Older SSH clients use the existing `sshpass`
cleanup path and warn once per run. The `-e` option keeps the fallback secret out of argv too.
[OpenSSH 8.4 release notes](https://www.openssh.org/txt/release-8.4).

The real password roundtrip to the fake SSH worker passed with rsync and OpenSSH,
including send and receive, with display variables unset and a deliberately missing
`sshpass`. The built CLI also completed a password-authenticated iOS run against
the fake worker in 3.59 s for the smoke suite. The smoke uses the fixture's SSH
shim to isolate known hosts from previous loopback runs. The legacy transport
witness confirms one SSH version probe and one warning across queued transfers.
The TERM and KILL witness checked both actual process IDs and the
transfer's confirmed cleanup receipt. It passed 50 times and once under CPU load.
In a scratch checkout at the x95a head, both witnesses failed because the legacy
transport needed `sshpass`. The KILL witness also exposed a macOS race where
`waitid(WNOWAIT)` could observe exit before a nonblocking reap succeeded. The
launcher now checks the terminal event code and retries a delayed reap within a
bound. The follow-up launcher measurement for 1,000 commands was **0.951 s direct**
and **0.861 s owned** in one macOS sample.

The differential comparator permits removal of the unconditional rsync4j warning
only on live native SSH captures. The JVM and pinned native captures keep their
existing warning check. No result golden or oracle recording changed.

## x95b recorder ownership

The listener opens a foreground `xcrun simctl io … recordVideo` session. The
simulator registers the handle before launch and includes unconfirmed handles
in its D93 shutdown receipt. The batch, unhealthy-device callback and disposer
shorten or join one detached stop. A cancelled waiter cannot abandon the stop.
The recorder has up to 15 s after the first stop request, limited by the device
window. INT receives the first two thirds. TERM and KILL use the remainder.

`Recording::finalization` and `ScreenRecordingListener::stop` expose the recorder's
outcome to the caller. A zero exit status before escalation reports `Finalized`.
An observed failure or escalated exit reports `Unfinalized`. A refused start
reports `NotStarted`, and an unavailable exit report leaves the device
`Unconfirmed`. D93 skips lifecycle and quarantines that lease at disposal. A
later recording does not start while an earlier one's stop remains unconfirmed.
The existing media transfer and attachment calls still own their current policy.

The local test runs a real signal-handling foreground child. The iOS tests also
cover the shared stop across batch cancellation, an unhealthy callback and
disposal, and the missing-exit quarantine after KILL. D80 still waits for a previous
stop before starting the next recorder, and waits for a previous transfer if the
next recorder reuses its destination.

The recorder listeners, the real-process witness and the D80 video suite passed
50 repetitions and one run under CPU load. Both new listener witnesses failed
in a scratch checkout that sent TERM in place of the initial INT.

The D96 iOS media comparison validates the recorded pidfile command sequence
and the new recorder's INT and successful exit before comparing the movie's
command and the other evidence. It preserves unrelated commands and all result
goldens. The deprecated pidfile wait tests are marked `n/a` in the parity matrix.

## x95b worker exec leases (4a)

An Apple SSH data connection probes stock Perl and installs the worker
supervisor in a private directory on first exec. Concurrent calls share the
result. If the worker lacks the interpreter or a required core module, the
connector warns once per worker and continues with the existing commands. A
transport error or invalid successful bootstrap response fails the command.

Each handed exec has a unique token. The worker writes the token's state
before letting the payload past the group-ready handshake. An unreaped anchor
pins the group ID until KILL. The worker writes a receipt after reaping the
anchor and seeing the group disappear. A stop that reaches the worker before
the exec publishes a terminal token which prevents late launch. The controller
uses worker proof for a supervised command's exit status and D93 receipt.
The device-log process now runs in a foreground session on supervised workers,
since the background log would otherwise be killed when its launcher exits.

This step renews and stops tokens over spare channels on the data connection.
If the connection stalls or cannot open another channel, lack of renewal
causes the worker to kill the group. The default lease is 79 s. At most one
already-issued renewal can extend the last expiry the controller observed.
Confirmation can still be lost in a partition. This slice retains the worker's
private token directory after the connection ends.

A loopback OpenSSH session with real Perl returns status 23 after the payload
runs past its initial lease. After loss of the connection, a second payload is
observed gone. A fake worker covers one probe per connection and one warning for
two connections. The Perl witness for stop before launch runs on Linux and macOS.
Each witness passed 50 macOS repetitions and once under CPU load. All eight prototype protocol witnesses also passed against the production
script. Scratch copies with expiry enforcement or the pre-launch stop check
removed failed their respective witnesses. The comparator allows only the
exact probe and completed fallback response on live native captures, with
negative cases for altered programs and results. No result golden changed.

## x95b independent worker control (4b)

A successful Perl probe opens a second, authenticated SSH connection before
handing off any payload. One control connection belongs to each data connection.
The control connection uses the same credentials and host-key verification,
with 4 s for the dial and 4 s for each exchange. Commands on the same worker can stop and
renew independently by token. A failure of an entire control connection
interrupts renewal of every token using it. Devices on other connections can
continue. A control connection that is known dead refuses new work and makes that
data connection ineligible for reuse.

The worker lease starts before exec and is not renewed while its exec reply
is missing. A device seal requests token KILL if the data channel has yet to acknowledge
launch. A recorder caught there can remain unfinalized. For a session that opened, D30 sends TERM and
then KILL through control, and waits for worker proof within the existing
window. A stuck data channel can be discarded after proof. When the worker
cannot return proof, D93 retains its unconfirmed receipt and quarantine rules.

A loopback OpenSSH worker limited to one session per connection completes a
command that outlives its initial lease, then confirms cleanup within the
bound when the data connection is severed. Fake-worker tests cover the lost
launch reply with a successful sibling command, and D30 escalation on virtual
time when the data channel withholds CLOSE. Both fake-worker tests run on Linux
and macOS. Scratch reverts of stopped launch and receipt-based D30 close failed
the corresponding tests. The five witnesses passed 50 repetitions and under
CPU load on macOS. The Apple, iOS and macOS suites passed 1,138 tests with
26 ignored. The CLI vendor differential, real-SSH iOS smoke and shutdown
report tests passed. No result golden changed.

## x95b rsync senders and recovery (4c)

Artifact pulls give the bridge the device's executor. The remote sender runs
under a token with the same worker account as the executor, and the bridge
joins worker proof before ending the D91/D93 receipt. Local rsync still
determines the transfer status. On macOS, openrsync removes shell quotes when
building its SSH command. The sender envelope therefore carries the remote
program as base64 and the original server arguments as individual arguments.
A real OpenSSH loopback pull copied the expected bytes, and cancelling a pull
killed a real remote sender that ignored TERM and HUP.

A lost control connection wakes the lease watchers. They share one bounded
dial with 1 s spacing after failures. They can renew through the data connection
when it can execute a request on the worker. The fallback needs a free SSH
session. With both routes unavailable, the worker handles expiry and the
device keeps the unconfirmed receipt. The existing keep-alive settings still
set the lease, with a floor of the renewal interval plus 26 s. The migration
guide gives the expiry bound, including one in-flight renewal.

The virtual-time worker tests cut the control socket and verify a fresh
connection renews leases before expiry. Another blocks reconnection and
verifies that the command connection renews two healthy groups. The partition
case cuts both connections, observes worker expiry and an unconfirmed device
receipt, and verifies that a sibling on another connection keeps running.
All four new concurrency witnesses passed 50 repetitions and under CPU load.
Scratch reverts of reconnect, data-path renewal, sender ownership and the
worker expiry check failed their witnesses.

The comparator requires the exact bootstrap, installed root, token, lease,
expected sender framing and matching terminal receipt for each supervised
command. Host rsync and SSH invocations must use the observed sender token.
It compares the original payload and its result. Successful wrappers have
positive and negative projection tests. The live vendor differential fixtures
exercise the Perl fallback. Pinned recordings and result goldens stayed intact.

The package suites passed 1,268 tests with 33 ignored and one known
`simulator_world` watchdog timeout. All seven tests in that target passed
serially. The scratch build reused the Cargo target and invalidated artifacts
needed by rustdoc. The isolated doctest rerun rebuilt them and passed.
Formatting, scoped Clippy, 115 evidence tests, registry, source and plan checks
passed.

Linux and real-device gates remain for the orchestrator. Before launch
acknowledgement, token KILL can leave a recorder unfinalized and D97 reports
its movie as undelivered.

## x95b round 1 SSH latency and probe corrections

The reference loopback used one-slot OpenSSH data connections on macOS,
with three warmups and 30 commands per case after bootstrap. Times are
medians in milliseconds from execute through result.

| Command | Plain before | Supervised before | Plain after | Supervised after |
| --- | ---: | ---: | ---: | ---: |
| \`/usr/bin/true\` | 13.60 | 104.31 | 13.52 | 21.24 |
| \`/bin/sleep 0.3\` | 339.23 | 411.62 | 339.77 | 333.33 |

Normal foreground exits use the SSH data channel and avoid both result polling
and the control request. The Perl wrapper can emit a normal status only after
writing its cleanup receipt. It is exec'ed directly so an unconfirmed wrapper
stop reaches SSH as a signal. D30 and D93 still join proof when necessary.
Short commands open no control transport. Renewal starts at the normal interval.
Control starts lazily and uses the small Perl command path, which avoids loading
the launch-only modules for each exchange. The connection retries with capped
exponential backoff and renews over a reachable data connection at the normal cadence.
A signalled payload keeps the wrapper's numeric status, 128 plus the signal number.

The single-line bootstrap ran through \`/bin/csh\` on stock Perl 5.34.
A fake worker verified fallback and one warning for filter, import and temporary
directory failures. It also rejected and returned malformed success for first
probes, and accepted the next probe on the same connection. Scratch controls
and transport witnesses cover retained D30 and D93 behavior.

The askpass transport limits OpenSSH to one password prompt and scans each
line of the version response. The real password rsync round trip passed in
1.75 s. Its argv witness also verifies that the key-authenticated path carries
no password prompt limit.

## x95b round 1 recorder ownership

A supervised iOS recorder acknowledges launch and closes its data session
while the worker still owns the recording group. Its exit receipt, read over
token control, supplies the finalization result. The worker renews a recorder's
lease during its healthy INT window. Workers without a usable supervisor run
the legacy nohup and pidfile commands. Local recording remains foreground.

The loopback fake worker capped the shared connection at 10 sessions. Five batch
and recorder pairs opened in 26.6 ms. Five health checks and five INT finalizations
finished by 52.2 ms. With foreground recording restored, the first health check
failed with the session-cap refusal. The stock Perl test observed a live payload after the detaching
launcher returned at 19.5 ms and confirmed the INT exit by 54.4 ms. A virtual-time
listener test reported finalization after a 24 s INT handler with a 30 s shell
timeout. Restoring the old 10 s INT window made that test report an unfinalized
movie. A second stop reported no new recording, and events after sealing
returned normally.

On a disposable iOS 18.6 simulator, real `simctl recordVideo` exited 0 after
INT. The 66,184-byte output contained complete `mdat` and `moov` boxes.
The simulator was shut down and deleted. The live vendor comparison passed with
the legacy pidfile wire, and the supervised comparator rejects an acknowledgement
without the recorder's own INT and successful terminal receipt.

## x95b round 1 worker state retirement

A terminal token sheds its mutable state and lock, retaining one receipt for
late proof requests and replay protection. A stock Perl witness let a detached
lease expire, matched its KILL receipt, and verified that a sibling succeeded.
It also observed that retirement waits while the group is owned and removes
the whole root after the last operation exits. A separate SSH witness verified
that healthy close reuses the data transport for retirement. A partition can
leave the installed script and compact receipts under `/tmp`.

The final one-slot OpenSSH run used the same 30-sample method. Median
`/usr/bin/true` took 13.31 ms plain and 19.39 ms supervised. Median
`/bin/sleep 0.3` took 336.82 ms plain and 330.97 ms supervised. Closing took
0.13 ms plain and 18.11 ms with the one-time retirement exchange.

The recorder capacity, stock Perl detachment, healthy stop window and
retirement witnesses each passed 50 repetitions and one run under CPU load.
Isolated negative controls reproduced the capacity refusal, early escalation,
missing retirement request and leftover mutable state.

## x95b round 2 integration and final review

The rebase starts at `1486a5b2`. The merged controller reaper and local
cleanup rules replace the older launcher implementation. Askpass dispatch
precedes helper startup. Device and provider command closes use the D97
deadline for supervised channels too. At 17 s, a token KILL can preempt a
stalled TERM exchange. The close still needs the worker receipt before
the 18 s receipt cutoff. A partition still relies on lease expiry.

A video is eligible for the D97 transfer path after finalization. A requested
movie with any other outcome is reported once as undelivered. Staged rsync
receives also require the worker sender's receipt before publication.
The regression witnesses cover refused delivery, publication suppression,
token KILL during a stalled control request, and the distinction between
a missing launch and missing cleanup proof.

With no worker supervisor, a legacy launch or stop failure affects only that
movie. The recorder still gets a best-effort stop and is outside the D93
receipt decision. The next test can start again. The witness refuses a
launch and a stop in separate runs, then delivers the following movie and
releases the simulator. Concurrent worker commands share one failed
capability probe and the next command tries again.

Local unconfirmed group cleanup drains the available stdout and stderr
within a byte bound. The real CLI ordinary-exit witness uses the existing
test-seams feature to leave a running group registered and skip the reaper.
The retirement witness checks the leader remains waitable at removal, and
the startup unit test checks READY, both failure bytes and EOF. The spawn
source check rejects native glob imports followed by spawn calls and
ignores comments and literals. Each new runtime witness passed 50 isolated
runs and one run under CPU load. Scratch worktrees reproduced the expected
failures with the corresponding changes removed.

The rebased loopback used a one-slot data connection, three warmups and
30 measured commands per case. Times are medians in milliseconds.

| Command | Before round 1 plain | Before round 1 supervised | Rebased plain | Rebased supervised |
| --- | ---: | ---: | ---: | ---: |
| `/usr/bin/true` | 13.60 | 104.31 | 12.88 | 17.19 |
| `/bin/sleep 0.3` | 339.23 | 411.62 | 337.95 | 329.33 |

Ordinary close took 0.09 ms plain and 16.57 ms with worker retirement.

### Linux D30 witness

The Linux trace showed TERM on the independent control connection. The fake
worker had recorded channel end while the reply was still in flight. Advancing
Tokio by 21 s expired the four-second control exchange and put the reconnect
into backoff. KILL then used the data connection, which failed the witness.
The test now releases only the D30 grace delay and keeps both independent
transport assertions. The corrected witness passed 50 runs and one loaded
run on each platform.

## Incidental bugs

No additional bug outside x95 was pursued.
