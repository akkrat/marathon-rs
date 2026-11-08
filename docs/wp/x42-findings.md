# x42 — D38 on the last device, after D44 and D48

D38's documents said that a readiness probe that never succeeds ends a run on its last device "after about 6 to 13 minutes
plus D48's wait", and gave 33 and 50 minutes for a hung probe and for every command hung. Those figures came from an audit
whose logs are not in the repository ([x25](x25-findings.md)) and predate D44 and D48. This pass re-derives them from the
code and measures what a scripted worker can show. Documentation and one test; no product code changed.

## What decides the end now

On a pool's only simulator D48's wait decides, not the number of setups:

1. The simulator joins the pool before its prepare, so it is live from the start even if it never got ready: a device is
   kept out of the live set only when it is added while the pool already lacks one (`pool.rs:831`, `pool.rs:445`).
2. A probe that fails for the whole of `bootWait` fails a boot attempt; two attempts fail the prepare with `NotBooted`, and
   the device leaves the pool marked lost (`boot.rs:73`, `prepare.rs:106-108`). That departure starts the wait
   (`pool.rs:453`, `pool.rs:391`). iOS reports nothing already waited for a device, so the start is the departure itself.
3. The recovery sets the simulator up again at once: the first pause is `30 s × (loss − 1)`, so 0 s
   (`recovery.rs:547`). The re-setup is added to the pool while it lacks a device, so it is not live until it reports ready,
   and its own departure would not move the start (`pool.rs:453-466`). The pool logs `device <serial> has arrived` for it.
4. At `start + deviceInitializationTimeoutMillis` the pool stops the device still getting ready (`pool.rs:490`,
   `pool.rs:507`). The device actor cancels its prepare and joins it (`actor.rs:537`); the prepare returns as soon as it is
   cancelled (`prepare.rs:80`), so this takes no time of its own. The pool then logs `all devices terminated, remaining
   tests will not run` and ends.

So D48's wait runs alongside the re-setups, not after them. Only the first recovery starts within it; the 30 and 60 s pauses
and the fourth setup are never reached, because a re-setup that never gets ready can neither end nor extend the wait.
`remaining tests will not run` is logged once, at the end of the wait. The probe's failure mode changes only when the
simulator leaves the pool the first time, so it matters in the first case below and not in the second.

## The figures

With the defaults: `bootWait` 180 s (`vendor.rs:638`), `shell` 30 s (`vendor.rs:619`), 10 s between boot attempts and a 1 s
poll (`apple_simulator_device/mod.rs:60-61`), recovery pauses of 0, 30 and 60 s and three recoveries (`recovery.rs:77`,
`:531`, `:547`), `deviceInitializationTimeoutMillis` 180 s (D48). Over SSH a timed-out command gets TERM, then 20 s, then
KILL (`executor.rs:289`, `channel.rs:42`). The probe runs under `shell` (`simulator.rs:112`).

| Case | Sequence | End of the run | How it was obtained |
|---|---|---|---|
| (a) probe exits non-zero, from the first setup | boot, 180 s, shutdown, 10 s, boot, 180 s, shutdown: prepare fails at 371 s; re-setup at once; wait ends at 551 s and stops it | **About 9 minutes** after the first boot (551 s), with `remaining tests will not run` | Measured: `a_probe_failing_from_the_first_setup_of_the_only_simulator_ends_the_run_one_wait_after_the_first_failed_prepare_d38_d48`, 5,510 to 5,519 ms at 1/100 in three runs |
| (b) probe times out at `shell`, from the first setup | As (a); a probe in flight at each of the two deadlines can run 30 s past it | **Up to about 10 minutes**: 2 × (180 + 30) + 10 + 180 = 610 s | Computed from the constants above |
| (c) probe hangs and needs KILL, from the first setup | As (b), with 30 + 20 s per hung probe | **Up to about 11 minutes**: 2 × (180 + 50) + 10 + 180 = 650 s | Computed |
| (a), (b) or (c), from a re-setup after the simulator ran batches | Loss; re-setup at once, not live; the wait ends 180 s after the loss, before the re-setup's prepare could fail, and stops it | **About 3 minutes** after the loss (180 s), with `remaining tests will not run` | Measured for (a): `a_probe_failing_from_a_re_setup_of_the_only_simulator_ends_the_run_one_wait_after_its_loss_d38_d48`, 1,808 to 1,827 ms after the run began at 1/100, the loss at about 16 ms. (b) and (c) follow, since nothing in the sequence depends on the probe |
| (d) every command hangs | Not re-derived | — | See below |

