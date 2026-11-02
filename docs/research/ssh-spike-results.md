# SSH spike (WP 0.3a): russh 0.64.1 against sshj 0.39.0

Measured on 2026-10-12 against the Kotlin marathon at `e47352f2` (its own `SshjCommandExecutorFactory`,
`SshjCommandExecutor`, `SshjCommandSession`, `writeFile` and `RsyncFileBridge`, not imitations) and a minimal
russh executor (`spikes/ssh-russh`). The matrix is T01–T20 of `docs/research/ssh.md` §5.

## Decision

- **russh: go, with hand-built parity work still required.** This is a library feasibility decision, not a claim that
  the throwaway executor is ready for production. Pin 0.64.1 with R1; implement the R3–R7 contracts below in WP 4.2.
  D7 deliberately suppresses sshj's stale TERM after a non-TERM exit. In particular, stock russh's keep-alive and
  packet-based stream handling are not sufficient for parity.
- **Crate versions to pin:**
  - `russh =0.64.1`, default features (`aws-lc-rs`, `rsa`, `flate2`), with `spikes/ssh-russh/russh-close-after-local-close.patch` applied. Vendor the crate, or use a `[patch.crates-io]` git fork; also offer the fix upstream;
  - transitive, already exact or locked: `ssh-key =0.7.0-rc.11` (pinned by russh itself), `ssh-encoding 0.3.0`, `russh-cryptovec 0.62.0`, `russh-util 0.52.0`, `aws-lc-rs 1.18.1`;
  - `tokio 1.53.2` as locked by the spike.
- **Hand-built parity for WP 4.2:** R1 close state/callback patch; R3 known_hosts parsing/matching; R4 password →
  keyboard-interactive sequence and error classification (timeout cause unresolved); R5 error shapes, cancellation,
  exec rejection and disconnect reasons; R6 independent readers/UTF-8/CRLF/backpressure; R7 unanswered keep-alive promises.
  The checklist below is the handoff contract. `shellCommand` and the `writeFile` script stay byte-identical.
- **PuTTY keys: no deviation needed.** `ssh_key::PrivateKey::from_ppk` loads unencrypted PPK v2/v3 (ed25519 and RSA).
  Encrypted keys without a passphrase fail in both clients; see T13's concrete errors below.
- **D7 approved:** do not reproduce the single stale TERM request that sshj sends after a non-TERM exit. The following
  `close()` is a no-op on that already-closed channel. Rust keeps the connection usable; no second russh patch is needed.
- **Changes for PLAN.md at merge:** confirm russh 0.64.1 with R1, close the PuTTY question, and retain approved D7.
  This branch has an older base; the orchestrator owns reconciliation with the current plan.
  - §6.4: the fake worker needs no russh change for exec, signal, password, keyboard-interactive or rsync. There are three caveats:
    - russh's server answers a client CLOSE itself, so a "server never closes" scenario (T15) works only by blocking the handler, which stalls that whole connection;
    - `sshpass` failed about 1 in 10 logins against both real OpenSSH and the fake, so host-side retries or a different password feeder are needed for stable differential runs;
    - the fake host key must not land in the user's `known_hosts`; `StrictHostKeyChecking no` writes it.
  - §9: port 22 of the remote Mac is Tailscale SSH, not macOS OpenSSH. It reports exit status 0 for every command, sends no exit-signal and ignores the channel TERM, so marathon cannot use it as a worker. The real sshd is reachable only through a forward (see Setup).

## Setup

- **sshj harness** (`spikes/ssh-sshj`): Kotlin, compiled with the Kotlin 1.9.24 compiler from the Gradle cache against
  the jars of the Kotlin marathon's installed distribution (its `cli/build/install` tree, copied into `build/lib`; nothing
  is built in the Kotlin repository).
  - The distribution was built from `d511c021`, eight commits before `e47352f2`. None of the SSH executor sources changed in between.
  - `device/ConnectionFailures.kt` (new in `e47352f2`) is copied into the harness unchanged.
  - It calls the internal `SshjCommandExecutorFactory.connect(newClient, …)`, `createClient` and `shellCommand` through `-Xfriend-paths`, as the Kotlin tests do.
- **russh executor** (`spikes/ssh-russh`): standalone package, excluded from the workspace. Same case names and output
  keys; `spikes/compare.sh` diffs the comparable keys. `build-patched.sh` builds a second binary against russh with the R1
  patch (client `russh-patched`).
- **Targets:**
  - `container`: `lscr.io/linuxserver/openssh-server:9.0_p1-r2-ls99` (the SshdContainer image; OpenSSH 9.0p1, bash login shell), run with colima;
  - `container` limits variant: `MaxSessions 2`, `MaxStartups 1`;
  - `remote-mac`: the Mac's own sshd (OpenSSH_10.3, macOS 26.7.1, zsh login shell), reached through a client-side forward over the `ssh remote-mac` master connection;
  - fake worker: `ssh-russh-spike serve`.

  `spikes/run-case.sh` resolves the targets at run time, so no address or user name is stored.
