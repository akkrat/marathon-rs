# Scripted ADB server and its device script

`marathon-adb` (feature `server`, module `marathon_adb::server`) contains an ADB *server* that plays fake Android devices. It replaces adam's `server-stub` in our tests and is the Android side of the differential harness (PLAN.md §6.4). Two ways to run it:

- **In process**, from Rust tests: `FakeAdbServer::start(script, ServerOptions::default())` on a tokio runtime. The handle gives the port, the transcript, the device filesystems and reverse forwards, and calls that connect, disconnect or change the state of devices, or kill the server.
- **As a process**, for the Kotlin marathon (`adbServers`) and the real `adb` client:

  ```sh
  cargo run -p fake-adb -- --script devices.yaml --port 5037 --transcript transcript.jsonl
  ```

  `--port 0` picks a free port; `--port-file <file>` writes the chosen port once the server listens. The process runs until SIGINT or SIGTERM, or until `adb kill-server` when `server.on_kill` is `exit`. Before exiting it flushes the transcript; a transcript write error gives exit status 1. `--strict` turns on strict mode (below) and then also gives exit status 1 if the transcript holds any warning.

Unlike adam's stub, which replays a fixed sequence of expected requests per connection, the server keeps state: a device list that `host:track-devices` follows, an in-memory filesystem behind sync, reverse forwards, and shell expectations matched against every command.

## Script format

A script is YAML; JSON is valid YAML and works too. Unknown keys are errors. Relative file paths resolve against the script's directory, and every referenced file is read when the script loads, so a missing file fails at start-up.

```yaml
server:
  adb_version: 41            # host:version (the real client restarts a server with another version)
  host_features: [shell_v2, cmd]   # host:host-features; a recent platform-tools list by default
  on_kill: exit              # host:kill: exit (stop like the real server) or ignore
  builtins: true             # built-in getprop and echo for unmatched shell commands
  strict: false              # unmatched commands get FAIL; fake-adb exits 1 if any warning was recorded

devices:
  - serial: emulator-5554
    state: device            # any adb state: device, offline, unauthorized, recovery…
    connected: true          # false: the device appears only through a connect event
    features: [shell_v2, cmd, stat_v2, ls_v2, abb_exec, sendrecv_v2]   # see "Features" below
    getprop:                 # `getprop` and `getprop <key>`; values may be unquoted numbers
      ro.build.version.sdk: 33
      ro.product.model: Pixel 7
    files:                   # the initial filesystem; parent directories are created
      - { path: /sdcard/config.json, content: "{}", mode: "0644", mtime: 1700000000 }
      - { path: /data/local/tmp/fixture.bin, file: fixture.bin }
      - { path: /sdcard/empty-dir, dir: true }
    framebuffer: { version: 2, width: 1080, height: 1920, fill: [0, 0, 0, 255] }
    shell:                   # this device's expectations, tried before the shared ones
      - { exact: "pm list packages", stdout: "package:com.example\n" }

shell:                       # expectations shared by all devices
  - prefix: "am instrument"
    stdout_file: instrumentation/pass-fail-ignored.txt
    chunk_size: 512
    chunk_interval_ms: 5
  - { prefix: "logcat", stdout_file: logcat.txt, hang: true }
  - { regex: "^rm -(r|f)+ ", stdout: "" }

default:                     # the answer to an unmatched command (a warning is also recorded)
  stdout: ""
  stderr: ""
  exit_code: 0

events:                      # a timeline, in milliseconds after start-up
  - { at_ms: 2000, connect: emulator-5556 }
  - { at_ms: 5000, state: { serial: emulator-5556, state: offline } }
  - { at_ms: 6000, disconnect: emulator-5554 }
  - { at_ms: 9000, kill_server: { restart_after_ms: 500 } }
```

### Shell expectations

One expectation answers `shell:` (v1), `shell,v2,…:` (v2), `exec:` and `abb_exec:` commands. The fields:

