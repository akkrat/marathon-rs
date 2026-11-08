# X44: remote parsing tries another device (D53, A103, U171)

The project owner approved D53 on 2026-11-08 ("fix for all platforms"). It fixes A103 on Android, iOS and macOS, and U171 on
macOS. No configuration field, CLI option, exit code or log line changed.

## The Kotlin behaviour, checked

`Marathon.kt:131-143` is `withTimeoutOrNull(deviceInitializationTimeoutMillis) { withRetry(3, 0) { borrow(); extract(device) } }`,
and the port has it in `crates/marathon-core/src/marathon.rs` (`extract_remotely`, `borrow_and_extract`). Each provider's
`borrow` picks at random through `hooks.choose_key`, with nothing that knows about an earlier attempt. A103's row was right:
with two devices and one that fails every parse, all three attempts land on the broken one in one run of eight.

## Design

**The trait.** `DeviceProvider` gains one method with a default:

```rust
fn borrow_untried(&self, tried: Vec<JavaString>, context: VendorContext) -> VendorFuture<'_, (DeviceRef, JavaString)>
```

It borrows as `borrow` does, but among the available devices whose key is not in `tried`, or among all if every one is, and
returns the device with its key. The default calls `borrow`, ignores `tried`, and names the device by its serial number, so
the test providers that implement only `borrow` (`pool_scheduler.rs`, `reconnect_test.rs`) are unchanged. The three vendors
implement `borrow_untried` and make `borrow` call it with nothing tried, so there is one loop per vendor.

Why this shape:

- **A method, not a field in `VendorContext`.** The context is built at dozens of sites and passed to every vendor call; a
  field would have to be set or defaulted at each of them and ignored by every call but one.
- **The provider returns the key.** Core cannot derive it from a `DeviceRef`: Android's key includes the adb server, which
  `DeviceInfo` doesn't carry, and iOS's is the devices-file key. Core only stores the keys and passes them back.
- **An owned `Vec`.** The call crosses to the vendor runtime in a `'static` closure.

**Core.** `extract_remotely` keeps a `RefCell<Vec<JavaString>>` for the parse, as it already keeps the last error, and
`borrow_and_extract` passes a copy to `borrow_untried`. When `extract` fails with anything but a cancellation, the key goes
into the list, once. A failed borrow has no device and marks nothing, and a cancellation, which is the scope's own expiry or
the run's, says nothing about the device. `with_retry(3, 0)`, the timeout scope, the error mapping and the `Parsed …` timing
line are untouched.

**The vendors.** Each narrows the set it already computes and then makes the same `choose_key` call, with the same label, over
the narrowed keys:

| Vendor | Set before the choice | Key returned | Replay key offered |
|---|---|---|---|
| Android | Ready and not held (D24, D41) | `<serial>@<server>`, always | As before: the bare serial, or `<serial>@<server>` when several servers expose it (D18), computed over all available devices before narrowing |
| iOS | `borrowable()` (D44) | The devices-file key | The same key |
| macOS | The map, now only Macs whose connection is up (U171) | The serial number | The same serial |

If no available device is untried, the set is not narrowed. A provider never waits because of `tried`: the wait loop only runs
when nothing is available at all, as before.

## The identity of a device

The key a provider borrows by, not the object. A reconnect gives a device a new object: Android's D24 registers a new
`AndroidDevice` after a disconnect, and iOS's D44 recovery a new simulator handle. A device that failed a parse and came back
is most likely still on the same stuck worker or broken emulator, and by object it would be untried again. The cost is the
reverse case, a device that a reconnect really fixed: it is avoided for the rest of this parse, but only while some other
device is untried, so no attempt is lost and none waits.

- **Android**, `<serial>@<server>`: the server name `host/address:port` and the adb serial, as D18 tells devices apart. The
  replay key can't serve, since it drops the server when the serial is unique, so a second server exposing the same serial
  between attempts would change it. The joined string is ambiguous only if a serial and a host name both contain `@`, which
  adb serials and host names don't in practice.
- **iOS**, the key of `borrowable()`'s map, which is the one the simulator has in the devices file and keeps across D44's
  recovery and a devices update.
- **macOS**, the serial number `<udid>@<host>`, the map's key, which `connect` keeps for a replacement.

## U171: what the macOS map holds

Read in `crates/marathon-macos/src/provider.rs`:

- `connect` inserts a Mac only after its setup succeeded, by serial. A second Mac with the same serial replaces the first in
  the map before the first is disposed, so the map never holds a replaced Mac.
- Nothing else removes or disposes a Mac before the parse. `initialize` returns before core parses, the device actors that
  dispose devices start after the parse, and `terminate` clears the map.
- **But a Mac whose connection is lost stays in the map.** Nothing reconnects it (D48 says so for the pool), and the SSH
  executor has no reconnect: `SshCommandExecutor::connected` is `is_connected()` on the one connection, and a command on a dead
  one fails. D36 already refuses a Mac whose connection dropped during setup on the same `connected()` test.

So `borrow` could hand the parse a Mac that cannot work, the shape A80 had on iOS. D53 filters on
`executor().connected()`, which reads a flag and does no I/O, as iOS's `borrowable` does with its handle. With no connected
Mac it waits, bounded by `deviceInitializationTimeoutMillis`, so a run whose only Mac lost its connection before the parse now
fails with `NoDevices` (`Timed out waiting for a temporary device for remote test parsing`) after that limit, where it failed
its three attempts at once. That is D44's trade on iOS. U171 is marked fixed.

## Tests, and each one without its fix