- **Infrastructure notes for remote-mac:**
  - Port 22 of the remote Mac address is Tailscale SSH (banner `SSH-2.0-Tailscale`, auth `none`), not OpenSSH. While the spike ran it was first in check mode, where every new connection waits for a browser approval; later it accepted new connections directly.
  - The Mac's real OpenSSH server is reachable only through a local forward over the approved `ssh remote-mac` connection (`127.0.0.1:22022 → 127.0.0.1:22` on the Mac). That forward is how all the `remote-mac` rows ran.
  - The only key in the local agent is not authorised there. With the user's approval, a throwaway ed25519 key (comment `marathon-rs-spike-<random>`) was appended to the Mac's `~/.ssh/authorized_keys` for the authenticated rows (`spikes/targets/remote-mac-key.sh`, cleanup in a trap). The file did not exist before; afterwards it no longer exists, so the Mac is back to its previous state, and the local key files were deleted.
  - One slip: T18 (`writeFile`) also ran on the remote Mac, because the runner's container-only guard for it was missing; it is fixed now. The case writes `/tmp/spike-write-small` and `/tmp/spike-write-max` and deletes both at its end. Afterwards no `/tmp/spike-write-*` files and no spike processes were left on the Mac.
- **Keys** (`spikes/targets/keys.sh`): OpenSSH-format keys from the local `ssh-keygen`. The PuTTY files come from `puttygen` (Alpine `putty` package) in a disposable container. Nothing was installed on the host apart from colima, the docker CLI and `sshpass` (`hudochenkov/sshpass` tap, needed for deliverable 5).

Run: `targets/container.sh up`, `ssh-sshj/build.sh`, `cargo build` and `build-patched.sh` in `ssh-russh`, then
`run-matrix.sh <sshj|russh|russh-patched> <container|remote-mac>`, `fake-worker-check.sh`, `t20/run.sh` (with the fake on :22230),
and `compare.sh <target> [client]`. For remote-mac's authenticated rows: `targets/remote-mac-key.sh ./run-matrix.sh <client> remote-mac`.

## Results

The T01–T20 table retains the original 2026-10-12 observations; it is not a count of exact comparator passes.
The round-1 rerun and comparison policy below supersede its parity claims. `russh` means unpatched 0.64.1;
`patched` has R1. Container is C, remote-mac is M. Remote-mac follow-ups are pending remote; original observations are historical.

