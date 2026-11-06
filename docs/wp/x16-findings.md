# X16: SSH connection faults fixed (D35–D37)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field, CLI option or exit code changed.

## D35 / A16: a failed key exchange

**Ledger row checked:** correct. `connection_failures.rs` listed `DisconnectReason::Unknown` as transient, and
`client.rs::transport_failure` reports `RusshError::NoCommonAlgo` with that reason.

**How it is told apart.** `transport_failure` is the one place where a russh error becomes an `SshFailure`, and both
routes to it (`connect_stream`'s error and the handler's `disconnected`) carry the `NoCommonAlgo` variant.
`SshFailure::no_common_algorithm` sets a private marker there, and the classifier refuses a marked link. The marker is
structural: a message match would let a server's disconnect text decide, and demoting `UNKNOWN` would stop retries of
real network faults, whose `Broken transport; encountered EOF` also has that reason.

**What keeps the JVM text.** Class, `UNKNOWN` reason and message, so `java_to_string` (the `Dying because` and `woke to`
log lines, which would otherwise gain `[KEY_EXCHANGE_FAILED]`), `to_failure` (the report and the logs) and `wire_reason`
(the disconnect code the reaper sends) are all unchanged. The marker does not cross into `Failure`, and no caller needs
it to: the classifier takes `SshFailure` before that boundary.

**After the change:**

| | Before | After |
|---|---|---|
| Connection attempts by the connector | 4, immediately | 4, immediately: `withRetrySync` retries every failure, an unverifiable host key too |
| Error the user sees | `TransportException: Unable to reach a settlement of KeyExchangeAlgorithms: […] and […]` | The same |
| `is_transient_connection_failure` | `true` | `false` |
| `SimulatorRecovery::connect_failed` | Retries the worker later | `Failed to initialize connection to worker …`, no retry |
| `SimulatorRecovery::retry` | Keeps retrying | `Failed to connect to worker …, attempt n, not retrying` |
| `SimulatorProvisioner::setup` | `Outcome::Unreachable`, retried | `Outcome::Failed`, `FailedSetup { unreachable: false }` |

Those three are the classifier's only callers in production code. macOS never calls it: `AppleMacosProvider` logs a
connection failure and does not retry, before and after.

**Left as they were.** russh's `Kex`, `KexInit` and `UnknownAlgo` also become `UNKNOWN`. They come from malformed
exchange data, not from a failed negotiation, and a corrupted packet is what a retry can heal, so they stay transient.
A server that sends its own DISCONNECT with reason 3 was already non-transient.