By temporary revert, each run against the crate's tests:

| Test | What it shows | Reverted | Result |
|---|---|---|---|
| `crates/marathon-core/tests/run_flow_witnesses.rs::a_parse_that_fails_on_one_device_takes_the_other_next_d53` | Stub devices a and b, the parser failing on a: attempts run on a, then b; the `Parsed 1 tests … on b` line | Core passing nothing tried | Fails: a, a, a |
| `…::a_parse_on_one_device_tries_it_three_times_as_before_d53` | One device: three attempts on it, its last error is the run's | Same | Passes, as it must: nothing changes with one device |
| `…::a_parse_that_has_tried_every_device_takes_any_again_d53` | Both fail: a, b, then a again, without waiting; the last error | Same | Fails: a, a, a |
| `crates/marathon-android/tests/provider_test.rs::borrow_untried_avoids_a_tried_device_by_server_and_serial_and_falls_back_to_any_d53` | Two fake adb servers with the same serial: the returned keys carry the server; with one tried, 20 borrows take the other; with both tried, both are picked; after the first server's device reconnects as a new object it is still avoided; plain `borrow` picks among all | The narrowing | Fails at the first 20 borrows |
| `crates/marathon-ios/tests/simulator_reconnect_limit.rs::borrow_untried_avoids_a_tried_simulator_across_its_reconnect_and_falls_back_to_any_d53` | The harness picks the first key: a with nothing tried, b with a tried, a with both tried; after the worker is lost and both come back as new objects, a is still avoided | The narrowing | Fails: a where b was expected |
| `crates/marathon-macos/tests/provider.rs::borrow_untried_avoids_a_tried_mac_and_takes_only_a_connected_one_d53` | Two Macs on two fake workers: with one tried, 20 borrows take the other; both tried, both are picked; one disconnected, only the other, also with it tried; none connected, `borrow` waits and takes the first to reconnect | The narrowing; separately, the connected filter | Fails at the tried loop; fails at the disconnected loop |
| `crates/marathon-cli/tests/ios_stalled_send.rs::a_stalled_send_while_parsing_leaves_the_parse_to_the_healthy_worker_d53` (ignored, real clock) | The built binary with two fake workers, one stalling every send, the bundle sent by parsing included: exit 0, `test1` ran, at most 2 sends to the stuck worker | Not reverted | See below |

The CLI test cannot fail every time without the fix: the binary's first pick is random and has no replay tape, so in half the
runs the healthy worker is picked first and both versions pass. Without D53 it fails one run in four (two or more parse
attempts on the stuck worker, three or more sends) and the run exits 1 in one of eight. The deterministic proof is the core
and vendor tests above. It was run here as listed in the next section.

The existing `a_stalled_send_while_parsing_is_sent_once_per_borrowed_device_u163`, one worker, is unchanged and still right:
with one device D53 makes the same three attempts. It stays as A103's witness of Kotlin's three, with a comment saying so. No
other test encoded the random retry: the JVM's `remote-retry` row fails the borrow, which marks nothing, and still gives three
borrows.

## Runs

- `cargo test -p marathon-core -p marathon-test-support -p marathon-macos`: all pass.
- `cargo test -p marathon-android -p marathon-ios`: all pass, 54 test binaries.
- The new ignored CLI test, by hand with `--ignored`: three runs, all passed. With a temporary print of the sends and the
  `Parsed` line, which is not committed, one run picked the stuck worker first: its parse send stalled for 295 s, the second
  attempt took the healthy worker (`Parsed 1 tests … on …@127.0.0.1:<healthy port>`, 295.6 s after the start), the stuck
  simulator's prepare sent once more, and the run passed in 296.5 s with 2 sends. The other picked the healthy worker first and
  passed in 1.6 s with 1 send, the stuck simulator's prepare. The first run, without the print, also took 296.5 s, the first
  of these two paths by its time. Without D53, the first path's second attempt would have had an even chance of the stuck
  worker again.
- `cargo clippy -p marathon-core -p marathon-android -p marathon-ios -p marathon-macos -p marathon-test-support -p marathon-cli
  --all-targets -- -D warnings` and `cargo fmt --check`: clean.
- Not run: `scripts/check.sh`, the remote check, the workspace suite, the other ignored CLI stall tests, the differential
  tools (no evidence, recording or comparator changed).

## What is unchanged

Three attempts with no pause; `deviceInitializationTimeoutMillis` around them; the error a failed parse reports; the
`Parsed …` timing line; the commands sent to devices and workers; and, for a healthy run, every `choose_key` call with the same
label and candidates, so replay tapes and recordings are unaffected. A narrowed candidate list appears only after a failed
attempt. No log line was added, so failed parses also log as before.

## Limits

- **The timeout half of A103 remains.** A stalled send is decided after 285 s or more (D31), and the default
  `deviceInitializationTimeoutMillis` is 180 s, so a stall in the first attempt ends the parse with `NoDevices` before a second
  attempt can start. D53 helps with failures that come sooner, such as a send that fails outright, a failing command or
  instrumentation, or a parse error, and with a stall when the limit is raised.
- The tried list lives for one parse. Batches and the pool are not affected.
- A device that a reconnect fixed is avoided for the rest of the parse while another is untried.
- No Android end-to-end test through the binary. The fake adb can give one device a failing `am instrument` while another
  passes, so the scenario is possible, but the binary's pick is random as on iOS, so it would show the fix only in some runs,
  and it would add a scenario and its fixtures for what the provider test, on the real provider over two fake adb servers,
  and the core test already show deterministically.

## Incidental bugs

None found.