| Row | sshj 0.39.0 (Kotlin marathon code) | russh 0.64.1 | Verdict | Fix |
|---|---|---|---|---|
| T01 stdout/stderr/exit | C, M: `[Hello world]` / stderr `[Hello world]` / exit 1; host id `user@addr:port` | same | match | – |
| T02 buffering, concurrent drain, consumer failure | 100 000 lines, ordered; `[start, end]` + 100 000 stderr; failed consumer then `close()` returns (C 1 ms, M 295 ms) | streams same; `close()` after the failed consumer **hangs** (C, M) | **mismatch** (patched: match, C 0 ms, M 10 ms) | R1 |
| T03 `shellCommand` quoting | 19 arguments: wire strings incl. `'it\'s'`, `'\'half'`, half-quoted passthrough `'mixed"`, bare empty argument; `'`-containing args are shell syntax errors (C bash exit 2, M zsh exit 1) | identical wire strings and results | match | port verbatim (known bug A12) |
| T04 env/workdir ignored | C rerun: `F=unset`, PWD `/config`; M exact PWD and absence check measured in WP 4.2 (`F=unset`, home; `docs/wp/4.2-findings.md`) | C same after correcting harness home normalisation | C match; M match | ignore env/workdir |
| T05 timeout → null | null after ~10 ms (C 81 ms, M 163 ms total); remote `sleep` gone | null (C 53 ms, M 25 ms); remote `sleep` gone | match | executor must end the streams itself after the local close (R1 note) |
| T06 TERM → 143, executor survives | busy loop 143 with 5 lines; `sleep 1000` 143; `echo hello` after | same (C, M, fake) | match | – |
| T07 ignored TERM | `sleep 100000 &` → `sleep` rejects `&` (quoted), exit 1, OS-specific message; `trap "" TERM` + TERM → exit 0 `survived` after ~4 s; `close()` waits for the process (C 4004 ms, M 4103 ms) | first two same; `close()` **hangs** | **mismatch** (patch repairs close; concurrent await parity remains open, see round 2) | R1 |
| T08 MaxSessions 2 (C only) | 3rd `execute` throws `OpenFailException` (a `ConnectionException`, reason "open failed"), `safeExecute` → null, connection usable, held sessions 143 | `ChannelOpenFailure(ConnectFailed)`; same null, usable, 143 | match | R5: map to the start-failure path |
| T09 keep-alive loss, 2 s × 2 (C only, `docker pause`) | `ConnectionException[CONNECTION_LOST]("Did not receive any keep-alive response for 4 seconds")` after 5945 ms; in-flight `await()` fails; `connected` false | `KeepaliveTimeout` after 5705 ms; command fails; `connected` false | match | R5: same error kind and message text |
| T10 truncation (C only) | 1500 ms → interval 1 s, loss after 3023 ms; 999 ms and 0 → keep-alive off, no detection in 15 s, command finishes after resume | `keep_alive_interval()` truncation gives 1 s / off; 2990 ms; no detection | match | truncate explicitly (as the spike does) |
| T11 healthy keep-alive 1 s × 2 | `sleep 60` exit 0, connected (C, M); keep-alive thread is renamed by sshj to `sshj-KeepAliveRunner-/addr:port-…`, so marathon's `"addr:port keep-alive"` name never shows | same; keep-alive runs inside the session task | match | – (thread names are an accepted difference, §3.10) |
| T12 connect retries | C: refused → 4 attempts, `ConnectException`, transient; wrong password → 1 attempt, `UserAuthException`, non-transient. M: closed port is filtered → 4 × timeout (67 s), transient; wrong password → **30 s, `TimeoutException`, transient** (R4) | C same; M filtered same; wrong password → 3.7 s, non-transient | **mismatch** (M wrong password) | R4 |
| T13 key formats (C) | ed25519/ecdsa/RSA openssh-v1, RSA PEM, PKCS8, PPK v2 + v3 (ed25519, RSA): ok. Encrypted openssh, PEM, PPK: fail, non-transient. Missing file: `FileNotFoundException`, non-transient | same successes and failures (`KeyIsEncrypted`, `Ppk(Encrypted)`, `NotFound`) | match | R5: messages |
| T14 EOF and late data | partial line `[a, b, partial]`; `x\r\ny\rz\n\n\r\nw` → `[x, y, z, , , w]`; 300 000 lines + exit 3; 50 000 + 50 000; flood then `close()` returns (C 2 ms, M 92 ms), no "EOF'ed stream" error; idle timeout `TimeoutException("idle timeout PT1S reached")` | same streams; flood `close()` **hangs**; idle timeout fires with its own message | **mismatch** (patch repairs close; concurrent await must also be compared, see round 2) | R1, R5 (message), R6 |
| T15 close against withheld CLOSE (fake) | First close waits 30 s, then throws before join | Revised executor waits 30 s, then fails | see rerun below | F1: close timeout differs from subsequent unbounded join |
| T16 signals other than TERM | KILL/SEGV/INT/HUP/USR2 → null, TERM → 143; then one stale TERM request: C connection **dropped** for every non-TERM signal; M dropped after INT and HUP | same exit codes; connection always survives | **mismatch** | D7 approved |
| T17 known_hosts | plain and hashed ok; mismatch, unknown host, other algorithm only, missing file → `HOST_KEY_NOT_VERIFIABLE`, 4 attempts, non-transient; a malformed line before the good entry is skipped → ok | same, except the malformed line fails the whole check (base64 error), 4 attempts | **mismatch** (malformed line) | R3 |
| T18 `writeFile` (C; also ran on M) | 6-byte and 32 KiB files written, md5 equal; 32 KiB + 1, `/tmp/a b`, a missing directory → false; identical wire string | same | match | port the script verbatim |
| T19 transient taxonomy | refused, filtered (timeout), DNS, reset (`Broken pipe` before the banner), MaxStartups drop ("Server closed connection during identification exchange") → transient; auth (C), host key → not; M wrong password → transient (R4) | refused, timeout, DNS (`Uncategorized` io error), reset (`ConnectionReset`), MaxStartups (`Disconnect`) → transient; auth, `KeyChanged` → not | **mismatch** (M auth) | R4, R5 |
| T20 rsync `--rsh` | argv recorded through marathon's `RsyncFileBridge` (see below); key auth roundtrips on C and the fake | not applicable here: the rsync path spawns system `rsync`/`ssh`/`sshpass` and does not use the SSH library | n/a | – (0.4a/4.3) |

The original “12 match / 7 mismatch / 1 n/a” count is withdrawn: it mixed semantic equivalence with raw comparison,
ignored close failures, and counted skipped rows as equal. T20 is a separate rsync feasibility test; T15 uses the fake.
The rerun reports comparator counts for the rows actually measured, including missing or incomplete results.

**T20 argv** (from the shims; `<keys>`/`<work>` are temporary paths):
```
rsync --recursive --links --perms --owner --group --devices --specials --times
      --rsh "ssh -o 'StrictHostKeyChecking no' -F /dev/null -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -i <keys>/ed25519_openssh -l test-user -p 22220 "
      --rsync-path=/usr/bin/rsync --delete --partial --partial-dir=.rsync-partial --delay-updates <work>/src/ 127.0.0.1:/config/t20/
password: sshpass -p <password> <rsync> … same, with "-l test-user -p 22220 " and no -i
remote (send):    /usr/bin/rsync --server --delete-before -g -l --partial-dir .rsync-partial -o -p -D -r -t --dirs --delay-updates . <dst>
remote (receive): /usr/bin/rsync --server --sender --delete-before -g -l -o -p -D -r -t --dirs --delay-updates . <src>
```
The `--rsh` string ends with a space when `-vvv` is off. The local client is openrsync (protocol 29), which rewrites
`--delete` to `--delete-before` and adds `--dirs` on the server side. Receive repeats `--links` (rsync4j `.links(true)`
plus `a()`).