| Field | Meaning |
|---|---|
| `exact` / `prefix` / `regex` | Exactly one: how the command is matched. `regex` uses the Rust `regex` crate (this is test tooling, not user configuration, so `JavaRegex` doesn't apply) |
| `service` | Only `shell`, `exec` or `abb_exec`; any by default |
| `times` | Stops matching after this many uses, so a later expectation for the same command takes over |
| `delay_ms` | Waits before the first output |
| `stdout`, `stdout_file`, `stderr` | Output, written in that order. `stdout_file` streams a file in `chunk_size`-byte writes `chunk_interval_ms` apart (the whole file at once by default) |
| `output` | Instead of the three above: a list of steps `{stdout: …}`, `{stderr: …}`, `{stdout_file: path}` or `{stdout_file: {path, chunk_size, interval_ms}}`, `{sleep_ms: …}`, and `{gate: name}`, which waits until the in-process test calls `FakeAdbServer::open_gate(name)` (a standalone server never opens one) |
| `exit_code` | 0–255 |
| `hang` | After the output, keep the command running until the client closes the connection (logcat, a stuck instrumentation) |

The command that is matched:
- `shell:<cmd>;echo x$?`, adam's v1 way of getting the exit code, is matched as `<cmd>`; the server then appends `x<exit code>\n` after the output, as the device shell would.
- `abb_exec:` arguments are separated by NUL on the wire and joined by spaces for matching (`package install -S 123`).
- The real `adb` client quotes some arguments, for example `exec:cmd package 'install' -S 123`, `exec:screencap '-p'` and `shell:rm '/data/local/tmp/app.apk' </dev/null`. Expectations see the command exactly as sent.

Device expectations are tried before the shared `shell` list, each in order; the first match wins. Without a match the server tries the built-ins (shell service only, when `server.builtins` is on): `getprop` (all properties as `[key]: [value]`, sorted), `getprop <key>`, and `echo <text>`. Otherwise it answers with `default` and records a warning. In strict mode (`server.strict: true` or `fake-adb --strict`) it instead refuses the stream with `FAIL fake adb: no expectation for <service> command '<cmd>'`, before OKAY, so the client sees an error; the warning is recorded either way. Differential runs use strict mode, so that a mistyped rule can't pass unnoticed. An expectation is reserved when the command arrives, before OKAY: two concurrent sessions can't both use an expectation limited by `times`, and in strict mode the second one is refused.

Without the shell protocol (v1 shell and exec), stderr goes down the same stream as stdout, as adbd does.

### Installs

Installs are scripted like any other command, but the server reads the streamed APK before it answers: for `exec:` and `abb_exec:` commands of the form `cmd package install … -S <n>`, `pm install-write -S <n> …` (also `-S<n>`, quoted, or through `abb_exec`), it consumes exactly `<n>` bytes of stdin and records the count as `stdin_bytes`. If the client's stdin ends early, the server answers `Failure [INSTALL_FAILED_INVALID_APK: fake adb received <k> of <n> bytes]` with exit code 1, records the command with handler `short_payload` (`stdin_bytes` is what arrived, `stdin_expected` the declared size) plus a warning, and gives the reserved expectation back. If the upload fails with a read error (a reset) instead, nothing can be answered; the record and the warning are the same, with the error added. A reservation also comes back in that case and when the session is dropped (device disconnect, server kill). `install-create -S <total>` carries no payload. The legacy path (sync push, then `pm install <path>` over shell) needs nothing special.

### Features

A device without `features` reports a recent emulator's list minus the compressed sync variants: `shell_v2, cmd, stat_v2, ls_v2, fixed_push_mkdir, apex, abb, fixed_push_symlink_timestamp, abb_exec, remount_shell, track_app, sendrecv_v2`. The features decide what clients use: `shell_v2` for shell v2, `stat_v2`/`ls_v2`/`sendrecv_v2` for sync v2, `abb_exec` and `cmd` for streamed installs. `features: []` gives a device on which `adb install` falls back to push and `pm install`.

### Files and sync

Each device has an in-memory filesystem with `/`, `/data/local/tmp`, `/sdcard` and `/system/bin`, plus the script's `files`. `mode` is an octal string (`"0644"` by default, `"0755"` for directories; a scripted directory keeps its mode, the parents created for it get `0755`); `mtime` is in seconds. Pushes create parent directories, like adbd. `/sdcard` is a plain directory, not a link to `/storage/emulated/0`.

### Framebuffer

`framebuffer:` answers with the version word, then:
- `file`: the file's bytes unchanged (header and pixels). This is the format of adam's replay files, such as `fixtures/vendor-android/framebuffer/screencap_1.bin`;
- `pixels` (raw RGBA_8888, `width × height × 4` bytes) or `fill` (one RGBA colour): a generated RGBA_8888 header for `version` 1 or 2 (`color_space` for v2), then the pixels.

Without a `framebuffer` the server closes the connection after OKAY, which is what adbd does when screencap fails.

### Events

| Event | Effect |
|---|---|
| `connect: <serial>` | The device (declared in `devices`) joins the list with a new transport id |
| `disconnect: <serial>` | The device leaves the list; its open sessions are closed and its reverse forwards dropped |
| `state: {serial, state}` | Changes the state; leaving `device` closes the open sessions |
| `kill_server: {restart_after_ms}` | Drops every connection and stops listening, as if the server died; listens again on the same port after `restart_after_ms`, with the same devices. Without it, the server stays down |

## Protocol coverage

- **Host services:** `host:version`, `host:devices`, `host:devices-l`, `host:track-devices` (and `-l`), `host:host-features`, `host:kill`; `host:transport:<serial>`, `host:transport-any`, `-usb`, `-local`, `host:transport-id:<n>` and the newer `host:tport:serial:<serial>` / `any` / `usb` / `local` (OKAY, then the 8-byte transport id); `features`, `get-state`, `get-serialno` and `get-devpath` under `host:`, `host-serial:<serial>:`, `host-usb:`, `host-local:` and `host-transport-id:<n>:`. A serial may contain colons (`127.0.0.1:5555`). Error messages follow adb (`device 'x' not found`, `more than one device/emulator`, `device offline`, …). A device service sent without a transport switch goes to the only device.
- **Device services:** `shell:` (v1), `shell,v2,raw:` and other `shell,<args>:` forms (the shell protocol with stdout, stderr and exit packets; stdin and close-stdin packets are read and dropped), `exec:`, `abb_exec:`, `sync:`, `framebuffer:`, `reverse:forward:[norebind:]<local>;<remote>` (`tcp:0` answers with the allocated port), `reverse:killforward:<local>`, `reverse:killforward-all`, `reverse:list-forward`. Other services fail with `closed`, as adbd's refusal looks to a client.
- **Sync:** v1 `STAT`, `LIST`/`DENT`, `SEND` (`<path>,<decimal mode>`), `RECV`, `DATA`, `DONE`, `OKAY`, `FAIL`, `QUIT`; v2 `STA2`, `LST2`, `LIS2`/`DNT2`, `SND2` (with the mode and flags block; the dry-run flag is honoured), `RCV2`. The compression flags (brotli, LZ4, zstd) get a FAIL; the default features don't advertise them.

## Transcript

Every record is a JSON object on its own line (`--transcript`), and the in-process handle returns the same list. Types:

| `type` | Fields |
|---|---|
| `request` | `conn`, `serial` (after a transport switch, else null), `service`: every request string |
| `shell` | `conn`, `serial`, `service` (`shell`, `exec`, `abb_exec`), `protocol` (`v2` or `raw`), `command`, `exit_code_trick`, `stdin_bytes`, `stdin_expected` (short uploads), `handler` (`script`, `builtin`, `default`, `short_payload`, or `rejected` in strict mode), `rule` (such as `devices[emulator-5554].shell[2]` or `shell[0]`), `exit_code`, `hang` |
| `sync` | `conn`, `serial`, `op` (`STAT`, `LST2`, `SEND`, `RCV2`, …), `path`, and per op `exists`, `entries`, `size`, `mode` (octal), `flags`, `mtime`, `error` |
| `framebuffer` | `conn`, `serial`, `size` |
| `event` | `event` (`connect`, `disconnect`, `state`, `add`, `kill`, `session_dropped`, `client_closed` (the client stopped listening to a command's output or a `track-devices` stream; not a warning), `server_killed` (a kill-server event, with `restart_after_ms`), `server_restarted`, `server_stopped` (the final record)) and its details |
| `warning` | `message`: an unmatched command, an unknown service, a short install payload, a connection error other than `client_closed`, such as a reset during an upload or a sync transfer |

The file is written by a separate thread. `FakeAdbServer::flush_transcript` waits until every record so far is in the file and reports the first write error. `FakeAdbServer::shutdown`, which `fake-adb` runs on kill-server, SIGINT and SIGTERM, first stops the timeline and every connection and waits for them, records `server_stopped`, then drains and closes the file, so the file is complete and ends with that record, also when the server was down waiting for a restart. A `kill_server` event records `server_killed` instead; `server_stopped` appears exactly once, last.

`conn` numbers connections in accept order. Concurrent sessions interleave, so a differential comparison should group records by serial and connection rather than compare the raw order.

## Known limits

- One fake filesystem per device; no permissions checks, symlinks or `/storage/emulated/0`.
- Shell commands don't touch the filesystem (`rm`, `mkdir` and the like are scripted, not executed).
- A hanging command ends when the client closes its socket. A client that only half-closes (the real adb client does so when its stdin ends on a v1 shell) also ends it.
- Compressed sync v2 is not implemented.
- No `host:forward`, `jdwp`, `track-jdwp`, `abb:` (abb with the shell protocol), `remount`, `root` or `reboot`.
