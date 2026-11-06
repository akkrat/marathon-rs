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

**What stays a state error.** `is_connected()` is false for two different reasons, so `disconnect_locally` now records
which: after our own `close()`, using the client is the caller's mistake and stays `IllegalStateException`, past
`safe_execute`, as in Kotlin. `Not authenticated`, and the `not_connected` errors of `auth_round` and `into_connection`
on a client that never connected, are unchanged too. Only `start_session` on a transport that died gives
`ConnectionException("Not connected")` caused by the death, with a marker for the classifier. So a text match cannot
misclassify anything, and neither can a server's message.

**Why every wire reason is transient here.** At connect, reason `PROTOCOL_ERROR` from a server is not transient, but
this connection did authenticate. That reason stays visible in the cause.

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
| macOS `setup`: `uname -m` | `Unknown` abi, only if the drop falls just before this last probe | Prepare then fails on its first critical command, and the actor terminates |
| `remote_file_manager`: `exists`, `test`, `execute`, `safe_remove` | Answers "no" or does nothing | The next critical command or transfer fails; `skipCopyingDerivedData` reports `unable to check`, retried by prepare |
| `nm` | `complete` scans already fail with `could not be started`, retried by the parser | Bounded by the parser's attempts; the class of the last error differs |
| `xcresulttool`, `plistbuddy`, `xcodeselect`, `terminate`, `screenshot`, `pkill`, `ps`, video pid reads and health check | No result, already handled | Each runs once; the video waits have a 10 s limit |

A batch that fails on such a null with another class is not counted as a lost device at once, but nothing is left
wrong: the actor takes the next batch, whose first critical command fails as a lost connection, and the iOS monitor
tests `connected()` before anything else. The batch's tests are retried as for any failed batch.

**No infinite reconnect.** Nothing reconnects on a null, and `execute` never reconnects. A server that always drops
after authentication: connect succeeds, planning or setup fails, `!executor.connected()` makes the targets missing, and
`SimulatorRecovery::retry` counts an attempt with its usual pause. It stops at `workerConnection.attempts`, or with late
join repeats every `retryInterval` until the late-join cut-off, as it does for any unreachable worker.
`a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36` shows one connection per acquire, whatever
number of commands were refused. A simulator can no longer register on such a connection because of the setup guard,
and that closes the only way to the monitor's uncounted `disconnected` path. A drop after setup finished is as before.

**Verified:** the fake's `after_auth`, once and repeatedly, on the wire reason 11 of the JVM evidence;
`during_exec`'s next command; the local close; a real OpenSSH 10.5p1 whose session process is killed
(`d36_a_killed_server_session_makes_safe_execute_return_null`). `WorldExecutor` now tells the two cases apart as the real
executor does, so every iOS world test that sets `connected` to false runs on the new class.

**Fails without it:** with `start_session` reverted, `disconnect_after_auth_makes_safe_execute_return_null_d36`,
`a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36` and
`disconnect_during_exec_fails_execute_with_the_wire_reason` fail. Removing the descriptor guard fails
`a_connection_lost_during_prepare_is_not_a_failed_boot_d36`, and removing the setup guard fails
`setup_on_a_lost_connection_fails_instead_of_describing_the_simulator_with_defaults_d36`.

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

| What ends it | How | Needs window? | Test (fake and real OpenSSH) |
|---|---|---|---|
| The consumer resumes | A read that returns data grants credit up to the cap again; nothing is lost | - | `reading_only_stderr_blocks_at_the_cap_…_d37`, `d37_reading_only_stderr_…` |
| Command timeout | `exit_code` sends TERM, then CLOSE, with a null exit code. Every command has a finite timeout (`Durations.INFINITE` is 7 days) | No | `a_stuck_consumer_is_ended_by_the_command_timeout_d37`, `d37_a_stuck_consumer_…` |
| Idle timeout | The other stream's reader is not blocked. It sees no data, so its idle check fires and fails the consumer | No | `a_stuck_consumer_is_ended_by_the_idle_timeout_d37` |
| Session close | TERM, CLOSE, then the line channels are cancelled, which frees a producer stuck on a full one | No | `a_session_blocked_at_the_cap_closes_d37`, `d37_a_stuck_consumer_…` (1 ms) |
| Cancellation | Producers and consumers select on it | No | Existing session tests |

A stuck callback could always stall its own consumer until one of these fires; D37 adds no such path, and only at the
cap does it replace the connection's death with that wait. A worker that finished writing while its output sits behind
the closed window still counts as running until the output is read, because its CLOSE follows the output.