## Findings for WP 4.2 (hand-built parity)

- **R1: no CLOSE after a local close (russh 0.64.1).** `Session::close` removes the channel from `enc.channels`.
  - What goes wrong: the peer's EOF and CLOSE that follow are then dropped as "not established", while the channel's sender stays in the client's table. `Channel::wait()` therefore never returns `Close` or `None`, and a client-initiated close (sshj's `close()` → `join()`) can never complete.
  - Where it shows: T02, T07 and T14 hang; the timeout path's streams never end.
  - The patch: `russh-close-after-local-close.patch` records only confirmed channels closed locally, consumes that record on the peer reply, delivers `ChannelMsg::Close`, releases the sender and calls `Handler::channel_close`. Pending-open channels are untouched; duplicate CLOSE does not call the callback again. Packet-level and live-channel tests cover these paths.
  - With it, close times match sshj, including the ~4 s wait for a process that ignores TERM.
  - Data that arrives after the local close is still dropped. That is exactly what the Kotlin `PatchedExecChannel.receiveInto` mutes, so no "EOF'ed stream" error is needed.
  - The executor must still stop its readers itself once it has sent CLOSE on a timeout, as sshj's `canRead` does.
- **R2 / D7: one stale TERM after a non-TERM exit.** In `SshjCommandExecutor.execute`, `if (result == null) { signal(TERM); close() }` runs on any null exit code, not only on a timeout.
  - The request goes out for a channel that is already closed, and OpenSSH treats it as a protocol error (`server_input_channel_req: unknown channel 0`) and disconnects. Every other command on that shared connection then fails.
  - The user approved D7: suppress that stale request. The subsequent sshj `close()` does nothing; it does not put another CLOSE on the wire.
  - Known bug A13.
- **R3: known_hosts.** sshj's `OpenSSHKnownHosts` skips lines it cannot parse; `russh::keys::check_known_hosts_path` fails the whole check on the first malformed line.
  - Both handle hashed entries, `[host]:port`, and "other algorithm only = unknown".
  - WP 4.2 needs its own known_hosts reader with sshj's line rules; markers such as `@cert-authority` and `@revoked` were not tested.
  - A missing file behaves as an empty one in both clients; marathon rejects a missing file at config validation anyway.
- **R4: password authentication.** `SSHClient.authPassword(user, password)` tries `password` and then `keyboard-interactive`.
  - Against macOS sshd with a wrong password, auth failed at sshj's 30 s timeout with `UserAuthException ← TimeoutException`. sshj recognises the `Password:` challenge and writes a response. The cause of the timeout is unresolved; do not implement an intentionally unanswered challenge. Reverification is pending remote.
  - `isTransientConnectionFailure` then calls it **transient**, so the worker is retried instead of retired (known bug A15).
  - The container answers fast and non-transient.
  - The Rust executor needs the same method sequence, timeout and classification. Not tested here: whether a *correct* password on macOS succeeds through `password` or only through keyboard-interactive (no password was available).
- **R5: error shapes.** Each sshj exception that marathon logs, matches or classifies needs a Rust equivalent with the same message, and the transient mapping onto russh errors. The spike's `is_transient` (io errors, `Disconnect`, `HUP`, the timeouts and `KeepaliveTimeout` → transient; `AuthFailed`, `UnknownKey`, `KeyChanged`, key errors → not) gave the same split as Kotlin on every T19 case except R4. The ones that differ today:
  - `ConnectionException[CONNECTION_LOST]("Did not receive any keep-alive response for N seconds")`, with N = interval × max;
  - `OpenFailException` for refused channels;
  - `TimeoutException("idle timeout PT1S reached")`, which uses Java `Duration.toString`.
- **R6: streams.** Port `LineBuffer` exactly:
  - per-chunk lossy UTF-8 decoding, so split code points become U+FFFD as in Java;
  - CR/CRLF → LF;
  - drain on EOF and local close with an appended newline. T14.timeout_partial measures
    `sh -c 'printf partial; sleep 3'` with a 1 s command timeout: both clients return
    `exit=null stdout=[partial] stderr=[]`. Preserve the buffered tail even when close has stopped further reads;
    do not let a stalled drain block the channel pump from receiving peer CLOSE.

  Keep the 20 ms slack in the idle check. russh delivers packets, so there is no polling, but the comparison keeps the slack.
  Both expose backpressure, but a 64-line queue alone does not establish equivalence: Kotlin has two independent readers of up to 16 KiB, whereas this spike decodes SSH packets in one pump. WP 4.2 must preserve reader independence and test a stalled consumer on one stream while the other keeps producing. Exact corruption at arbitrary transport boundaries is not established by matching line counts.
- **R7: keep-alive semantics:** russh resets its miss counter on *any* received data. sshj counts unanswered keep-alive requests regardless of other traffic. Both declare loss after `(max + 1) × interval` of silence (5.7–5.9 s for 2 s × 2; 3.0 s for 1 s × 2). A peer that keeps sending data but never answers global requests would be lost under sshj and kept under russh. T25 measures this difference with a fake that withholds probe replies while output flows. It is a required parity item R7, not an approved deviation: use a separate outstanding-request queue/timer which ordinary traffic cannot reset.

