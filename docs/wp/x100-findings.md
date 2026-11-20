# x100 ssh_session load flake

Based on `f4b6ecd6` (0.15.0). Test-only fix: no D-row, no found-bugs row and no CHANGELOG entry.

## Cause

`ssh_session.rs::await_cancellation_kills_nothing_and_is_alive_stays_true` ended with an A11 check that started
`echo hello`, awaited it and asserted `is_alive()`. A11 is kept Kotlin behaviour: `SshjCommandSession.isAlive` is
`command.isOpen`, read once when the session is built (`session.rs::session_control`). The fake answers `echo hello`
at once with exit status, EOF and CLOSE. On a loaded host the client's channel reader can process that CLOSE before
`execute` reaches `session_control`, and the snapshot is then `false`. The test assumed the command was still open at
construction, which only a fast host makes likely.

This is not a product defect. The Kotlin code reads `isOpen` at the same point, and nothing in production reads a
session's `is_alive` (the A11 row already records this). The row now also says that the snapshot is `false` when the
command has already ended at construction.

The test also used real-time sleeps as synchronization (forbidden by `docs/testing.md` rules 4 and 7). It waited
300 ms before cancelling and 100 ms before the negative transcript check. Neither sleep caused the observed failure,
but the 100 ms window bounded how long the check looked for a stray `signal`/`client_close`.

## Evidence

- Loaded repro of the old form (this Mac, 36 `yes` processes on 18 cores plus simulators from other work): the single
  test failed at run 100 of 100, at a load average of about 340 (1 min). The assertion was `finished.is_alive()`
  (`ssh_session.rs:878` on the base). Further loaded runs of the old form at load 50–90 did not fail: 0 of 200
  sequential and 0 of 400 in 8 parallel workers. The window is narrow, and the extreme load came from simulators
  that other work was running.
- Deterministic witness in a scratch worktree at the base: a 2 s wait for the channel to close, inserted just before
  `session_control`, makes the old test fail every time at the same assertion. The new test passes with the same
  injection. Its A11 command is still open when the session is built.

## Fix

The test now orders by events:

- Cancel: `tokio::join!` polls `await_result` first, so the await is already waiting on the hanging command when the
  cancel is issued. The 300 ms sleep is gone. The `Cancelled` result is the same either way, because the select is
  biased toward cancellation.
- Negative check (T22): the test runs `echo hello` on the same connection before reading the transcript. The fake
  handles one connection's messages in order and records each exec before answering it. Once the client has its
  output, the transcript therefore holds everything the client sent when the await was cancelled. The check is now
  limited to the cancelled channel (number taken from the fake's `exec` event). The 100 ms sleep is gone. The echo's
  deadline is now 60 s (hang guard, rule 1).
- A11: a `sleep 1000` session, which is open at construction by construction, is terminated and awaited, and its
  `is_alive()` still reads `true`. This pins "captured at construction and stays true after the command ended"
  without racing the command's end.

The test still pins T22 (cancelling an await sends no signal or close and the executor stays usable) and A11.

## Verified

- Mutant in the scratch worktree: the canceller also calls `session.terminate()`. The new negative check caught it in
  50 of 50 runs, so the event barrier sees a signal sent at cancel time.
- Fixed test under load (36 `yes` processes, load 44–53 at 1 min and 105–119 at 5 min, plus simulators): 300 runs
  in 4 parallel workers, 0 failures. Earlier, 200 sequential runs at load 66–69 (1 min) also had 0 failures.
- Whole `ssh_session` suite under the same 36-process load (load 50–70 at 1 min): 20 runs, 0 failures (50 tests each).
- `cargo fmt --check`, `cargo clippy -p marathon-apple --all-targets -- -D warnings`, `cargo test -p marathon-apple`.

Not verified: the original gate failure's output was not kept, so it may have been a different assertion. Every
other assertion in the old test was analysed. `Cancelled` holds whatever the order, because the select is biased.
A stray signal or close was never seen, and nothing in the cancel path sends one. The 5 s `echo` deadline could
expire under extreme load, and it is now 60 s. The remote Linux gate was not run (the orchestrator runs gates).

## Sweep of the same pattern (not changed)

`is_alive` on a short command occurs nowhere else in the SSH tests. Real-time waits that remain in the files using
the fake SSH helpers, for the record:

- `ssh_session.rs`: a 5 s sleep for a late open reply's text to reach and leave the queue (`late_open_reply` test).
  A 100 ms sleep lets `close()` start before the transport dies (`stubborn` close vs transport loss). A 200 ms sleep
  lets a flood fill the window before `close()`, with an `elapsed < 5 s` bound. A 20 ms sleep before `close()` in
  `await_concurrent_with_close_is_cancelled` makes no difference to the asserted outcome. Each orders different work,
  and none is the A11 race.
- `ssh_session.rs` (four places, including the test just before the fixed one) and others: `ssh::run(.., ["echo",
  "hello"], 5000)` uses a 5 s positive deadline, below rule 1's 60 s.
- `ssh_supervisor.rs` (6 s, 8 s and 4 s polling deadlines) and `rsync_file_bridge.rs` (a 5 s D43 window on a scaled
  clock) poll with hang-guard deadlines shorter than rule 1's 60 s. None failed in this work.

## Incidental bugs

None found in product code.
