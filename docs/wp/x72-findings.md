# WP x72: a macOS Mac cannot join the provider after termination (U200)

Base: `ac39afbd`. Approved 2026-11-17 by the project owner. No deviation: no contract item changes (PLAN §3.12).

## Cause

U200, found in WP x69's review (M-x69-1). `AppleMacosProvider::connect` had no termination guard. A termination that ran
while `connect` awaited a replaced Mac's disposal took the new Mac with the device map and released its lease; `connect`
then registered that disposed Mac and sent its `Connected` into the closed channel, so the registry kept a Mac the map no
longer had. Reachable on the signal path, where the provider's finish step runs beside a run still cancelling.

A second case turned up while writing the witness: a Mac whose setup finished after termination was inserted into the
emptied map, registered, and its lease never released, because termination had already taken the map.

## Fix

`terminated: AtomicBool` on the provider. `terminate` sets it while holding the device map's lock, as it takes the map.
`connect` reads it under the same lock twice:

- before inserting: if termination came first, `connect` disposes of the Mac and releases its lease itself (termination
  will not see it) and returns the disposal's outcome, so a fatal error stays fatal;
- before registering and announcing: if termination came while the predecessor was disposed of, the new Mac went with the
  map and termination disposes of it and releases its lease, so `connect` returns without registering or announcing it.

Registration happens under the map's lock, so a Mac is registered either before termination takes the map (and termination
removes it from the registry) or not at all. Lock order is map then registry; no path takes them the other way round.

## Invariants

1. Every lease is released exactly once (D74): by termination for a Mac in the map, by `connect` for a Mac that never
   joined it, by `connect` for a replaced Mac (A89).
2. After termination no Mac is added to the map or the registry, and none is announced.
3. Without a termination nothing changes: the same order of insert, predecessor disposal, release, registry removal,
   registration and announcement.

## Verified

- `provider::a_termination_during_a_replacement_keeps_the_new_mac_out_of_the_registry_u200`: the old Mac's disposal waits
  for a running batch; a clock wrapper signals when it starts waiting, and termination runs then. Afterwards neither Mac
  is in the registry or the map, the new Mac is never announced, and the leases are released once each (two), also after
  a second termination. Without the fix it fails at "new Mac not registered after termination".
- `provider::a_mac_set_up_after_termination_is_released_not_connected_u200`: a setup after termination; its lease is
  released and it does not join the map. Without the fix it fails at "the late Mac's lease is released" (1, expected 2).
- Both ran 20 times in a row, all passing; no sleeps (Notify gates, 60 s hang-only timeouts).
- `cargo fmt --check`, `cargo clippy -p marathon-macos --all-targets -- -D warnings`, `cargo test -p marathon-macos`;
  the gates at merge.

## Not verified

Nothing on a real Mac. iOS is untouched: its provider already serialises registration and termination with its lifecycle
mutex.

## Incidental bugs

None.