## Fake-worker feasibility (input for 0.4a)

`spikes/fake-worker-check.sh` against `ssh-russh-spike serve`. The server is russh 0.64.1, runs each exec request with `sh -c`, and generates its host key from a file.

- **Stock OpenSSH client, key auth:** `/usr/bin/ssh -o 'StrictHostKeyChecking no' -F /dev/null -i key` worked. The agent's key is offered first and rejected, then the file key is accepted.
- **Password through `sshpass`:** worked. OpenSSH asks for `keyboard-interactive` first, so the fake must implement it: one `Password:` prompt. `password` is reached only after three failed keyboard-interactive rounds. A wrong password ends with exit 5. `sshpass` 1.06 (tap) failed about 1 in 10 logins against both the real container and the fake (`Permission denied`): cause unresolved; rerun by hand (fake-worker-check.sh has no retry loop).
- **Exec and signal reach the handler:**
  - `exec_request` receives the full `shellCommand` string;
  - `Handler::signal` receives `Sig::TERM` from both the sshj and the russh executors;
  - after the fake kills the child, it answers `exit_signal_request(TERM)` and both clients report 143 (T06 on the fake).
  - Stock ssh sent no `env` or `pty` requests in these runs. The fake rejects both if they come.
- **`rsync -e ssh` both ways:** `RsyncFileBridge`'s exact argv, with key auth and with sshpass, ran `/usr/bin/rsync --server` and `--server --sender` on the fake; the roundtrip was byte-identical. The rsync on both sides is the system openrsync (protocol 29), the same as the real workers. The spike's checks add `UserKnownHostsFile=/dev/null`, because `StrictHostKeyChecking no` would write the fake's key into the user's `known_hosts`.
- **"Server never closes" (T15):** russh's server replies to a client CLOSE itself, before calling `channel_close`. The fake can withhold that reply only by never returning from the handler, which stalls the whole connection. That is enough for the hang scenario, but not for a fake that withholds one channel's close while the others keep working; that needs a russh change.

## Extra observations

- **Tailscale SSH (port 22 of the remote Mac address), both clients alike:**
  - exit status is always 0 (`exit 1` → 0, every signal → 0);
  - there is no exit-signal;
  - the channel TERM is ignored, so T06's busy loop runs to the 5 s timeout and gives null.

  Marathon cannot drive a worker through it. Production workers are expected to run OpenSSH.
- **Login shells differ.** The container runs bash and the remote Mac runs zsh: the T03 syntax errors and the T07 `sleep` message differ by OS. On the Mac, a `.bashrc` line printed `mise: command not found` on stderr in T01's `bash -c` command, in both clients.

## What could not be run

- T08, T09, T10 and T15 on the remote Mac: they need sshd configuration changes, pausing the server, or a misbehaving server, so the container or the fake stands in.
- T13 on the remote Mac beyond one unauthorised key: key parsing is client-side, and the container covers it.
- T20 send to remote-mac: it would write to the Mac's filesystem.
- A correct password on the remote Mac (R4 open question): no password was available.
- Extended data of a type other than 1 (T14's protocol-error case): it needs a misbehaving server. The fake can send it in 0.4a; neither real sshd does.

## Round 1 measurement policy and concrete contracts

Rerun date: 2026-10-23. Both harnesses were rebuilt; the R1-patched crate passes 27 client tests, including actual CLOSE
packets for pending-open, local-close and duplicate-close states and live callback delivery. No remote-mac credentials were
created: the existing real-sshd forward worked, but agent authentication failed (`UserAuthException`, one attempt).
All new remote-mac-dependent measurements are **pending remote**. There were no saved raw results in `/tmp/ssh-spike` at takeover;
the original measurements above are historical, not silently re-certified by this rerun.

`compare.sh` now compares normal output keys, including normalized close/await/error outcomes. It excludes numeric
`.info.*` timing, library-specific error text/class names, and thread names; the error messages remain explicit contracts
below. A legacy nonnumeric `.info.close_ms`/`.info.flood_close_ms` outcome is retained. Each runner writes a status sidecar: running, completed,
failed or explicitly skipped. Only two completed, nonempty results can be equal; absent/failed/interrupted/legacy rows
are missing completion, two explicit skips are skipped, and absent/empty result directories fail. Differences and missing
completion give exit 1. `spikes/compare-check.sh` exercises these failures. Matching here does not certify unmeasured
packet boundaries, timing, or WP 4.2 error-message compatibility.

### Details requested by F6

- **T04:** C emits `F=unset`; `/config` is the actual PWD (normalized to `<home>` for comparison), not `/tmp`. The harness
  now distinguishes an absent F from an empty F. M exact PWD/absence verification: **pending remote**; do not commit the
  worker's user name in its home path.
- **T07:** C exact stderr lines: `sleep: invalid time interval ‘&’` and `Try 'sleep --help' for more information.`
  The argument is a quoted `&`, not shell backgrounding. M exact stderr: **pending remote** (the old note did not quote it).
- **T12:** C sshj wrong password: 234 ms, one attempt, `UserAuthException[UNKNOWN](Exhausted available authentication methods)`,
  non-transient. Connection refused: four attempts, 216 ms, `ConnectException(Connection refused)`, transient.
  These are single-run times, not fixed thresholds. R4's historical M 30 s timeout remains unresolved and pending remote.
- **T13:** sshj encrypted OpenSSH: `UserAuthException` → `KeyDecryptionFailedException(Decryption of the key failed. A supplied
  passphrase may be incorrect.)` → `EncryptionException(OpenSSH Private Key integer comparison failed)`.
  Encrypted PEM: `UserAuthException` → `PasswordException(Password is null, but a password is required)`.
  Encrypted PPK v3: `UserAuthException` → `IOException(Invalid passphrase)`. All are one attempt, non-transient.
  Unencrypted PPK v2/v3 ed25519/RSA all authenticate; there is no PPK format failure to emulate.
- **T16:** KILL/SEGV/USR2 on M are **pending remote**. Preserve the original INT/HUP observation only as historical evidence.
  D7 is approved for all non-TERM signals; raw status/signal spelling still needs normalization in the comparator contract.
- **T18:** For six bytes `hello\n` to `/tmp/spike-write-small`, the exact wire command is:

```sh
sh -c 'umask 077 && echo aGVsbG8K | base64 --decode > /tmp/spike-write-small.marathon-tmp && [ $(wc -c < /tmp/spike-write-small.marathon-tmp) -eq 6 ] && mv -f /tmp/spike-write-small.marathon-tmp /tmp/spike-write-small || { rm -f /tmp/spike-write-small.marathon-tmp; exit 1; }'
```

### WP 4.2 SSH parity checklist

No separate Muse checklist or WP 4.2 brief exists in this branch or current `main`'s docs (only the review's reference to
one). This consolidated checklist is ready for the orchestrator to put into that brief:

