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