The first-setup times count from the first `simctl boot`; the setup before it (connection, install) comes on top. All
times leave out what happens after the pool has ended: the run owner still joins the stopped prepare, and with a probe hung
in it that is the TERM, KILL and CLOSE sequence, up to 30 s (D30), not measured here.

A test that ends after the loss, reported by the device that left, would start the wait again at that moment (D48); the
re-setup figure assumes none does.

**(d) was not re-derived.** With every command hung the probe path is not the one taken: the descriptor read (`cat
device.plist`) times out too, which reads as state `UNKNOWN` and fails a boot attempt at once instead of after `bootWait`
(`boot.rs`, `simulator_state`), and before that the setup's own commands time out, which may keep the simulator from
joining the pool at all, so that the run ends as one without devices. Which of these happens depends on which commands hang
first. The 50-minute figure is taken out of the documents.

### With another simulator alive

The 26 to 30 minute figure of [x17](x17-findings.md) still holds: a `NotBooted` prepare is a loss, not a failed setup, so
D44 and D33's budget do not count it; losses are capped at three recoveries with pauses of 0, 30 and 60 s
(`recovery.rs:531`, `:547`), and D48 does not apply while the pool has a live device. Checked against the code, not
measured in this pass. For a probe that needs KILL the same count gives 4 × (2 × 180 + 10) + 90 + 8 × 50 = 1,970 s, about
33 minutes, which agrees with the audit's figure; it is computed, not measured.

## The test

`crates/marathon-ios/tests/simulator_probe_last_device.rs` runs the real scheduler, pool, device actor, iOS provider,
recovery and boot loop on the fake worker, with every default above divided by 100 and the probe answering exit status 1
with a ready marker in its output. A first-setup run takes about 5.5 s and a re-setup run about 1.8 s of real time. Their
upper bounds only separate "the wait ended the run" from "a further prepare failed first", with margins of 3.6 s and 1.9 s
(docs/testing.md rule 6). The probe's failure starts at the first setup or, counted by `simctl getenv … HOME`, at the
second; the loss in the re-setup case comes from three silent batches (D39, U165), as in `simulator_pool_recovery.rs`.

## Corrected

D38 in `docs/deviations.md` and PLAN §3.11, `docs/migration.md`, CHANGELOG's "Check before you upgrade", the table in
[x25-findings.md](x25-findings.md) (old figures kept, marked superseded) and the note in [x40-findings.md](x40-findings.md).

## What was run

- `cargo test -p marathon-ios --test simulator_probe_last_device` three times, 2 passed each time.
- `cargo test -p marathon-ios --test simulator_pool_recovery --test simulator_prepare`: 8 and 19 passed.
- `cargo fmt --check`, `cargo clippy -p marathon-ios --all-targets -- -D warnings`: clean.
- Not run, as the brief says: `scripts/check.sh`, the remote check and the workspace tests. Nothing ran on a real simulator.

## Incidental bugs

None found. Two observations, not bugs:

- The pool logs `device <serial> has arrived` for a re-setup that never gets ready, so a log of this case shows an arrival
  just before `no device returned within deviceInitializationTimeoutMillis`.
- On the last device a re-setup must report ready within the wait that its loss started. With the defaults `bootWait` and
  the wait are both 180 s and the re-setup starts after the loss, so a simulator that needs nearly all of `bootWait`, or a
  second boot attempt, to come up is stopped first. That is D48 as approved; raising `deviceInitializationTimeoutMillis`
  gives such a simulator room.