- [ ] Vendor R1, preserve the pending-open distinction and exactly-once close callback; run the crate's packet and live tests.
- [ ] Keep `close()`'s first 30 s wait/error distinct from the following unbounded `join()`; no KILL escalation. A command
  timeout must await close, and may throw instead of returning null. With keep-alive enabled, transport loss can end join.
- [ ] Apply D7: suppress the stale TERM on a channel already closed after a non-TERM exit; retain TERM → 143 and null for
  other signals. Complete pending remote signal/error rows when authorized credentials are available.
- [ ] Copy `shellCommand` quirks, ignored env/workdir, per-command exec channels and the A11 construction-time `isAlive` snapshot.
- [ ] Port sshj known_hosts matching as measured in T17: malformed-line skipping, any matching same-algorithm key,
  whitespace, case sensitivity, wildcard/negation/host lists, hashed entries and missing file. Do not substitute OpenSSH's
  expected semantics for sshj's measured ones. Markers/certificates remain unmeasured.
- [ ] Implement password then keyboard-interactive fallback; preserve the measured timeout/error mapping without assuming
  its unresolved cause. Copy encrypted-key and unsupported-passphrase failures; unencrypted PPK is supported.
- [ ] Preserve error/cause text and the transient whitelist, including explicit wire disconnect reasons and failed-KEX
  `UNKNOWN` (A16). Post-auth pre-exec disconnect can yield `IllegalStateException("Not connected")` (A17).
- [ ] Catch only `ConnectionException`/`TransportException` equivalents in `safeExecute`, across both execute and await;
  the spike's broad Option-returning helper is not the production specification. Test exec rejection and loss during await.
- [ ] Match concurrent session await versus close (T07/T14): compare returned/cancelled/failed as normal keys.
  Closing cancels Kotlin's reader/exit jobs after join; Rust's task completion can win that race. Preserve this
  lifecycle contract, including cancellation of readers still delivering buffered output; R1 alone does not establish parity.
- [ ] Cancellation of an await does not kill the remote process or invalidate the shared executor (T22); explicitly close
  sessions during lifecycle cleanup. Keep cancellation distinct from command timeout and consumer failure.
- [ ] Use independent readers of up to 16 KiB, per-read lossy UTF-8 decoding, cross-read CRLF state and EOF drain. Both
  streams reset one idle clock, with the 20 ms poll slack. Test asymmetric output and a stalled consumer; matching packet
  decoder output alone does not certify equivalence. Translate `idle timeout PT1S reached` with jcompat formatting.
- [ ] Implement outstanding keep-alive request promises and whole-second truncation; channel traffic must not clear missed
  replies (T25). Test 1500/999/0 ms and completion after server resume, and reproduce loss text/timing (R7).
- [ ] Configure SSH logging at ERROR normally and DEBUG for `ssh.debug`; preserve the callers' WriteFile debug guard/reroute so content
  is not exposed through the command. Keep russh trace logging from bypassing that guard.
- [ ] Retain T20/system-rsync feasibility coverage in 0.4a/4.3; keep fixture credentials and fake host keys out of user state.

### Incidental bugs

