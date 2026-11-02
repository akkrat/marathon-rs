# Fake SSH worker contract (WP 0.4a)

`tools/fake-worker` is reusable test infrastructure. It does not implement the production SSH executor or the Apple tool world. Kotlin at `e47352f2`, OpenSSH, and the R1-patched russh client all drive the same transport and disk. WP 0.4b supplies Apple handlers and media; WP 4.2 still owns client parity, including R3–R7. No Kotlin parity rows move to `ported` here.

## Start and stop

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build -p fake-worker
cargo test -p fake-worker
# A disposable reference build must already exist (or run tools/oracle/build.sh).
tools/fake-worker/validation/build-kotlin.sh
export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
tools/fake-worker/validation/run.py --output tools/fake-worker/runs/new-run
```

The runner creates fixture keys, config, a private backing root and transcripts; it runs the actual Kotlin boundary methods, not copies of them. A nonzero result is significant: it includes U4 failures, missing prerequisites and timeouts. `--only standard` and `--only faults` split the matrix. `--skip-long` is for development and records a **skipped** result, never acceptance. Use a new output directory each time. `--worker`, `--rsync`, `--sshpass` and `--java` select explicit platform binaries.

The standalone interface is `fake-worker --config CONFIG.json`. Configuration:

```json
{
  "listen": "127.0.0.1:0",
  "root": "/absolute/private/run/worker-root",
  "transcript": "/absolute/private/run/transcript.jsonl",
  "host_key": "/absolute/private/run/fixture-host-key",
  "auth": {
    "user": "fixture",
    "password": "fake-password",
    "authorized_keys": ["/absolute/private/run/fixture-key.pub"],
    "password_auth": true,
    "keyboard_interactive": true
  },
  "max_sessions": 4,
  "rsync_binary": "/usr/bin/rsync",
  "rsync_remote": "/usr/bin/rsync",
  "fixtures": "/absolute/checkout/fixtures/fake-worker",
  "run": "example",
  "script": {
    "unmatched_status": 127,
    "rules": [{"name":"probe", "matcher":{"exact":"probe"},
      "action":{"type":"output", "chunks":[{"data":"hello\n"}], "status":0}}]
  }
}
```

The root must **not exist**. Its parent and the transcript parent must exist. The transcript uses create-new and mode 0600; the root uses mode 0700. The listener rejects non-loopback addresses. `password: null` disables password acceptance; unknown users always fail. Authorized key files contain plain OpenSSH public-key lines, not `authorized_keys` options. Keys and config are fixture-only and are not committed.

Readiness is one JSON line on stdout, after binding and opening evidence:

```json
{"schema":1,"endpoint":"127.0.0.1:54321","host_key_fingerprint":"SHA256:...","run":"example"}
```

The binary handles SIGINT/SIGTERM by awaiting shutdown. Library callers use `start(config, extensions).await`, `worker.ready`, `worker.fs`, and `worker.shutdown().await`. Dropping the handle initiates cancellation; only awaiting `shutdown` establishes that transports, actors, background jobs and children have been joined. The private filesystem and evidence remain for inspection.

## Router and extensions

`Request` retains original `raw: Vec<u8>`, UTF-8 `text`, and parsed `argv`. Raw bytes are recorded **before** parsing. UTF-8 and shell-quote errors produce status 2; A12's `echo 'it\'s'` is not repaired. The parser handles quoting, not general shell evaluation. It does not expand environment variables, substitutions, wildcards or pipelines. No unmapped command is passed to a host shell. Env, PTY, shell and subsystem requests fail explicitly; forwarding is refused. The harness never invents Kotlin SSH env or workdir.

Routing order is script rules, registered extension handlers, built-in file/rsync wrappers, then unmatched status (forced nonzero, with a warning). Matchers are `{"exact":"wire string"}`, `{"prefix":"prefix"}`, or `{"pattern":"byte*glob?"}`. The last is a deliberately small test DSL, **not** a Marathonfile regex; it does not use or replace the merged D6 `JavaRegex` wrapper. First match wins. Patterns match original wire strings; absolute executables such as `/usr/libexec/PlistBuddy` require no transport changes.

The runnable `examples/extension.rs` demonstrates this seam (`cargo run -p fake-worker --example extension -- CONFIG.json`).

A library extension implements `router::Handler`:

```rust,ignore
fn name(&self) -> &str;
fn route<'a>(&'a self, request: &'a Request, fs: &'a VirtualFs)
    -> HandlerFuture<'a>; // Send future -> Result<Option<Action>>
