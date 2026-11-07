# Pinned russh for the fake worker

Source: crates.io `russh` **0.64.1**, archive SHA-256
`ba61e87b9ec9a39a59a6bbed4c0b8ff7fb07073405b24a767a1f2c8560ac8f58`.
Upstream: <https://github.com/warp-tech/russh>. License: Apache-2.0 (`LICENSE`).
Registry build metadata and the registry lockfile are not vendored. An empty
`[workspace]` keeps direct crate tests independent of ancestor workspaces.
The root workspace pins `=0.64.1` and patches crates.io to this directory.

Applied changes, in order:

1. `spikes/ssh-russh/russh-close-after-local-close.patch` (R1/U1), unchanged.
   Its pending-open/duplicate/callback and live client regression tests remain.
2. `tools/fake-worker/russh-server-hooks.patch`:
   - asynchronous `reply_to_channel_close` hook, default true; a false result
     suppresses only the reply, and still releases channel state/calls the handler;
   - asynchronous `unknown_global_request` hook, default immediate failure;
     delayed failures are timer-polled by the session, capped at 128, and may be
     withheld without blocking channel data;
   - `RunningSession::abort` using an owned futures abort registration. The harness
     awaits aborted sessions; upstream's default detach-on-drop semantics stay;
   - release the server channel sender on server-initiated CLOSE (previously its
     peer reply was ignored after the channel parameters had been removed);
   - initialize the delayed queue in upstream test sessions and retain a sender
     lifetime regression.

3. `crates/marathon-apple/russh-manual-window.patch` (WP 4.2, flow control):
   - client `Config::manual_window_adjust`, default false. When set, received
     channel data only shrinks the receive window instead of refilling it at once;
   - `Handle::adjust_window(channel, bytes)` sends `CHANNEL_WINDOW_ADJUST`. The
     production SSH executor grows the window as its readers consume a channel,
     as sshj's `ChannelInputStream` does, so one stalled consumer blocks only its
     own channel and never the session loop;
   - two client tests (manual window, default unchanged).

4. `crates/marathon-apple/russh-open-failure-description.patch` (WP x38, U157):
   - client `Handle::channel_open_session_described()`. A refused open returns the
     new `Error::ChannelOpenRefused { reason, description }`, with the description
     the peer sent for that channel. `Handler::channel_open_failure` gets the text
     too, but nothing there ties it to the opener, so concurrent or abandoned
     refusals could take each other's text;
   - the session sends the new `ChannelMsg::OpenFailureDescription` to the opener
     just before `ChannelMsg::OpenFailure`. `channel_open_session` and the other
     openers skip it, so they still return `Error::ChannelOpenFailure(reason)`;
   - one client test (concurrent refusals each get their own text; the plain open
     is unchanged). The test server can refuse a list of session opens.

Each file these changes touch starts with a `Modified by marathon-rs` line (Apache-2.0 section 4(b)):
`Cargo.toml`, `src/channels/mod.rs`, `src/client/{encrypted,mod,session,test}.rs`, `src/helpers.rs`,
`src/keys/{known_hosts,mod}.rs`, `src/lib_inner.rs`, `src/pty.rs`, `src/server/{encrypted,mod,session}.rs` and
`src/session.rs`. The upstream archive holds no NOTICE file.

No process-wide environment flag changes protocol behavior. All default hooks
retain the upstream wire behavior. These patches have not been submitted upstream.

Validation (from the repository root):

```sh
CARGO_TARGET_DIR="$PWD/target/vendor-tests" cargo test --manifest-path vendor/russh/Cargo.toml --lib client::
CARGO_TARGET_DIR="$PWD/target/vendor-tests" cargo test --manifest-path vendor/russh/Cargo.toml --lib server_close_releases_channel_sender
cargo test -p fake-worker
```

The original R1 suite contains 27 client tests; with patch 3 the `client::` filter runs 29, and with patch 4, 30.
`scripts/check.sh` runs them. The harness tests exercise CLOSE
withholding, output during suppressed keep-alives, delayed reply interoperability,
wire disconnect reasons, and owned shutdown against this same patched dependency.

Upstream inline key/hash test strings are extracted byte-for-byte into
`fixtures/fake-worker/russh` and loaded with `include_str!`/`include_bytes!`,
which keeps the base64 test data out of the Rust sources. PTY opcode 4's Rust
variant is named `KillLine`; its wire number is unchanged. No upstream protocol
behavior is changed by these adaptations. The inline source strings are public,
published upstream test keys, not credentials for any host.