| # | Where | Behaviour | Seen in | Our handling | Status |
|---|---|---|---|---|---|
| R1 | russh 0.64.1 client close | Drops a peer CLOSE after local close, leaving the consumer waiting | T02/T07/T14 and crate tests | Narrow local-close tracking patch, callback preserved; not reported upstream | patched-locally |
| R2 | *withdrawn: Kotlin application bug A13* | The stale TERM comes from marathon’s null-result branch, not sshj; following close is a no-op | T16 | Approved D7 suppresses it; tracked in known-kotlin-bugs.md, not an incidental library defect | withdrawn |
| H1 | WP 0.3a fake `server.rs` | TERM's child-exit task could send CLOSE before the withholding handler, making T15 race and sometimes return in 9 ms | Round-1 rerun | Withhold child CLOSE too in hang mode; disable keep-alive in T15/T15h to isolate close/join; not an upstream issue | patched-locally |
| H2 | Spike comparator/runners | Failed closes, skips and incomplete runs could look equal; T07/T14 await outcomes were hidden; metadata filtering also matched values | F4, R2-sol-2/3 and comparison regressions | Compare concurrent session await outcomes and anchor metadata filtering to keys; round 1 alone was only a partial repair | patched-locally |
| H3 | WP 0.3a disconnect classifier | A generic Disconnect wrapper could hide a non-transient wire reason underneath | T26 code review | Inspect explicit peer reason before generic errors and clear it on every retry; not an upstream issue | patched-locally |
| H4 | Spike build-patched.sh | Path patches rewrote the tracked Cargo.lock and ignored CARGO_TARGET_DIR | R2-sol-5 | Build scratch package copies and use the supplied target-directory base for both variants | patched-locally |
| H5 | Spike exec.rs reader drain | Local close discarded buffered partial output | R2-sol-1, T14.timeout_partial | Drain buffered tails independently of the close flag and channel pump | patched-locally |

### Additional measured rows and backlog evidence

| Row / target | sshj | R1-patched russh spike | Contract / remaining work |
|---|---|---|---|
| T09 C, 2 s × 2 | Loss 5934 ms after pause; `Did not receive any keep-alive response for 4 seconds` | 5976 ms; `KeepaliveTimeout` | Map error text; silence timing matches within scheduling slack |
| T10 C | 1500 ms → 1 s, loss 2950 ms; 999/0 ms → off, both complete exit 0 after resume | 2979 ms / same off and completion | Now checks completion after resume, not just no loss |
| T15 fake, keep-alive off | First close throws after 30007 ms | 30001 ms | 30 s close wait; no claim that first close hangs forever |
| T15h fake, keep-alive off | Command timeout at 1 s then close failure at 31013 ms; later close still waiting after 60 s | Await fails at 31004 ms; later close still waiting after 60 s | Unbounded join is a separate path; lack of return is measured to the cap, not proof of infinity |
| T15t C, ignores TERM | Null exit after 8005 ms, close then returns; next command works | 8004 ms, same outcomes | Timeout does not return immediately; waits for peer CLOSE |
| T21 deterministic read boundaries | Split UTF-8 bytes `e2` / `82 ac 0a` → `[���]`; `left\r` / `\nright\n` → `[left, right]` | Same | Direct LineBuffer probes isolate read-boundary semantics |
| T21 C streaming | 100000 UTF-8 lines, 25 corrupted; CRLF 100000 ordered, no empty lines; stderr-only activity keeps stdout alive | 100000 lines, 26 corrupted; same CRLF/shared-idle results | Actual transport corruption differs (R6); line-count match is not byte parity |
| T21 C stalled consumer | 1000001 lines after consumer resumes, producer blocked 5 s | Same count, blocked 6 s | Backpressure observed; independent-reader design still required |
| T22 C cancellation/snapshot | Await cancelled at 1 s; `sleep 3.3` still alive 300 ms later; close later returns, next command works; completed session `isAlive=true` | Same | A11 confirmed; cancellation must not kill remote execution |
| T23 fake exec rejection | `ConnectionException[UNKNOWN](Request failed)`; safeExecute null; connected true | `ExecRejected("exec request failed")`; null; connected true | Normalize exception/message in WP 4.2 |
| T24 fake disconnect on exec | `ConnectionException[UNKNOWN](spike disconnect)` caused by `TransportException[BY_APPLICATION](spike disconnect)`; connected false | ConnectionLost with wire reason ByApplication; connected false | Keep cause/reason for taxonomy |
| T25 fake, replies withheld, output flowing | Connection lost at about 3 s, `Did not receive any keep-alive response for 2 seconds` | Reaches 12 s command timeout with null, still connected | R7 mismatch: implement unanswered promises independent of incoming traffic |
| T26 fake no-common-KEX | `TransportException[UNKNOWN]`, transient, four attempts | `NoCommonAlgo`, mapped transient, four attempts | A16 contradicts Kotlin KDoc; keep measured behavior |
| T26 fake before KEX | Reasons 12/2/11 → transient true/false/true, four attempts each | Same with explicit wire-reason capture | Generic Disconnect must not override PROTOCOL_ERROR |
| T26 fake after auth | Reasons 12/2/11 all produce `IllegalStateException(Not connected)`, non-transient, one attempt | NotConnected, non-transient, one attempt | A17; distinguish pre-exec state from in-flight transport errors |