```

Return `None` to continue routing. A handler may inspect/mutate the shared filesystem and return a scripted action, including fixture streams. WP 0.4b registers one or more handlers for every tool in PLAN §6.4; media generation belongs there. Routing runs in a tracked channel actor, not inside the SSH packet callback, so waiting on a filesystem lease does not stop other channels' transport traffic. Both initial routing and nested `nohup` routing drop a pending extension future on channel CLOSE, connection loss, or run cancellation. `shutdown().await` waits for the tracked actors; a handler that remains pending cannot prevent shutdown. Once nested routing completes and registers its background job, that job deliberately outlives the channel and remains owned by the connection/run. Extension futures must therefore be cancellation-safe when dropped at any await, must not spawn unowned jobs, and must not use blocking I/O on the executor. A filesystem operation already submitted to the blocking pool may finish while holding its lease; it cannot escape containment, and shutdown waits for the final filesystem snapshot.

`Action` is a tagged JSON object (`type`):

- `output`: `chunks`, `status` (default 0), optional `signal`, `hang`, `ignore_signals`, `eof_before_exit`, `close_before_eof`, `withhold_close`. Each chunk has `data` (UTF-8), optional `base64` (takes precedence), or `fixture` (streamed in 32 KiB blocks); `delay_ms` is relative to the preceding chunk; `eof_before` injects EOF before that chunk (including deliberately late output after EOF); `extended` selects an SSH extended-data type, including invalid types such as 42. No extended type means stdout; 1 means stderr. Fixture paths are relative to `fixtures` and cannot escape via traversal or symlinks.
- `echo_stdin`: lossless stdin echo until client EOF.
- `filesystem`: explicitly route this expectation to a built-in filesystem command.
- `reject`: send SSH request failure, distinct from a command with a nonzero status.
- `disconnect`: `reason` (wire number), `message`; disconnect during exec.
- `drop_transport`: abort the transport without a disconnect message.

The built-ins recognize `mkdir -p`, `rm -rf`/`-R`/`-f`, `cp -R`, `mv -f`, `ln -s`, `test -e`/`-f`, and RemoteFileManager's bounded-depth `find` with zero or more `! -name` exclusions and `-exec rm -rf {} +`. Exclusion names use the same glob matcher. Unsupported syntax fails without a shell. A simple `sh -c` may wrap a built-in; arbitrary shell programs must be mapped by their full raw expectation.

### WriteFile and background PIDs

The exact WriteFile `umask 077 && echo … | base64 --decode > … && [ $(wc -c < …) -eq N ] && mv -f … || { rm -f …; exit 1; }` form is recognized structurally and checked against its reconstructed string. It writes decoded bytes next to the destination, checks size, sets mode 0600 and renames. A failed write preserves the old destination and removes a temporary regular file; a pre-existing temporary directory remains, just as `rm -f` does. The 32 KiB/plain-path guard and `ssh.debug` fallback belong to the client, and the Kotlin runner invokes them in the actual code.

The exact `sh -c 'nohup COMMAND > /dev/null 2>&1 & echo $! > PIDFILE'` wrapper routes `COMMAND` again, allocates a virtual PID starting at 1000, and writes `PIDFILE` through the same filesystem. The child command must select an `output` action. Output is redirected/discarded and recorded as `background_redirect`; chunk delays, hangs and ignored signals are honored. Background jobs outlive the launching channel and end on signal or connection shutdown. They do **not** invoke host `nohup`, `sh`, or `kill`.

`kill -s SIGNAL PID` routes to the virtual process table. The exact Apple recording-stop wrapper, `sh -c 'kill -s INT $(cat PIDFILE) && lsof -p $(cat PIDFILE) +r 1 &>/dev/null'`, resolves its PID file and waits for that virtual job to finish. `processes::Processes` documents the reusable registration/signal/completion primitive. No virtual PID may be sent to a host process API. 0.4b can prepare media through its handler and use these virtual lifetimes; it does not need another PID or path mapper.

## One filesystem

All connections share one `VirtualFs`. Only virtual `/tmp/marathon` and descendants map into the per-run backing directory. No operation touches the host's `/tmp/marathon`. Other absolute worker paths must be scripted. The virtual root is reserved: remove its children or use `reset`, rather than removing the backing directory itself.

The public async API provides `mkdir`, `read`, `write`, `write_file`, `stat`, `list`, `copy`, `rename`, `remove`, `symlink`, `snapshot`, `reset`, and `resolve_xctestrun_path`. Blocking filesystem work runs in `spawn_blocking`. Snapshots retain file bytes, modes and symlinks and support restoring an isolated test run; transcript snapshots contain metadata/hashes only. Directory traversal order is lexical. `reset` accepts a trusted snapshot captured from this API, not untrusted network data.

Paths outside the virtual prefix, NUL, and `..` are rejected. Reads resolve virtual symlink targets component by component inside the root, with a 40-link cycle bound. No host filesystem operation may follow a symlink below the root check. Every destination node is checked, including directory-operand basenames and every node visited by recursive copy, before creation, copying, or permission changes. Writes reject symlink leaves and ancestors, including symlink chains whose targets all stay inside the root. This is a deliberate fail-safe gap: those writes fail even where a real filesystem would allow them. Leaf-only removal/rename uses non-following operations after checking parents; paths are rebuilt from components so trailing slashes cannot turn a leaf link into a dereferenced directory. WriteFile may atomically replace a destination link, but never follows its temporary path. Reads resolve links in the virtual resolver, then pass the fully checked link-free path to host I/O. Symlink target bytes remain unchanged, including virtual absolute paths, so rsync preserves them on the wire. The virtual resolver maps absolute targets instead of asking the host kernel to follow them; real children reject symlink operands and do not use link-following flags. Relative links must remain in the root. Removing/renaming a source link acts on the link. Another deliberate fail-safe gap is `mv -f victim file-link`: the harness rejects a symlink destination where real `mv` would replace that link. WP 0.4b must account for these restrictions. Rsync's `--links` keeps transferred link bytes unchanged; unsafe links can be stored as inert nodes, but the resolver will not follow them.

Real rsync holds an exclusive filesystem lease until child exit, pipe drain and reaping. This prevents other harness operations from swapping a checked ancestor for a symlink during a transfer. The only real child program is the explicitly selected rsync, with a narrow option allowlist and **one** mapped operand after `.`. No `--copy-links`, `--keep-dirlinks`, arbitrary temp/log/config paths, or shell interpolation are allowed. Real-transfer regressions upload file/directory/partial-directory links and check subsequent copy, move, remove, find, WriteFile and rsync operations against an outside sentinel and its mode. Rsync replaces descendant links without following them under these allowed flags; a symlink operand itself is rejected before spawning. Receiver traversal protection additionally depends on the selected rsync implementation; this is a loopback fixture harness, not an OS sandbox for a malicious or compromised rsync binary. Existing checked root ownership and controlled fixture clients are part of its contract.

`resolve_xctestrun_path(value, test_root, test_host)` resolves an individual path field, including `__TESTROOT__` and optional `__TESTHOST__`, through the same resolver. 0.4b parses the plist and passes its path fields. Opaque plist bytes, environment strings and the original virtual paths are never blanket-replaced. Backing paths are an internal library result and never emitted on SSH stdout or substituted into raw transcripts.

## Rsync and host shims

macOS validation pins both ends to Apple's `/usr/bin/rsync` (openrsync, protocol 29). Linux uses the installed `/usr/bin/rsync` (Samba 3.4.1, protocol 32). The build box has no openrsync executable or package in its configured apt catalog; using its maintained Samba binary avoids introducing an unvalidated portable build on the critical path. This is the explicit Linux substitute decision. **Linux evidence does not certify macOS argv/protocol parity.** Both dialects are tested separately and their version output and binary SHA-256 hashes are saved.

The router accepts openrsync's separate `-g -l ... --partial-dir .rsync-partial ... --delete-before --dirs` options and Samba's compact server flags. Only the final remote path operand is mapped; directory trailing slashes remain. Stdout, stderr and stdin are real binary pipes. TERM/KILL/custom known Unix signals are delivered to the owned rsync PID; cancellation kills and awaits it. No shell is used to spawn or signal it.

The `rsync`, `ssh`, and `sshpass` shims record a single JSON array for original argv, then `exec` explicitly selected real binaries (`FW_REAL_RSYNC`, `FW_REAL_SSH`, `FW_REAL_SSHPASS`). `FW_ARGV_LOG` selects JSONL output. The runner sets `RSYNC4J_RSYNC`, `RSYNC4J_SSHPASS` and a controlled PATH. Recursing into shims is rejected. Original argv and harness-added options are separate fields. The ssh shim adds isolated user/global known_hosts, disables key updates and agent/key discovery. Every direct fake-worker OpenSSH invocation uses this shim too. The tests hash both user known_hosts files before and after success/failure runs.

The original `--rsh` string is asserted byte-for-byte, including quoting, `-F /dev/null`, key/password branches, `-l`/`-p`, the trailing space, 1500 ms → 1 second, 999 ms → disabled, and conditional `-vvv`. Debug rsync `--version /tmp /tmp` is retained as an event, not silently dropped. The source assigns `.links(true)` again for receive, but **the actual rsync4j builder deduplicates it**: the measured original argv contains one `--links`. The brief's expected repeated argv flag was not reproduced because it is not on the wire.

### U4 and the explicit feeder decision

The normal sshpass path is unchanged and is exercised 20 times per platform, plus wrong-password rejection. Every attempt has a deadline and a saved status/stderr; there are no retries. U4 remains open regardless of the pass count.

The Kotlin transfer scenario opts into `FW_USE_FIXTURE_ASKPASS=1`. After recording original argv, the ssh shim sets `SSH_ASKPASS_REQUIRE=force`, a fixture `DISPLAY`, and the checked-in `fixture-askpass` executable. This returns only the labelled fixture password and rejects other prompts. It avoids the sshpass PTY feeding path while still exercising all three shims and the actual RsyncFileBridge password branch. The override is recorded separately as `harness_env`. Ten independent successful-login attempts and a wrong-password attempt test its limits. It is a fixture workaround, **not a production authentication implementation or a claim that sshpass is fixed**. Binary stdin remains owned by rsync. The aggregate validation runner remains nonzero when any baseline U4 attempt fails.

## Faults and lifecycle handoff to WP 0.6

Server `script.faults` supports:

- `withhold_close`: suppress both command-completion CLOSE and russh's automatic reply to a client CLOSE. Per-rule `output.withhold_close` isolates a single channel; other channels and connections remain usable. This is a narrow server hook, **not** connection stalling. The real T15/T15h runner distinguishes the first approximately 30-second close failure from a subsequent join observed still waiting at 60 seconds. External deadlines are 45/100 seconds; shutdown remains bounded.
- `keepalive`: `"answer"`, `{"delay": milliseconds}`, or `"withhold"`. Replies are RFC-valid REQUEST_FAILURE packets. Timers are owned by the transport event loop and do not block output. Delayed requests are bounded at 128. The transcript records receipt and the selected reply policy; it is not proof of packet reception by the peer.
- `reject_exec`, `before_kex`, `after_auth`, `during_exec`, `no_common_kex`. Disconnect objects contain `reason` and `message`; reasons 12/2/11 are asserted at all stages. Before KEX, the clear disconnect is sent after reading the client banner and unread bytes are drained so TCP reset does not hide the wire reason. Post-auth Kotlin `Not connected` is preserved as A17, not relabeled an in-flight failure.

There are at most 64 accepted connections, `max_sessions` live session channels per connection, 16 queued input/channel messages and 32 transport events; output is window-aware and transferred in at most 32 KiB blocks. These are in-memory queue/flow-control bounds, not a transcript size limit: unbounded stdin through `echo_stdin` grows the on-disk transcript without limit (both stdin and echoed output are recorded). N+1 sessions receive `ResourceShortage` (SSH open-failure code 4). Slots are released **before** command completion becomes observable on the wire, as well as on client close/disconnect. Releasing after writing CLOSE creates a real N+1 race and is covered by tests.

The accept loop owns connection tasks; each wraps the russh running session with abort-on-drop. Connection cancellation propagates to all channel/background jobs. The run task tracker joins these jobs after connections stop. Real-child cancellation kills and reaps, including when stdout is blocked by flow control. Rust tests hold a real rsync child waiting for protocol input, check another session still progresses, shut down and assert the PID is gone. No unfinished WP 0.6 interface is imported. WP 0.6 may use this ownership contract as evidence, but this harness is not the production lifecycle implementation.

The stock OpenSSH CLI has no operation for sending an SSH channel `signal` request. Its matrix therefore uses a ControlMaster connection, starts a mapped background command and sends TERM through the worker's virtual `kill` command on another channel; it asserts connection reuse and signal delivery in the transcript. sshj and russh exercise **actual SSH signal requests**. This capability difference is explicit, not a fabricated OpenSSH channel-signal test.

## Evidence schema and normalization

Each server JSONL row has `schema:1`, `run`, monotonic `seq`, `connection` (0 for run events), nullable `channel`, `event`, and `detail`. Sequence allocation and append share one async writer lock. After acquiring it, an append transfers the owned lock and the whole record to the blocking pool, so cancellation cannot leave a partial JSONL record or a suspended future holding the lock. Subsequent events and `flush` wait for that append; write errors remain sticky and fail cleanup even if the original waiter was cancelled. `flush` joins submitted writes, without promising fsync durability. `exec`, `stdin` and `output` carry lossless `base64`, byte `size`, and `sha256`; output additionally identifies `extended`. The controlled transcript is the artifact containing WriteFile payloads, not an ordinary diagnostic log. Password attempts record method/user/result only; no password, key material or response hash is recorded.

Events include readiness, connections/end outcomes, auth/challenges, accepted/rejected sessions, request rejection, original exec, selected handler/unmatched warning, routing error, stdin/output, client/server EOF/CLOSE, signal/ignored signal, exit status/signal, keep-alive policy, wire disconnect stage/reason/message, child start/reaped, background PID/signal/end, filesystem entries, channel outcomes, and final cleanup snapshot. Output events mean bytes accepted into the bounded transport pipeline; client assertions establish received bytes. A failed connection remains `failed` even when intentionally induced. Final cleanup is an independent lifecycle outcome.

Host argv evidence is a separate JSONL stream (`event:host_argv`, `program`, `argv`, `harness_options`, `harness_env`). It is not assigned a fabricated total order relative to the server's stream. The schema-2 validation manifest declares the selected suite (`standard`, `faults`, or `all`), platform, expected case names and required artifacts from `validation/suite_plan.py` before any prerequisites run. It records each case's `completed`/`failed`/`timeout`/`skipped`, exit code and deadline outcome, and a run-level `terminal_outcome` initially set to `running`. Startup/prerequisite exceptions are failed results; terminal completion requires all expected cases and artifacts. Comparison validates both raw and normalized manifests against the declared suite inventory, requires exactly one completed result per expected case and every required artifact, and requires each worker transcript to end in completed cleanup. Interrupted prefixes, old manifests without a terminal outcome, missing/empty transcripts and failed/skipped cases cannot compare equal, even to themselves.

`validation/evidence.py` copies only transcripts, public readiness, version/results and client output into a curated raw evidence directory, and creates normalized copies. It excludes private keys and config. Normalization is field-specific: run directory prefixes in argv/error locations, ephemeral port arguments/endpoints, child PIDs and readiness host fingerprints are mapped consistently; opaque exec/stdin/output bytes and virtual paths are unchanged. Directory inode sizes are normalized away; file sizes/hashes/modes remain. Raw evidence is always retained alongside normalized copies. All stdout/stderr artifacts are copied byte for byte, including invalid UTF-8 and CRLF; no unstructured stream is implicitly treated as a text diagnostic. Only explicitly structured JSON fields are normalized. Diagnostic streams can retain additional fingerprints and platform-specific paths; normalization is not a promise of cross-run equality for those logs. Timing is excluded from comparisons but retained in raw manifests. Cross-platform logs and rsync wire streams are not declared equal: protocol differences are documented.
