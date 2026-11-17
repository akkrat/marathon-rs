# WP x69 findings: a macOS replacement joins although the old Mac's disposal failed

Fixes A107. Base: main 0c94a641. No D-row (D78 was reserved and is not used; see "Contract").

## A107

**Before.** `AppleMacosProvider::connect` put the new Mac in the device map, disposed of the Mac it replaced (same
serial), released that Mac's lease (D74) and then returned the disposal's error through `?`. The old Mac stayed in the
registry, and the new one was neither registered nor announced: it never joined a pool, and only termination disposed of
it and released its lease. Kotlin does the same.

**After.** `connect` keeps the disposal's outcome, releases the old lease as before, removes the old Mac from the registry,
registers the new Mac and spawns its `Connected` event, and only then returns the outcome. The error is returned, not
logged: a Mac's `dispose` returns an error only when it is fatal (non-fatal ones are logged inside `MacosDevice::dispose`),
and returning it keeps it fatal. `initialize_transport` passes it on without releasing the new lease (its setup
succeeded), `initialize` cancels the other transports and returns it, and the run ends as before. A non-fatal disposal
error is therefore not reachable here, and no test covers one.

**Leases (D74).** Unchanged: the old Mac's lease is released in `connect` after its disposal, whatever the outcome; the
old Mac is no longer in the map, so termination does not release it again. The new Mac is in the map and its lease is
released once by termination, which takes the map out first (M-x65-2), so a repeated termination releases nothing.

## Witness

`crates/marathon-macos/tests/provider.rs`,
`a_replacement_whose_predecessor_fails_to_dispose_is_announced_and_released_once_a107`: a Mac is prepared and held in a
batch; the `FatalJoin` clock (from x65) makes its disposal's wait for the cancelled batch fail fatally; the same worker is
initialised again, so a new Mac replaces it. The test asserts: `initialize` fails fatally; one release (the old lease);
the old Mac is out of the registry and the new one in it; exactly two `Connected` events, the old Mac's and the new Mac's;
after termination no further event, two releases in all, and still two after a repeated termination.

With `provider.rs` reverted to main the test fails ("old Mac left the registry"); the event reads are bounded by 60 s so a
regression past that point fails instead of hanging.

## Contract

No contract item changes. The Marathonfile, CLI, result files and pinned text are untouched. The only effect is that a new
Mac is announced in a run that is already ending with a fatal error, and that needs a fatal disposal, which in tests only
an injected clock produces. Ledger-only fix; no golden changed, no CHANGELOG entry (nothing a user can see).

## What was run

- `cargo fmt --check`; `cargo clippy -p marathon-macos --all-targets -- -D warnings`: clean.
- `cargo test -p marathon-macos`: all pass.
- The temporary revert above.
- Not run, per the batch rules: `scripts/check.sh`, the remote check, `cargo test --workspace`. No Python validator is
  affected (no parity-matrix row, registry or plan change). marathon-cli suites not run: the change is reachable only through
  a fatal disposal, which no end-to-end or differential case produces. Nothing ran on a real Mac.

## Incidental bugs

- **iOS, same order, different outcome (not a new bug).** `AppleSimulatorProvider::register` also inserts the new
  simulator before disposing of its predecessor. Since M-x65-1 that is deliberate: on a failed predecessor disposal the new
  simulator is not announced, `register` returns the error and hands its lease back to the provisioner, which disposes of
  and releases it as a failed setup. Unlike macOS, the predecessor is removed from the registry before its disposal. What
  remains is that the disposed new simulator stays in the device map and the registry until a later registration or
  termination; `borrow` skips it (D44). The concrete simulator cannot fail that disposal while `register` holds a clone,
  so only the generic device seam reaches it. Not changed, per the brief.
- **U200: `connect` has no termination guard** (M-x69-1). A termination overlapping `connect` can leave a disposed new
  Mac in the registry. No lease is released twice and no announcement is delivered. Recorded as U200, not fixed.

## Review round 1 (Muse)

Verdict: merge. Two Low findings, both pre-existing.

- **M-x69-1**: recorded as U200 (above).
- **M-x69-2** (fixed): a fatal disposal at termination returned before `registry.remove`, so a Mac that had left the map
  and released its lease stayed in the registry. The removal now comes before the fatal return. Witness: the registry
  assertion added to `a_fatal_disposal_still_releases_the_lease_and_a_repeated_termination_releases_nothing_more_d74`,
  which fails with the old order.