**T17 detailed matching:** both accept plain, hashed and comma-separated host lists; both reject a wrong key, unknown host,
an entry with only another algorithm, a missing file and uppercase `[LOCALHOST]:port` when connecting to lowercase
`localhost`. sshj accepts the matching key with a differing same-algorithm key either before or after it; russh rejects
with `The server key changed at line 1` / `line 2`. sshj accepts tabs/multiple spaces, `[127.0.0.?]:port`, `*` and the
literal list `![127.0.0.1]:port,*`; russh rejects each with `Unknown server key`. In particular, do not infer OpenSSH
negation behavior for that list: sshj accepted it in this run. The malformed-line-first error is exactly
`connect <- Base64 decoding error: invalid symbol at 4 <- invalid symbol at 4` in russh; sshj skips that line and connects.
Rejections are four attempts and non-transient. These are explicit R3 implementation cases.

**Logging:** `PerformanceDefaultConfig`'s actual logger reports DEBUG/INFO/WARN disabled and ERROR enabled by default;
with `ssh.debug`, all four are enabled. T18 produces zero SSH log/content lines by default and 121 log lines, including
one `aGVsbG8K` content line, with ssh.debug. This deliberately invokes WriteFile directly to demonstrate why callers must
route to rsync when ssh.debug is enabled; WriteFile itself has no debug guard. Stock russh emits 0/118/513 log lines at
warn/debug/trace, respectively, and zero literal base64-content lines in these probes. That absence is not a secrecy
contract for all trace output; WP 4.2 must control the log routing explicitly.

**Round-1 rerun totals (superseded for T07/T14 by round 2 below):** container: **19 same, 2 differ, 0 skipped, 0 missing** across
T01–T14, T15t, T16–T19, T21–T22. Differences are T16 (D7) and T17 (R3). Fake: **5 same, 1 differ, 0 skipped, 0 missing**
across T15, T15h, T23–T26; difference T25 (R7). Both comparator commands exit **1**, as required for those differences.
R6's unequal corruption counts and R5's raw error strings are excluded by the declared normalization, remain documented
above, and are not declared full parity. Remote-mac rerun/count: **pending remote**, not skipped or same. T20 remains the historical
separate feasibility test, not part of these counts.

## Round 2 measurements and corrections

Measured on the container on 2026-10-23 with rebuilt harnesses. T07 and T14 now start the same concurrent session await
(streams **and** exit), then close the session. The normal keys `T07.close_await` and `T14.flood_await` compare
`returned`, `cancelled` or `failed`; only line counts, timings and raw errors remain metadata. The comparator checks
`.info.` in the key before the first colon, never in a result value.

- **T14.timeout_partial / R6:** both return `exit=null stdout=[partial] stderr=[]` for
  `sh -c 'printf partial; sleep 3'` with a 1 s timeout. The Rust drain now preserves the buffered partial line on local close.
- **T07:** close returns after about 4002–4006 ms in Rust and 4003 ms in Kotlin. Kotlin's concurrent await is
  `cancelled`, raw `JobCancellationException(Job was cancelled)`; Rust's is `returned`, null exit, zero output lines.
  This is a lifecycle parity gap, not an R1 crate-patch failure.
- **T14 flood:** two Kotlin runs report `still waiting after 45 s` (45006/45002 ms) for close, followed by an await
  cancellation with the same raw exception as T07. The cap describes the observation, not an infinite hang.
  Rust close returns in 11–52 ms; await was `cancelled` once and `returned` on two later runs (365966 and 359611 stdout
  lines, null exit). The final run returns in 13 ms with 359611 lines. These observations supersede the old blanket
  “patched: match” claim. The new Kotlin close delay's cause is unresolved; WP 4.2 must investigate close/join and
  reader/exit cancellation ordering with concurrent output. Neither scheduling variation nor line counts justify
  filtering out the await outcome. Both clients' subsequent `echo hello` succeeds.
- **Regression probes:** Rust T02 ordered/concurrent output and consumer failure, T21 UTF-8/CRLF/shared idle/backpressure,
  and T22 await cancellation/isAlive complete. The patched russh client tests remain 27/27 passing.

The saved container matrix combines these targeted round-2 reruns with unchanged round-1 rows: **17 same, 4 differ,
0 skipped, 0 missing**, exit **1**. Differences are T07/T14 (close/await lifecycle), T16 (approved D7) and T17 (R3).
This is not a new full-matrix run. Fake rows were not rerun in round 2; their prior 5 same/1 differ count remains historical.
No new deviation is requested: the close/await gap remains required parity work in WP 4.2.

The patched-build helper was run both with its default output base and with `CARGO_TARGET_DIR=target/r2-override`.
Both client and fake-worker variants use that supplied base; tracked `Cargo.lock` stays byte-identical. Cargo resolves
path patches only in scratch package copies, which are removed on exit.