**Verified:** the fake's `no_common_kex`, a throwaway real OpenSSH 10.5p1 with `KexAlgorithms
diffie-hellman-group1-sha1` (`d35_no_common_kex_on_a_real_sshd_is_not_transient`), the classifier against the same
class, reason and text without the marker, and the worker retry through the iOS provider (1 attempt for the failed
exchange, 2 for a dropped connection followed by a failed login). The test for every wire reason still pins reason 0 as
transient.

## D36 / A17: a connection lost after authentication

**Ledger row checked:** correct. `start_session` returned `IllegalStateException("Not connected")` for any transport
that was not alive, and `safe_execute` catches only the two sshj classes.

**What stays a state error.** `is_connected()` is false for two different reasons, so the transport's first death now
records which. After our own `close()` of a live connection, using the client is the caller's mistake and stays
`IllegalStateException`, past `safe_execute`, as in Kotlin. `Not authenticated`, and the `not_connected` errors of
`auth_round` and `into_connection` on a client that never connected, are unchanged too. Only `start_session` on a
transport that died gives `ConnectionException("Not connected")` caused by the death.

**Disposal of a lost worker.** The flag is set only by the close that ended the transport: the first death wins, under
its lock. So a worker's drop, then disposal closing the executor, then a task on that device still calling
`safe_execute` gives null, with the worker's reason in the cause. russh closes the handle before it reports a death
(see below), so a close in that gap also counts as lost. `WorldExecutor` models this, and
`closing_a_connection_the_server_already_dropped_keeps_it_a_lost_connection_d36`, pin it.

**How recovery really happens.** Not through `is_transient_connection_failure`. That classifier takes a
`ConnectionError::Device`, which only the connect path and the simulator inventory build, and connect starts no session.
Execute errors also leave `SshFailure` at `into_vendor()` in `executor.rs`. So an earlier draft of this fix, which gave
the failure a marker for the classifier, had a clause nothing could reach, and it was removed. The route is:

- iOS: `simulator_provisioner.rs` (`Err(error) if !executor.connected()`) reports the setup unreachable,
  `simulator_provider.rs` (`executor.connected()` around planning, and `lost` after provisioning) makes the targets
  missing, and `SimulatorRecovery` retries them; the monitor in `recovery.rs` tests `d.connected()` first.
- macOS: `classify_execute` turns the failure into `DeviceLost`, and the actor terminates. There is no reconnect, before
  or after D36.

**Callers of `start_session`:** only `executor.rs::execute`. **Callers of `execute`:**

| Caller | Was (`IllegalStateException`) | Now (`ConnectionException`) |
|---|---|---|
| `critical_execute` and everything critical | Propagates | Propagates |
| iOS `classify`, macOS `classify_execute` | Lost device: their class lists hold both | The same |
| Provisioner `!executor.connected()`, `setup` planning | Unreachable, class-agnostic | The same |
| `ConnectionFactory::connect` | Not reached: connect starts no session | The same |
| `safe_execute` | Propagates | Null, logged |

**Callers of `safe_execute`.** A null was already possible, for a connection that died during the command; what is new
is that every later command also gets one. So the question for each is what used to stop it:

| Caller | With a null | Bounded by, or why it is harmless |
|---|---|---|
| iOS `fetch_device_descriptor` (`simulator_state`, `boot_failure`, `wait_for_state`, `erase_device`, `shutdown_device`) | Would read as state UNKNOWN, so as a failed boot: `LeavePool(NotBooted)` | **Guarded:** fails with `ConnectionException` when the executor is not connected |
| iOS `setup_record` `getenv`s | Would give `Unknown` defaults and register the simulator | **Guarded:** fails at the end of setup, so the provisioner reports it unreachable |
| iOS `boot_failure` loop | `is_running` null reads as booting | The next descriptor read fails (guard), and `bootWait` bounds it anyway |
| iOS `wait_for_state` | - | 30 rounds, and the same guard |
| iOS and macOS `load_worker_facts`/`setup`: `env` | `DeviceSetup` error instead of the state error | An error either way; the provisioner tests `connected()` first |
| macOS `setup`: `uname -m` | Would give `Unknown` abi and register the device, if the drop falls just before this last probe | **Guarded:** fails when the executor is not connected. A failed probe on a live connection still gives `Unknown` |
| `remote_file_manager`: `exists`, `test`, `execute`, `safe_remove` | Answers "no" or does nothing | The next critical command or transfer fails; `skipCopyingDerivedData` reports `unable to check`, retried by prepare |
| `nm` | `complete` scans already fail with `could not be started`, retried by the parser | Bounded by the parser's attempts; the class of the last error differs |
| `xcresulttool`, `plistbuddy`, `xcodeselect`, `terminate`, `screenshot`, `pkill`, `ps`, video pid reads and health check | No result, already handled | Each runs once; the video waits have a 10 s limit |

**Can `connected()` lag the failing command, so that a guard lets a null through?** Not for a dead transport:

- A null from `Not connected` comes from the same `is_connected()` test, and that state never reverts.
- A null from a channel failure needs the pump to see `wait_dead`, which is the death itself, or russh's channel queue
  to close. In `client/mod.rs`, `run` closes the handle's receiver right after `run_inner`, so `is_closed()` is already
  true, and channel senders are dropped later, with the session. `disconnect_during_exec_fails_execute_with_the_wire_reason`
  asserts `!connected()` at once after the failed command, for three wire reasons.

What the guards do not catch is a connection nobody knows is dead yet: on a silent network each probe waits for its 30 s
reply and fails with `TimeoutException` while `connected()` is true, until the keep-alive declares the transport dead
(60 s by default, so within the four `getenv`s of iOS setup; with keep-alive disabled, never). A null there means the
same as in Kotlin, and D36 did not change it, so it is left.

A batch that fails on such a null with another class is not counted as a lost device at once, but nothing is left
wrong: the actor takes the next batch, whose first critical command fails as a lost connection, and the iOS monitor
tests `connected()` before anything else. The batch's tests are retried as for any failed batch.

**No infinite reconnect.** Nothing reconnects on a null, and `execute` never reconnects. A server that always drops
after authentication: connect succeeds, planning or setup fails, `!executor.connected()` makes the targets missing, and
`SimulatorRecovery::retry` counts an attempt with its usual pause. It stops at `workerConnection.attempts`, or with late
join repeats every `retryInterval` until the late-join cut-off, as it does for any unreachable worker.
`a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36` shows one connection per acquire, whatever
number of commands were refused. A simulator can no longer register on a connection known to be dead because of the setup guard,
which closes that way to the monitor's uncounted `disconnected` path. A drop after setup finished is as before.

**Verified:** the fake's `after_auth`, once and repeatedly, on the wire reason 11 of the JVM evidence;
`during_exec`'s next command; a close of a live connection, and a close of one already dropped; a real OpenSSH 10.5p1 whose session process is killed
(`d36_a_killed_server_session_makes_safe_execute_return_null`). `WorldExecutor` now tells the two cases apart as the real
executor does, so every iOS world test that sets `connected` to false runs on the new class.

**Fails without it:** with `start_session` reverted, `disconnect_after_auth_makes_safe_execute_return_null_d36`,
`a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36` and
`disconnect_during_exec_fails_execute_with_the_wire_reason` fail. Removing the descriptor guard fails
`a_connection_lost_during_prepare_is_not_a_failed_boot_d36`, and removing the setup guard fails
`setup_on_a_lost_connection_fails_instead_of_describing_the_simulator_with_defaults_d36`. Setting the flag on every
close fails `closing_a_connection_the_server_already_dropped_keeps_it_a_lost_connection_d36`, and removing the macOS
guard fails `setup_on_a_connection_lost_before_the_last_probe_fails_instead_of_using_the_default_abi_d36`.

## D37 / A32: one stream unread

**Ledger row checked:** correct, and a detail matters for the design: sshj grants credit only on a read that returns
data, and only while the window is at or below its threshold (512 KiB). A read that comes earlier grants nothing. So a
stalled stream usually blocks the channel well below the cap, in Kotlin and here: the overflow needs stderr reads at the
right moments, which the oracle's sleeps and our test's waits arrange. That early block is unchanged, and was already
how a stalled consumer behaved.

**The fix.** `window_credit` caps a grant by `MAX_BUFFER - 1 - max(stdout, stderr) - window`, not by the stream just
read. The peer can spend the whole window on either stream, so the invariant `fullest + window <= 16 MiB - 1` is what
makes overflow unreachable for a conforming peer. In-flight bytes count: they are still in the local `window` until the
pump delivers them. Bytes moved from window to buffer leave the sum as it was.

**Why the maximum and not the sum.** Each stream's limit stays sshj's own buffer size, so a single stream behaves
exactly as before for any amount, and `slow_consumer_receive_window_stays_within_the_buffer_cap` still reaches
16 MiB - 1. Memory per channel is bounded by two buffers.

**The ordinary case.** `grants_are_unchanged_while_neither_stream_is_near_its_cap_d37` compares every grant with sshj's
over a grid: with at most 16 MiB - 2 MiB - 1 unread in the fuller stream, the adjustments are identical, whichever
stream is read. So throughput cannot change when output is read promptly. Only above that does the grant shrink.

**What guarantees progress when a consumer has stopped.** Stdout and stderr sharing a window means the worker blocks
and the other stream stops too. No consumer reads one stream to its end before the other: `await_result` and `drain`
collect in two tasks, and `consume_output` joins both. So a stream stops only because a callback is stuck, or nobody
consumes at all. Then:

| What ends it | How | Needs window? | Real OpenSSH 10.5p1, at the cap | Fake, at the cap |
|---|---|---|---|---|
| The consumer resumes | A read that returns data grants credit up to the cap again; nothing is lost | - | All data, exit 7 (`d37_reading_only_stderr_…`) | All data, exit 0 (`reading_only_stderr_blocks_at_the_cap_…_d37`) |
| Command timeout | `exit_code` sends TERM, then CLOSE, with a null exit code. Every command has a finite timeout (`Durations.INFINITE` is 7 days) | No | Exit null (`d37_at_the_cap_other_sessions_flow_and_the_command_timeout_ends_it_…`) | TERM sent; exit null, or the 30 s close wait expires (`a_stuck_consumer_at_the_cap_is_ended_by_the_command_timeout_d37`) |
| Idle timeout | The other stream's reader is not blocked. It sees no data, so its idle check fires and fails the consumer | No | `TimeoutException` (`d37_at_the_cap_the_idle_timeout_ends_it_…`) | `TimeoutException` (`a_stuck_consumer_at_the_cap_is_ended_by_the_idle_timeout_d37`) |
| Session close | TERM, KILL after 20 s if the process has not ended (D30), CLOSE, then the line channels are cancelled, which frees a producer stuck on a full one | No | Returns in 0 ms (`d37_at_the_cap_close_returns_…`); 20,004 ms and `Ok` for a process that ignores TERM (`d37_at_the_cap_kill_ends_…`) | TERM sent; returns, or the 30 s bound expires (`a_session_blocked_at_the_cap_can_be_closed_d37`); TERM and KILL sent (`kill_at_the_cap_does_not_turn_a_close_into_another_error_d37`) |
| Cancellation | Producers and consumers select on it | No | - | Existing session tests |

**How these tests know they reached the cap.** They would otherwise block on the initial window, as an earlier version of
them did: with stderr once per MiB, each stderr read found the window above the 512 KiB threshold and granted nothing, so
the session stopped near 2 MiB, which is pre-D37 behaviour and passes without the fix. So the floods now write stderr
every 64 KiB, and `ssh::wait_for_the_cap` requires `ConnectionShared::peak_unread` to reach exactly 16,777,215. That
counter exists only under the `test-support` feature. The command timeout fires when the test says so, through
`GatedTimeoutClock`, so it cannot race the way to the cap on a loaded host.

A stuck callback could always stall its own consumer until one of these fires; D37 adds no such path, and only at the
cap does it replace the connection's death with that wait. A worker that finished writing while its output sits behind
the closed window still counts as running until the output is read, because its CLOSE follows the output.

**Other sessions.** SSH has no connection-level window, so a closed channel window spends nothing shared. russh's
session loop does `chan.send(...).await` into a bounded queue, which could stall the transport, but `pump` drains that
queue without ever waiting for a consumer (it only appends to `State`), and window adjustments go out through each
channel's own writer task. `reading_only_stderr_blocks_at_the_cap_…_d37` runs a command on the same connection while
blocked, and `stalled_consumer_blocks_only_its_own_channel` also has the keep-alive answered.

russh does have one shared stall, and the fake's server side shows it (U153): with data held past the window, its session
loop takes no message from any channel's handle, and the reply to CLOSE waits too. The client's loop has the same gate
(`client/mod.rs`, `can_receive_outbound`), so this was checked: `pending_data` only holds a side's own channel data, and
the executor never sends any; EOF is not sent either. Window adjustments, exec, signals and CLOSE are not data. So a
client's receive window, closed or open, cannot arm the gate.

This limits what the fake can show. It reached that state in about 1 run in 10 at the cap, so the fake tests assert what
does not depend on the fake answering: the cap, the connection surviving, TERM on the wire, and a close that either returns
or ends in exactly the 30 s wait's expiry (2 of 25 runs took that branch). The other session while blocked, the exit code
of the timeout without that alternative, and the close time are asserted on real OpenSSH only, which has no such gate.
Those real tests are opt-in, so the gate does not run them.

**The cap's protection.** `receive` keeps its check: `a_peer_that_ignores_the_window_still_hits_the_cap` drives the
channel directly, since the fake honours the window, and memory stops at the cap with sshj's `Attempted overflow`.

**The flood scenario.** With `fixtures/oracle/ssh/flood.jsonl`'s script, stderr only:

| | JVM oracle | Before D37 | After, fake | After, real OpenSSH 10.5p1 |
|---|---|---|---|---|
| stderr bytes read | 15 | Below 32 | Below 32 | 15–16 |
| stdout buffered | 16,744,448 | Above 8 MiB | Within 64 bytes of the cap | 16,777,215 |
| Connection | Lost | Lost | **Alive** | **Alive** |
| Next command on it, while blocked | Fails | Fails | Not asserted (U153) | Works |
| After reading stdout | - | - | All 32 MiB, 32 stderr bytes, exit 0 | All 32 MiB, 32 stderr bytes, exit 7 |

**Fails without it:** with the credit computed from the read stream again, all four of
`reading_only_stderr_blocks_at_the_cap_…_d37`, `a_stuck_consumer_at_the_cap_…_d37` (both) and
`a_session_blocked_at_the_cap_can_be_closed_d37` fail on the fake, and all four `d37_*` tests fail on real OpenSSH: the
connection is lost, at 16,760,832 unread bytes for the three that use a session.
`both_streams_read_promptly_deliver_everything_d37` passes on both, as it should.

## Combined with x13 (D29–D32) and x17 (D38)

Rebased on main after both were merged, and checked against them.

**`channel.rs`: a read after CLOSE, and x13's rejected session.** A read after CLOSE still grants credit and queues an
adjust. `Encrypted::close` has already forgotten the channel on the client, which never holds pending data, and
`grow_window_size` does nothing for a channel it no longer knows, so the adjust goes nowhere and is no error
(`a_read_after_the_close_only_queues_an_adjust_that_russh_skips_d37`, which then runs another command on the connection). A
rejected `exec` never reads, so `take` grants nothing for it; if `ExecChannel` has been dropped the pump drains without
buffering, so `receive` and the cap are not involved. The unit tests build `ExecChannel` directly, and x13's new fields
take their defaults.

**`session.rs`: x13's close at the cap.** It does not take 20 s when TERM works, and it is right that it does when TERM does
not:

| Process at the cap | Close on OpenSSH 10.5p1 | Why |
|---|---|---|
| Ends on TERM | 0 ms, `Ok` | Its exit status is a channel request, which needs no window, so the wait for the process ends at once |
| Ignores TERM | 20,004 ms, `Ok` | D30's wait, then KILL and CLOSE. Same as T30 without the flood (20,005 ms) |

KILL does not turn a clean close into an error: it is only sent when the 20 s have passed, and the process it ends answers as
usual. On the fake, the deferred exit status and CLOSE of U153 make the same close end in D30's 30 s bound instead, whose
error text is unchanged, so `assert_closed_or_close_wait_expired` still holds, for D30's reasons now. The D37 tests that
close stay at factor 1, because they accept either outcome and assert no successful close; the one that must wait out
D30's steps, `kill_at_the_cap_…`, runs at factor 5. The real tests run at factor 1.

**D36's error classes against KILL on a dead transport.** A dead transport rejects the KILL (`let _ = self.signal(Sig::KILL)`),
and in fact the close never gets that far: the pump has failed the channel, so the wait for the process ends first. The close
ends at once with `ConnectionException` over the death, and the next `safe_execute` returns null
(`a_transport_that_dies_during_the_close_is_a_connection_failure_d36`). No state error appears.

**D32's `env NSUnbufferedIO=YES`.** More small stderr packets mean more reads that can grant, so the cap comes sooner. That is
what D37 is for: `stderr_in_small_packets_reaches_the_cap_without_losing_the_connection_d37` floods with stderr every 16 KiB
and stops at exactly 16,777,215. Finer is only slower, because each packet costs the fake a transcript line. The real run has
no xcodebuild; the variable changes only when a process writes, which the flood scripts control directly.

**D38: a null `is_running` on a dead connection.** Confirmed, and now pinned: `boot.rs` reads the null as "not running", so state
3 stays `Booting`. Its loop starts with the next `simulator_state`, whose descriptor read fails on the guard in
`fetch_device_descriptor`, before the deadline is looked at, so the simulator is not reported as failing to boot
(`a_null_readiness_probe_on_a_lost_connection_is_not_a_failed_boot_d36_d38`). That guard is the only thing that prevents it:
without it this test and `a_connection_lost_during_prepare_is_not_a_failed_boot_d36` fail.

## What was run

`cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-test-support
--all-targets -- -D warnings`; the tests of those four crates; `tests/ssh_real_sshd.rs` against throwaway loopback OpenSSH
10.5p1 servers with temporary keys, removed afterwards; `make_registry.py --check` and `test_evidence.py`. Each fix was
reverted temporarily to see its tests fail. For the D37 tests: 25 runs on the fake (no failures) and 6 on OpenSSH
(no failures, the cap reached exactly each time). There was no load run.

## Not verified

- `scripts/check.sh`, `scripts/remote-check.sh` and the workspace test suite: left to the orchestrator by this batch's
  gate rule, so Linux has not run these changes, on the fake or against a real sshd.
- A worker over a real network: every real run was on loopback, where the JVM's and our measured numbers agree.
- Kotlin on the new tests: the JVM oracle records the old behaviour by design, and was neither re-recorded nor edited.

## Incidental bugs

- `LineBuffer` holds a line until it ends, with no limit, and so does Kotlin's. A worker that writes a very long line
  with no newline grows marathon's memory, whatever the window does: `both_streams_read_promptly_…_d37` runs on the
  oracle's newline-free script and holds 32 MiB there. D37 keeps the limit it was asked to keep, on the channel's
  buffers, and this is left alone.
- Vendored russh 0.64.1, server side, seen through the fake only: U153 in `docs/found-bugs.md`. Patching it would let
  the fake assert the strict outcomes too. That is a change to a vendored crate, so it is left for the owner to decide.
- `docs/research/ssh.md` still says that a failed key exchange is transient (T26/A16). It records what was found at the
  time, so it was not edited.
