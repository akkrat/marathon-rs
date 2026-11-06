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
