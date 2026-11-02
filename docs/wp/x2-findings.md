# x2: deviation D18, Android devices keyed by adb server and serial

User decision 2026-11-02: fix the same-serial collision of N27 (x1 findings) as deviation D18 (PLAN §3.11).

## Change

`crates/marathon-android/src/provider.rs`:

- `Inner::devices` is a `HashMap<(String, String), Provided>` keyed by (`server_name(client)`, serial) instead of the
  serial. The Connected insert and the Disconnected lookup use the server of the client whose device list changed.
- `borrow`: the choose-key replay label stays the bare serial (as in Kotlin); only when several ready entries share a
  serial is it `serial@server`, so the choice can still name one device.
- Unchanged: the per-server state tracker (already keyed by `host:port/addr`), `DeviceRef`/actor identity
  (`host:port:serial`), the `android.AdamDeviceProvider.connected` barrier key, log text, and the retention half of N27
  (disconnected entries stay in the map; `borrow` can return a disposed device, now of the same server only).

## Tests

| Test | Change |
|---|---|
| `provider_test.rs::same_serial_on_two_servers_is_keyed_per_server_d18` | replaces `same_serial_on_two_servers_collides`: a drop on either server names that server's device, the other stays healthy, and the serial's return on the dropped server yields a new device object |
| `reconnect_test.rs::same_adb_serial_on_two_servers_keeps_the_healthy_device_working_d18` | replaces `…_disposes_the_healthy_device_when_it_shadows_the_dropped_one`: 20 batches (none returned), no "already present" warning, A's fresh actor runs batches after the reconnect, B's device ran before the drop and keeps working; state-change lines still equal the Kotlin recording; zero fake-adb warnings; the scheduler receives Connected for B's device once and Connected, Disconnected (the same device object), Connected (a new one) for A's |
| `reconnect_test.rs::kotlin_witness_of_the_serial_only_device_map_is_unchanged` | new: asserts the recorded Kotlin outcome of `cross-host-shadowed` (21 batches) so the witness of the old behaviour stays pinned |
| `reconnect_test.rs::…_drops_the_right_device_when_it_is_the_latest_entry`, same-server tests | unchanged (Kotlin and D18 agree) |

The Kotlin and Rust CLI recordings under `fixtures/fake-adb/android-reconnect/expected/` were not re-recorded.
`cross-host-shadowed-rust*` is the pre-D18 Rust run and is kept as history; `tools/oracle/android-reconnect-probes/run`
would now show a Kotlin/Rust difference in that scenario only (batch count 21 vs 20), which is the deviation.

## Incidental bugs

`provider_test.rs::no_devices_messages` failed once (in its "unavailable" step, after the first of 3 runs of the file) and then passed
in 27 further runs of the file and 12 runs alone; 12 runs of the file on the x1 base also passed. The step re-uses a
just-freed port, so a parallel test taking the port is the likely cause. Not caused by the key change; not investigated.
