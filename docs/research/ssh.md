# SSH spike (WP 0.3a): sshj behaviour inventory, russh mapping, fallback, test matrix

Research note for the SSH spike. Sources: marathon `e47352f2`
(read-only at `$MARATHON_KT`), sshj 0.39.0
(`buildSrc/src/main/kotlin/Versions.kt:38`, `com.hierynomus:sshj`), russh
**0.64.1** ([docs.rs](https://docs.rs/russh/latest/russh/), 2026-10-05,
[repo](https://github.com/warp-tech/russh)), ssh2 **0.9.6**
([docs.rs](https://docs.rs/ssh2/latest/ssh2/struct.Channel.html), 2026-09-19).
PLAN refs: §3.6 (commands/quirks), §4.2 (russh choice), §6.4 (fake worker), §9
(real workers: `ssh remote-mac` macOS 26.7.1/openrsync; OpenSSH container).

Kotlin paths below are relative to
`vendor/vendor-apple/base/src/{main,test}/kotlin/com/malinskiy/marathon/apple/`.

## 1. sshj behaviours marathon depends on

### 1.1 One exec channel per command, `env`/`workdir` ignored

`execute()` opens a fresh session and exec channel for every command on the
shared connection and never reuses channels
(`cmd/remote/ssh/sshj/SshjCommandExecutor.kt:47-48`):

```kotlin
val session = client.startSession()
val cmd = session.exec(shellCommand(command))
```

`PatchedSSHClient.startSession()` opens a `PatchedExecChannel` instead of
sshj's `SessionChannel` (`sshj/PatchedSSHClient.kt:13-19`). `env` and
`workdir` are accepted by the signature but never sent (no `setEnv`, no
`reqEnv` anywhere in the call) — PLAN §3.6 keeps this. The Rust port must not
"fix" it by sending env requests.

### 1.2 `shellCommand` quoting (bug-compatible)

`shellCommand` (`SshjCommandExecutor.kt:79-87`) joins argv with spaces; per
argument: left as-is when "shellscaped", else single-quoted when it contains a
special char, else bare. Quirks to keep (PLAN §3.6):

- `isShellscaped` (`:92-96`) checks only the first and last character
  (`'...'` or `"..."`), never the contents — a half-quoted or internally
  inconsistent string passes through untouched.
- `containsShellSpecialChars` (`:97-102`) matches exactly
  `" $ ' \ # = [ ] ! > < | ; { } ( ) * ? ~ &` plus space. Notably `=`
  triggers quoting (so `FOO=bar` args are quoted), while `/ - . , : + @`
  do not.
- The single-quote "escape" (`:83`, `it.replace("'", "\\'")`) is wrong for
  POSIX shells: inside single quotes backslash is literal, so an argument
  containing `'` produces a string the worker shell parses differently
  (the correct idiom would be `'\''`). Keep as-is; add a row to
  `docs/known-kotlin-bugs.md` if none exists.
- `WriteFile` depends on this: it passes `listOf("sh", "-c", script)` and the
  base64 script (no single quotes in it) gets wrapped in `'...'`
  (`cmd/WriteFile.kt:32-35`).

### 1.3 Stream reading: two polled readers, one shared idle clock

Both streams are read by `produceLinesManually` (`extensions/Reader.kt:39-101`)
with 16 KiB chunks gated on `InputStream.available()` (never a zero-length
read — sshj's `ChannelInputStream` blocks on those, see the comment at
`:63-65`). Details:

- Stop condition is `canRead = { cmd.isOpen && !cmd.isEOF }`
  (`SshjCommandExecutor.kt:51-52`); after EOF/close the loop breaks and
  `LineBuffer.drain()` flushes a trailing partial line *with* an appended
  newline (`cmd/LineBuffer.kt:48-59`). `flush()` emits only complete lines
  (`:26-46`); `\r`, `\r\n`, `\n` all become `\n` (`:21`).
- The idle timeout is **shared** across stdout and stderr through one
  `AtomicLong lastOutputTimeMillis` (`SshjCommandExecutor.kt:50`): output on
  *either* stream resets *both* readers' clocks. It fires when
  `now - lastOutput - backoff.maxMillis (20 ms) > idleTimeout`
  (`Reader.kt:84-89`), i.e. up to ~2 poll delays (40 ms) late by construction
  (comment at `:81-83`), and closes the channel with
  `TimeoutException("idle timeout ...")`, which `BaseCommand.await()` turns
  into a failed `await()` via channel-close-with-cause.
- Consumers must drain both streams concurrently (`consumeOutput`,
  `cmd/CommandSessionOutput.kt`); `await()` collects both then the exit code
  (`cmd/BaseCommand.kt:25-61`).

### 1.4 Extended data and the EOF race (fork and JetBrains patches)

`PatchedExecChannel` (`sshj/connection/channel/direct/PatchedExecChannel.kt`):

- `gotExtendedData` (`:20-34`): data-type 1 → stderr stream; **any other type
  → `ConnectionException(PROTOCOL_ERROR, "Bad extended data type = ...")`**.
  Real servers only ever send 1, so this fires only against a misbehaving
  peer — but the Rust port must reproduce the failure, not ignore the packet.
- `receiveInto` (`:40-53`): mutes exactly `"Getting data on EOF'ed stream"`,
  the race where channel packets arrive after local close. Any other
  `ConnectionException` is rethrown. (Upstream origin: JetBrains fork,
  copyright header `:1`; reshaped by the fork in `07e0b3fa`.)

### 1.5 Exit wait: poll, single `join`, TERM → 143

`suspendFor` (`SshjCommandExecutor.kt:104-123`) with `PollBackoff` 1 ms → 20 ms
max (`extensions/PollBackoff.kt:7-22`; ~500 polls per 10 s of runtime,
`SshjCommandExitStatusTest.kt:40-51`):

- While `isOpen`, poll only. Once closed, a single `join(1, NANOSECONDS)`;
  a `ConnectionException` caused by `TimeoutException` (join poll timeout) is
  retried, any other (e.g. `CONNECTION_LOST` after keep-alive gives up) is
  rethrown (`SshjKeepAliveTest.kt:53-64`).
- Then `close()`, and return `exitStatus ?: autodetectExitCode(exitSignal)`
  (`:121-122`): `Signal.TERM → 143`, any other signal → `null` (`:125-134`).
  Cancelling the wait coroutine leaves the channel untouched (no join, no
  close — `SshjCommandExitStatusTest.kt:53-63`).

### 1.6 Command timeout → `null` (TERM then close)

`execute()` (`SshjCommandExecutor.kt:53-62`): `withTimeoutOrNull(timeout)` (plain
`kotlinx.coroutines.withTimeoutOrNull`, `core/.../extension/Timeout.kt:6`)
around `suspendFor()`; on expiry it sends `signal(TERM)` **then**
`cmd.close()` and the `Deferred` completes with `null`. Note the signal goes
to the whole channel (process group semantics depend on the worker shell —
`sh -c '...'` children may survive TERM; cf. the ignored-TERM test in §1.9).
`CommandExecutor`'s contract says `execute` never throws a timeout
(`cmd/CommandExecutor.kt:28-30`).

### 1.7 `close`/`terminate`/`kill`/`isAlive`

`SshjCommandSession` (`cmd/remote/ssh/sshj/SshjCommandSession.kt:20-41`):

- `isAlive = command.isOpen`.
- `terminate()` → `signal(TERM)`; `kill()` → `signal(KILL)`. `close()` calls
  `terminate()` when open, then `command.close()`, then **blocking unbounded
  `command.join()`**, then cancels the supervisor job (stopping readers and
  the exit-code deferred) — `BaseCommand.close()` (`cmd/BaseCommand.kt:76-83`).
  `close()` is once-only via `AtomicBoolean`. There is **no KILL escalation**
  on the close path. Measured in WP 0.3a (T15, `ssh-spike-results.md`): the
  `command.close()` before the `join()` waits for the peer's CLOSE only up to
  sshj's connection timeout (30 s) and then throws, so the `join()` is not
  reached; it waits without a bound only when the channel was already closed
  locally, e.g. by the timeout branch (T15h).
- `safeExecute` catches `ConnectionException`/`TransportException` around both
  `execute()` and `await()` (`cmd/CommandExecutor.kt:76-85`) and returns
  `null`, so a transport death mid-command also gives `null` there; callers of
  `execute()`/`await()` directly see the exception.

### 1.8 Keep-alive: KEEP_ALIVE, whole-second truncation, max count, loss

- `PerformanceDefaultConfig` sets `KeepAliveProvider.KEEP_ALIVE`
  (`.../sshj/config/PerformanceDefaultConfig.kt:52-55`): heartbeat requests
  that **expect replies** and kill the transport after too many misses
  (HEARTBEAT would only send `SSH_MSG_IGNORE`). fork commit `b0b4d42f`.
- `SshjCommandExecutorFactory.createClient` (`SshjCommandExecutorFactory.kt:80-90`):
  `keepAliveInterval = keepAliveInterval.toSeconds().toInt()` — **truncates to
  whole seconds** (a 1500 ms config becomes 1 s; sub-second becomes 0), names
  the thread `"$addr:$port keep-alive"`, sets `maxAliveCount =
  keepAliveMaxCount`. `SshjKeepAliveTest.kt:28-37` pins provider + both values.
- Defaults from `SshConfiguration` (`configuration/.../apple/SshConfiguration.kt:18-20`):
  interval 15 s, max count 4 → a silent worker is declared lost after ~60 s;
  `keepAliveMaxCount < 1` is a config error (`:41-43`); interval 0 disables
  (rsync side is explicit about it, `RsyncFileBridge.kt:146-154`; sshj side
  gets interval 0 — spike must record what `KeepAliveRunner` does with 0).
- On loss the transport dies with
  `ConnectionException(CONNECTION_LOST, "Did not receive any keep-alive
  response for 60 seconds")`; in-flight `suspendFor` rethrows it (§1.5), and a
  lost *shared* connection is replaced, never re-handed
  (`device/ConnectionFactory.kt:160-164`).

### 1.9 Termination semantics (from `BaseCommandExecutorTest`)

- `testTerminate` (`:172-199`): TERM a busy loop → exit **143**, stdout
  truncated (3–10 lines, fuzzy).
- `testTerminateShouldNotCloseExecutor` (`:201-223`): after TERM, the same
  connection runs `echo hello` fine — signal + close are channel-scoped.
- `testInterruptUsingKillShouldNotCloseExecutor` (`:225-248`): `sleep 100000
  &` (backgrounded, ignores the channel TERM) exits non-zero — the "TERM that
  is ignored" case; exact code is OS-dependent (`shouldNotBeEqualTo 0`).
- `testTimeout` (`:155-170`): `sleep 100` with 10 ms timeout → `null`.
- `testBuffering` (`:84-101`): 100 000 lines through the bounded channel;
  `testConsumeOutputDrainsStreamsConcurrently` (`:104-129`): 100 000 stderr
  lines + stdout `start/end` must both arrive; consumer failure must not wedge
  `close()` (`:131-153`).

### 1.10 Connect: fresh client per attempt, 4 attempts, no delay

`SshjCommandExecutorFactory.connect` (`SshjCommandExecutorFactory.kt:44-65`):
`withRetrySync(connectionAttempts = 4, delay 0)` (warn-log + `Thread.sleep(0)`,
`core/.../execution/Retry.kt:26-40`) wraps **only** `client.connect()`; every
attempt builds a **new** `PatchedSSHClient` because sshj swaps the socket
without closing the old one (comment at `:44-47`); each failed client is
closed. Authentication runs once, outside the retry — auth failure closes the
client and throws (`SshjCommandExecutorFactoryTest.kt:21-45`). Host id string
is `"user@addr:port"` (`:58`, `SshjCommandHost.kt:21`). Concurrency cap: 8
simultaneous handshakes (`device/ConnectionFactory.kt:202-213,289` — just
under OpenSSH `MaxStartups 10`).

### 1.11 Host-key verification and auth

- `knownHostsPath == null` → `PromiscuousVerifier` (accept-any);
  otherwise `OpenSSHKnownHosts(file)` — OpenSSH-format known_hosts
  (`device/ConnectionFactory.kt:230-232`). A configured-but-missing file is a
  *parse-time* `FileNotFoundException` (`SshConfiguration.kt:38-40`).
- Password: `authPassword(user, password)`; public key:
  `loadKeys(path)` + `authPublickey`
  (`.../sshj/auth/SshAuthentication.kt:10-28`). `loadKeys` takes **no
  passphrase**, so passphrase-protected keys fail here, not at config
  validation (config only checks the file exists, `SshConfiguration.kt:33-36`).
  sshj 0.39.0 `loadKeys` supports PKCS8, PEM-encoded PKCS1, PuTTY key files
  and `openssh-key-v1`
  ([javadoc](https://javadoc.io/static/com.hierynomus/sshj/0.39.0/net/schmizz/sshj/SSHClient.html)).
- Session charset is `client.remoteCharset`
  (`SshjCommandExecutorFactory.kt:57-62`, sshj default UTF-8).

## 2. Reproducing each item with russh 0.64.1

Client API: `russh::client::connect(config, addr, handler)` → `Handle<H>`;
per-command `Handle::channel_open_session()` → `Channel<Msg>`,
`Channel::exec(bool, cmd)`, `Channel::signal(Sig)`, `Channel::eof/close`,
`Channel::wait()` → `Option<ChannelMsg>`
([Channel](https://docs.rs/russh/latest/russh/struct.Channel.html),
[ChannelMsg](https://docs.rs/russh/latest/russh/enum.ChannelMsg.html)).
`Sig` has `TERM/KILL/…` plus `Custom(String)`
([Sig](https://docs.rs/russh/latest/russh/enum.Sig.html)).

| # | Behaviour (§1) | russh reproduction | Gap |
|---|---|---|---|
| 1.1 | exec channel per command; ignore env/workdir | `channel_open_session` + `exec(true, shell_command)` per call; simply never send env requests | none |
| 1.2 | `shellCommand` quoting | pure function, port verbatim (bugs included) incl. `'` → `\'` | none; add known-bug row |
| 1.3 | polled readers, shared idle clock, CRLF→LF, drain of partial line | `wait()` loop splitting bytes on `\n` after the same `\r\n`/`\r` normalisation; one shared `Instant` across both stream tasks; idle check `elapsed - poll_slack > idle_timeout` → fail with the same `idle timeout …` message | `wait()` is push-based so the ≤40 ms poll slack is timing-only; keep the slack constant in the comparison so timeout-edge tests agree |
| 1.4 | ext≠1 → PROTOCOL_ERROR; mute post-close data race | russh delivers `ExtendedData{data, ext}` without judging: fail the command when `ext != 1`; accept `Data` until `Close`/`wait()`→`None` and ignore `Error::Disconnect` on a closing channel | must be hand-built; spike row T14 |
| 1.5 | poll → single join → `exitStatus ?: TERM→143` | loop `wait()` until `ExitStatus{exit_status}` / `ExitSignal{signal_name,..}` / `Close`+`None`; map `TERM→143`, other→`null`; no polling needed | unknown signal names arrive as `Sig::Custom(s)` — decide `s=="TERM"→143` in spike (sshj `Signal` lookup behaviour on unknown names is itself a spike row) |
| 1.6 | timeout → TERM, close, `null` | On timeout send TERM and close; wait for peer CLOSE, returning null only if close succeeds | R1: first close can fail after 30 s; T15h measures ~31 s including the command timeout, subsequent close can join without a bound |
| 1.7 | close = TERM-if-open, close, join, no KILL escalation | First close waits up to 30 s and may throw before join; a subsequent close skips that wait and joins without a bound (off the core thread) | T15/T15h distinguish these paths; R1 delivers the peer CLOSE |
| 1.8 | keep-alive with truncation + max count | Truncate to whole seconds, disabling probes below 1 s; maintain outstanding request promises independently of incoming channel traffic | R7: stock russh resets misses on any traffic; T25 requires a separate queue/timer. Silent-peer loss is (max + 1) × interval |
| 1.10 | 4 fresh-client attempts, no delay, auth outside retry | drop + rebuild `Handle` per attempt, 4 tries, no sleep; authenticate once after | map `russh::Error::{Disconnect, Timeout,…}` to the §1.12 taxonomy; row T12 |
| 1.11 | known_hosts vs accept-any; key/password auth | Own sshj-compatible verifier; key loading via russh, password then keyboard-interactive | T13 confirms unencrypted PPK v2/v3 support; T17 requires malformed-line skipping and sshj matching rules, including multiple same-algorithm keys |
| 1.12 | transient taxonomy | classify the russh error chain into the same transient/non-transient split; auth/host-key errors stay non-transient; failed KEX is transient when sshj reports UNKNOWN | A16 and T26: preserve the measured UNKNOWN classification despite the Kotlin KDoc; explicit disconnect reasons take precedence |
| 1.13 | rsync argv/`--rsh` | unchanged: spawn system rsync with the identical argv built in Rust | none (host-side shim work belongs to 0.4a) |

Cross-cutting: russh logs via the `log` facade — wire it into the tracing
setup with the same ERROR-unless-`ssh.debug` rule, and keep exec command
lines out of non-debug logs (`WriteFile` secrecy, §1.11). There is no random-
factory or cipher-suite tuning to port (`PerformanceDefaultConfig`'s Bouncy
Castle/JCE memoisation has no Rust analogue and no observable behaviour).

## 3. russh as SSH *server* for the fake worker (WP 0.4a)

Server API: `russh::server::Handler` (`auth_none/password/publickey(_offered)/
keyboard_interactive/certificate/succeeded/banner`, `channel_open_session`,
`channel_open_*`, `data/extended_data`, `exec/pty/shell/subsystem/env/agent/
x11/window_change_request`, …) with `Session` methods `channel_success/
channel_failure/channel_open_failure`, `data/extended_data/eof`,
`exit_status_request/exit_signal_request`, `close/disconnect`, `request_success/
request_failure`
([Handler](https://docs.rs/russh/latest/russh/server/trait.Handler.html),
[Session](https://docs.rs/russh/latest/russh/server/struct.Session.html)).

- **Exec routing**: `exec_request(channel: ChannelId, data: &[u8], session)`
  receives the full `shellCommand` string; the scripted world parses/routes it
  and replies `data` (stdout) / `extended_data(ch, 1, …)` (stderr) →
  `exit_status_request` → `eof` → `close`, the same order OpenSSH uses.
- **Session limits**: enforce the `MaxSessions`-like cap inside
  `channel_open_session` (count live channels, reject beyond N); the spike
  must record which `ChannelOpenFailure` code russh sends and what sshj vs
  russh clients surface for it (row T08). A `MaxStartups`-style handshake
  throttle is a separate, optional fake feature.
- **OpenSSH client interop** (rsync's `--rsh`, stock `ssh -F /dev/null`):
  algorithm overlap (kex/ciphers/host-key types) is broad on both sides, but
  three things must be verified in the spike: (a) password logins through a
  stock client arrive as `auth_password`, `keyboard-interactive`, or both —
  `sshpass` answers either, but the fake must implement whichever OpenSSH
  actually uses (implement both if needed); the parity-fixed `--rsh` string
  forbids client-side `PreferredAuthentications` steering (§1.13). (b) `rsync
  --server …` over exec with stdio piped to a real `rsync --server` child in
  both directions (PLAN §6.4). (c) `SendEnv`/agent/pty requests from stock
  ssh must be answered (accept or reject), never left hanging.
- **Signal delivery to scripted commands** (hang/timeout scenarios, TERM→143):
  confirm in the spike that the server handler observes signal-requests
  (which `Handler` method, and whether `Sig::Custom` names survive); the fake
  then kills the scripted task and answers `exit_signal_request(TERM)`. If
  russh 0.64.1 does not surface signal requests, that is a fake-worker design
  constraint to resolve in 0.4a (e.g. emulate via close/EOF), not a client
  blocker.
- **Key/auth matrix for the fake**: password accept/reject, publickey
  accept/reject (key compared by fingerprint), unknown-user behaviour; host
  key generated at fake startup (Ed25519).

## 4. Fallback analysis

### 4.1 `ssh2` 0.9.6 / libssh2 — not recommended

- **No channel signals.** The
  [Channel method list](https://docs.rs/ssh2/latest/ssh2/struct.Channel.html)
  (`exec, exit_status, exit_signal, eof, close, wait_close/wait_eof, …`) has
  no `send_signal`, and upstream libssh2 implements no signal-request API in
  any released version. TERM→143, `terminate()`/`kill()`, and the
  still-usable-executor tests (§1.9) cannot be reproduced — a hard parity
  failure on PLAN §3.6, which names signals explicitly.
- **Weaker keep-alive.** `Session::set_keepalive[_interval]` drives
  `libssh2_keepalive_send`, which emits `SSH_MSG_IGNORE` only: no reply
  tracking, no transport kill. The dead-worker detection (§1.8) would need an
  app-level watchdog — reimplementing `KeepAliveRunner`.
- **Blocking only.** Every call blocks a thread (`wait_eof/wait_close` have
  no async form); channel-per-command with concurrent stream draining means a
  thread pool sized by channels, fighting the lifecycle rules (no blocking on
  the core thread, `docs/design/lifecycle.md` pending).
- What *is* fine: `exit_signal` (read side), `handle_extended_data`, OpenSSH
  known_hosts files, PEM/OpenSSH key formats. Not enough to outweigh the
  signal gap. Keep ssh2 as the fallback only if the spike finds a russh gap
  that cannot be closed — and note that any such gap must then be weighed
  against ssh2's unclosable signal gap.

### 4.2 Other crates

- `openssh` (wraps the stock `ssh` binary, multiplexed): exact wire behaviour
  for free, but client-side quirks (truncation, single-join, close/join hang,
  error taxonomy) would have to be re-derived from OpenSSH's behaviour rather
  than controlled — wrong layer for bug-parity. Not recommended.
- `thrussh`, `async-ssh2*`: dead fork / thin wrappers inheriting the same
  gaps as their backends. Not recommended.
- Recommendation stands: **russh**, contingent on spike rows T08–T15.

## 5. Spike test matrix (real sshd)

Targets: (A) `ssh remote-mac` (macOS 26.7.1, arm64, OpenSSH server, openrsync);
(B) the `SshdContainer` image `lscr.io/linuxserver/openssh-server:9.0_p1-r2-ls99`
(`SshdContainer.kt:8-44`) run locally for destructive cases (keep-alive loss,
MaxSessions, firewall). Every row runs against **sshj first** (record the
exact result), then the russh trial implementation; the russh column must
match or the difference goes to `docs/known-kotlin-bugs.md`/deviations.

| ID | Behaviour | Provoke | Expected sshj result |
|---|---|---|---|
| T01 | stdout/stderr/exit | `echo Hello world`; `bash -c '1>&2 echo …'`; `sh -c 'exit 1'` | lines split, streams separated, exit 0/0/1 (`BaseCommandExecutorTest:25-82`) |
| T02 | buffering | `sh -c 'seq 1 100000'`; `echo start; seq 1 100000 1>&2; echo end` | 100k lines both streams, `start/end` intact, exit 0 (`:84-129`) |
| T03 | `shellCommand` quoting | args with spaces, `$`, quotes, leading/trailing quotes, `=`; record the wire string (`ssh.debug`) | quoting rules of §1.2 incl. the `\'` and half-quoted passthrough quirks |
| T04 | env/workdir ignored | `execute([…], env={F=1}, workdir=/tmp)` | remote env/workdir unchanged |
| T05 | timeout → null | `sleep 100`, timeout 10 ms | `null` exit; channel signalled+closed (`:155-170`) |
| T06 | TERM → 143, executor survives | busy loop + `terminate()`; then `echo hello` on same connection | 143, truncated stdout; second command `hello` (`:172-223`) |
| T07 | ignored TERM | `sleep 100000 &` then await; `trap '' TERM; sleep 30 & wait` + `terminate()` | non-zero exit, executor usable (`:225-248`); record exact codes per target |
| T08 | MaxSessions exhaustion | sshd `MaxSessions 1–2`, hold N sleepers, open one more | `startSession`/`open` fails; record exception type/message for both clients; connection stays usable |
| T09 | keep-alive loss | DROP traffic (iptables/pause B) with interval 2 s, max 2 | `CONNECTION_LOST "Did not receive any keep-alive response…"` in ≤ ~2·2+slack s; in-flight `suspendFor` rethrows |
| T10 | keep-alive truncation | interval 1500 ms → observe requested interval; interval 0 | whole seconds (1 s); record what interval 0 does in `KeepAliveRunner` |
| T11 | keep-alive healthy | interval 1–2 s, idle 60 s on both targets | no spurious loss; thread named `addr:port keep-alive` |
| T12 | connect retries | closed port (refused) → connect; wrong password → connect | refused: 4 attempts then throw; auth failure: 1 attempt, client closed (`SshjCommandExecutorFactoryTest`) |
| T13 | key formats/auth | ed25519, RSA-PEM, PKCS8, openssh-v1, PuTTY `.ppk`, encrypted-no-passphrase; wrong password | unencrypted formats including PPK v2/v3 succeed; encrypted-without-passphrase fails (concrete errors in spike results) |
| T14 | EOF/late data, ext types | fast-exit with output; large output with immediate close; (ext≠1 only against the russh fake in 0.4a) | complete streams, no `EOF'ed stream` leak; ext≠1 → PROTOCOL_ERROR (fake only) |
| T15 | close/join hang | server that never closes the channel (fake, 0.4a) + `close()` | first close fails at 30 s; after a timed-out local close, subsequent join is unbounded until peer CLOSE/connection loss (T15h) |
| T16 | signals other than TERM | `sh -c 'kill -KILL $$'`; `kill -SEGV $$` | `null` (not 143) |
| T17 | known_hosts | matching / mismatched / hashed entries; missing file | match ok; mismatch fails auth-setup (non-transient); missing file → `FileNotFoundException` at validation |
| T18 | `WriteFile` | roundtrip ≤32 KiB, oversize, non-plain `dst` | `true`/`false` per `WriteFile.kt:28-45`; command absent from non-debug logs |
| T19 | transient taxonomy | refused, reset, DNS failure, `TOO_MANY_CONNECTIONS` (MaxStartups pressure), auth failure, bad host key | transient/non-transient split per `ConnectionFailures.kt:27-38` |
| T20 | rsync `--rsh` exactness | record argv of one send + one receive on target A | byte-equal to §1.13 strings (with/without `-vvv`, key vs password) |

Spike exit criteria (per WP 0.3a): russh-or-fallback decision with rows
T08–T15 resolved; containerised sshd committed for the `SshjCommandExecutorTest`
equivalents; every sshj/russh difference filed as a known-bug row or a
deviation proposal.
- sshj internal logging goes through marathon loggers at ERROR, or DEBUG with
  `ssh.debug` (`PerformanceDefaultConfig.kt:25-47`); the exec command line is
  logged at DEBUG — which is why `WriteFile` (content inside the command) is
  forbidden with `ssh.debug` (`WriteFile.kt:19-23`,
  `.../sshj/config/PerformanceDefaultConfigTest.kt:7-16`).

### 1.12 Transient-failure taxonomy

`isTransientConnectionFailure` (`device/ConnectionFailures.kt:27-38`, fork
commit `03fd0075`): only `DeviceFailureException(UnreachableHost)` (which
`ConnectionFactory.connect` wraps every `IOException` into, `:202-213`, plus
DNS/reachability failures at `:244-261`) whose cause chain (≤ 16) contains a
`TransportException` with disconnect reason in `UNKNOWN, CONNECTION_LOST,
TOO_MANY_CONNECTIONS, BY_APPLICATION, SERVICE_NOT_AVAILABLE, MAC_ERROR`
(`:12-19`) or a `SocketException/SocketTimeoutException/UnknownHostException/
TimeoutException` (sshj reports a late auth reply as `TimeoutException`).
Auth failure, bad host key and unreadable key → **not** transient. A no-common-algorithm key exchange is actually
**transient** via sshj `UNKNOWN` (T26/A16), despite the Kotlin KDoc saying otherwise.

### 1.13 rsync side (for the fake-worker half of the spike)

`RsyncFileBridge` (`cmd/remote/rsync/RsyncFileBridge.kt`): rsync4j argv `-a
--partial --partial-dir=.rsync-partial --delay-updates --rsync-path=<remote>
[--verbose]` + `--delete` (the `.a()` helper, `:166-177`); `--rsh` string is
exactly `ssh -o 'StrictHostKeyChecking no' -F /dev/null [-o
ServerAliveInterval=N -o ServerAliveCountMax=M] {-i key | } -l user -p port
[-vvv]` (`:110-141`) — accept-any on the rsync path **always**, even with
`knownHostsPath` set. Sends are serialised by a mutex (`:41`); the debug ctor
runs `rsync --version` locally (`:93-98`).

## WP 0.3a review corrections (handoff to WP 4.2)

- **D7 is approved:** suppress the single stale TERM after a non-TERM exit; sshj's following close is a no-op.
- R4's measured 30 s macOS wrong-password timeout has an unresolved cause. sshj recognises and answers `Password:`;
  do not prescribe silence as parity behaviour. Remote-mac follow-up is pending remote.
- Keep-alive must track unanswered requests independently of channel traffic (T25), not use russh's traffic-reset timer.
- Known_hosts needs sshj's multiple-key, whitespace, case, wildcard and negation rules (T17).
- Streams need independent up-to-16-KiB reads, shared idle activity and bounded consumers, not packet boundaries (T21).
- No-common-algorithm KEX is actually transient via sshj's `UNKNOWN`, despite ConnectionFailures.kt's KDoc (T26/A16).
- The measured contracts and remaining limits are in `ssh-spike-results.md`, including the WP 4.2 checklist.
