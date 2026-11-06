# X17: Apple readiness, device failure matching and device removal (D38–D40)

The project owner asked on 2026-11-06 for these fixes to bugs the port had kept for parity. Each is an approved
deviation in PLAN §3.11. No configuration field, CLI option, exit code or command sent to a worker changed.

## D38 / A41: the exit status of the readiness probe

### The measurement came first

The trap: Kotlin may have dropped the exit check because the probe exits non-zero on a healthy simulator. Its only
comment is `//Not sure if it's really 16+`, and its history gives no reason: the code arrived whole in the iOS rework
and has not changed since. So the probe was measured before any code changed:
`xcrun simctl spawn <udid> launchctl dumpstate user/foreground/com.apple.SpringBoard`.

| Runtime | Simulator | State | Exit | stdout | stderr | Marker line |
|---|---|---|---|---|---|---|
| iOS 27.0, Xcode 27.2 | Throwaway, created for this and deleted | Shutdown, before boot | **149** | Empty | 4 lines, `device is not booted` | None |
| iOS 27.0 | Same | Booting, 6 probes from the first one after `simctl boot` | **0** | 69,581 to 135,618 lines | Empty | `0x4e03 M D com.apple.springboard.services` |
| iOS 27.0 | Same | Booted, 6 probes up to 20 s after it settled | **0** | 142,448 to 170,066 lines (19 MB) | Empty | `0x4e03 M A com.apple.springboard.services` |
| iOS 27.0 | Same | Shutdown again | **149** | Empty | 4 lines | None |
| iOS 18.6, Xcode 26.6 | Already booted, on a second Mac, read-only | Booted, 1 probe | **0** | 145,192 lines | Empty | `0x4703 M A com.apple.springboard.services` |

Each output has a second marker line, `"com.apple.springboard.services" = {`, whose last token is blank, so it never
counts. On iOS 27.0 the older probe, `launchctl print system`, exits 0 with 232 lines and no marker, which is why the
runtime branch has to stay.

So **the ledger row's description is right**: a healthy simulator exits 0, on both runtimes measured and in every phase
of the boot. The check is safe to add, and a failed or timed-out probe is not running, as below runtime 16.

**Not measured:** runtimes 16, 17 and 26. No simulator of them was booted, because the brief allows one throwaway
simulator here and nothing booted on the second Mac. Runtimes either side (18.6 and 27.0) agree, but a runtime that
exits non-zero on a healthy simulator would never count as booted, and each of its simulators would take the bound
below and leave its pool.

The throwaway simulator was created with `simctl create`, shut down, deleted, and confirmed gone from `simctl list
devices`. No other simulator was touched.

### What callers do with `false`

`is_running` has one production caller, `simulator_state`, which runs only when the descriptor's state is 3. `false`
means `Booting`, nothing else.

| Caller | With `Booted` | With `Booting` | Effect of a false negative |
|---|---|---|---|
| `boot_failure`, first look | Done | Goes on to poll | One more poll. `simctl boot` is sent only for `Shutdown` |
| `boot_failure`, poll loop | Done | Polls again after `boot_poll_interval` (1 s), up to `bootWait` | See the bound below |
| `boot_failure`, after a failed `simctl boot` | Treated as booting | Treated as booting | None |
| `shutdown_device` | Shuts down | Shuts down | None |
| `erase_device` | Shuts down, then erases | Shuts down, then erases | None |
| `wait_for_state` | Only ever waits for `Shutdown` | Same | None |

**The bound.** No path from `false` erases a simulator. One failed probe costs one more poll, shown by
`one_failed_or_timed_out_probe_costs_one_more_poll_and_no_new_boot_d38`. Only a probe that fails for the whole of
`bootWait` (180 s by default) reaches the failure path: the simulator is shut down, after a 10 s pause it is booted
once more, and after a second `bootWait` it leaves its pool with `NotBooted`. A probe in progress at the deadline may
overrun it by up to `shell` (30 s). `a_probe_that_never_succeeds_is_bounded_by_boot_wait_and_erases_nothing_d38` shows
this path: one boot, two shutdowns, no erase.

**How much is new.** Kotlin already takes this path for a probe that times out before it prints the marker, which sits
about 55% of the way through the output (line 93,060 of 170,066). The new cases are a probe killed after the marker,
and a non-zero exit with the marker in stdout. Locally a probe took 0.3 to 1.8 s, so reaching the 30 s limit on the
whole of `bootWait` takes a far slower host or link than measured here.

### Tests

The JVM rows stay the witness: `sim.running/{16,16.0,+16.1}/{fail,nul}` in `fixtures/oracle/apple-bin/probes.jsonl`
return `true`. `deviation` in `apple_bin_oracle.rs` expects `false` for exactly those six, changing only the return
value, and `d38_allowance_covers_only_failed_dumpstate_probes_that_kotlin_reported_as_running` pins the list and shows
that the healthy, starting and older-runtime rows are not covered. The command transcript is unchanged, so no
differential comparator needed an allowance. With the check temporarily removed,
`every_probed_wrapper_call_matches_kotlin` and both `_d38` device tests fail.