**Other sessions.** SSH has no connection-level window, so a closed channel window spends nothing shared. russh's
session loop does `chan.send(...).await` into a bounded queue, which could stall the transport, but `pump` drains that
queue without ever waiting for a consumer (it only appends to `State`), and window adjustments go out through each
channel's own writer task. `reading_only_stderr_blocks_at_the_cap_…_d37` runs a command on the same connection while
blocked, and `stalled_consumer_blocks_only_its_own_channel` also has the keep-alive answered.

russh does have one shared stall, and the fake's server side hit it in about 1 run in 10 (see Incidental bugs): its
session loop takes no message from any channel's handle while any channel has `pending_data`. The client's loop has the
same gate (`client/mod.rs`, `can_receive_outbound`), so this was checked: `pending_data` only holds a side's own channel
data, and the executor never sends any; EOF is not sent either. Window adjustments, exec, signals and CLOSE are not
data. So a client's receive window, closed or open, cannot arm the gate. The test now grants credit only with nothing in
flight, so that it does not depend on the fake's race; the real OpenSSH run reads at the threshold and has no such gate.

**The cap's protection.** `receive` keeps its check: `a_peer_that_ignores_the_window_still_hits_the_cap` drives the
channel directly, since the fake honours the window, and memory stops at the cap with sshj's `Attempted overflow`.

**The flood scenario.** With `fixtures/oracle/ssh/flood.jsonl`'s script, stderr only:

| | JVM oracle | Before D37 | After, fake | After, real OpenSSH 10.5p1 |
|---|---|---|---|---|
| stderr bytes read | 15 | Below 32 | Below 32 | 15 |
| stdout buffered | 16,744,448 | Above 8 MiB | Within 64 bytes of the cap | 16,777,215 |
| Connection | Lost | Lost | **Alive** | **Alive** |
| Next command on it | Fails | Fails | Works | Works |
| After reading stdout | - | - | All 32 MiB, 32 stderr bytes, exit 0 | All 32 MiB, 32 stderr bytes, exit 7 |

**Fails without it:** with the credit computed from the read stream again,
`reading_only_stderr_blocks_at_the_cap_…_d37` and `d37_reading_only_stderr_…` fail, because the connection dies. The
timeout and close tests pass on both: they pin the progress guarantee, not the overflow.

## What was run

`cargo fmt --check`; `cargo clippy -p marathon-apple -p marathon-ios -p marathon-macos -p marathon-test-support
--all-targets -- -D warnings`; the tests of those four crates; `tests/ssh_real_sshd.rs` in full (19 tests) against two
throwaway loopback OpenSSH 10.5p1 servers with temporary keys, removed afterwards; `make_registry.py --check` and
`test_evidence.py`. Each fix was reverted temporarily to see its tests fail. The flood test ran 30 times in a row on its
own after its fix; there was no stress run.

## Not verified

- `scripts/check.sh`, `scripts/remote-check.sh` and the workspace test suite: left to the orchestrator by this batch's
  gate rule, so Linux has not run these changes, on the fake or against a real sshd.
- A worker over a real network: every real run was on loopback, where the JVM's and our measured numbers agree.
- Kotlin on the new tests: the JVM oracle records the old behaviour by design, and was neither re-recorded nor edited.

## Incidental bugs

- `LineBuffer` holds a line until it ends, with no limit, and so does Kotlin's. A worker that writes a very long line
  with no newline grows marathon's memory, whatever the window does: `both_streams_read_promptly_…_d37` first ran on
  the oracle's newline-free script and held 32 MiB there. D37 keeps the limit it was asked to keep, on the channel's
  buffers, and this is left alone.
- Vendored russh 0.64.1, server side, seen through the fake only. A window adjust does `window_size().update(new_size)`
  from the session's count, which has not yet subtracted data queued as messages, so a writer can reserve past the
  real window. The excess lands in `pending_data`, and `server/session.rs` then takes nothing from `receiver` for any
  channel until that window reopens: exec replies of other sessions wait (the client's 30 s `Timeout expired`). No
  marathon code runs this in production. It is for docs/found-bugs.md and an upstream report; a number is left to the
  orchestrator, since other branches are adding rows at the same time.
- `docs/research/ssh.md` still says that a failed key exchange is transient (T26/A16). It records what was found at the
  time, so it was not edited.
