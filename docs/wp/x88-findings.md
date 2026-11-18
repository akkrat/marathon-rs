# X88 — Rotate a saturated shared SSH connection (D90, U172)

## Reproduction

Sharing is off by default. With sharing enabled, each Apple worker transport has a shared executor. D30 can give up on a
session whose peer never answers CLOSE. If the peer keeps the session slot, a later channel-open refusal becomes an
`OpenFailException`. `safe_execute` logs the exception and returns no result. The iOS descriptor read clears its cached
value, and `simulator_state` returns UNKNOWN because the transport itself is still connected. A setup or recovery that
acquires the same shared executor can hit that refusal again within the D33 and D44 budgets. D30 retirement of the
simulator whose xcodebuild may still be running follows a separate path.

The original fake-worker `withhold_close` fault suppresses the reply but releases the slot on client CLOSE. The new
opt-in `hold_withheld_session_slots` fault takes the slot before handing CLOSE to the channel actor and holds it until
the transport ends. This makes the slot exhaustion reproducible with two sessions.

## Design and invariants

- D30 records abandonment on the exact connection that owns the session. A later resource-shortage or connect-failed
  refusal makes that generation eligible for replacement. OpenSSH uses `SSH2_OPEN_CONNECT_FAILED` when `session_open`
  fails at `MaxSessions`. [OpenSSH channel-open code](https://github.com/openssh/openssh-portable/blob/master/serverloop.c),
  [OpenSSH session limits](https://github.com/openssh/openssh-portable/blob/master/session.c).
- A refusal without a preceding D30 abandonment keeps the current generation. A low configured session limit can
  produce ordinary contention. Replacing connections in response would turn that contention into repeated logins. Each
  connection logs its first session-open refusal at WARN, including with debug off. The existing error and `safe_execute`
  handling still apply to the refused command.
- One factory lock covers slot reservation, removal from shared admission and publication of the pending attempt. New
  acquisitions wait for that attempt. A health check pins the old lease while it runs and checks that the executor is
  still current before handing it out. Handshake and close I/O run outside the lock. The retained acquire path still
  releases a lease whose caller cancels or drops before accepting it.
- Two permits per transport bound the number of shared generations. For real SSH the client handler keeps the permit
  until the transport ends, including while a queued disconnect is still being sent. At the cap a current executor can
  still be leased. If there is no current executor, a pending acquisition waits up to 30 s for a permit. A failed
  replacement leaves the old leases usable and lets a later acquisition retry under the existing provider policies.
- Each old lease stays bound to its own executor until released. The old connection is closed once, on the last release.
  Active sessions and connection-loss notifications stay bound to that executor. D30 still retires the simulator that
  abandoned its batch. iOS can acquire the replacement during D33/D44 setup. macOS uses the same connection factory and
  its existing provider policy. The detached worker process can survive even after its session slot goes away.

## Evidence

- `simulator_world::a_saturated_shared_worker_rotates_and_prepares_a_simulator_without_ending_old_sessions_d90` uses
  real loopback SSH and the Apple simulator world. TERM and CLOSE deadlines are released after their wire events. The
  test checks ordinary exhaustion, abandonment alone, one replacement for concurrent acquisitions, preparation of an
  iOS simulator on that replacement, survival of existing sessions, the two-generation limit and each transport's final
  close. It counts one WARN on each refusing generation. The simulator's file bridge uses the same worker filesystem
  in-process. The test takes a fresh lease directly, at the boundary the iOS setup and recovery paths use.
- `connection_factory::rotation_shares_one_handshake_and_counts_a_closing_generation_d90` gates a health check, the
  replacement handshake and the old executor's close. Concurrent acquisitions must share the replacement, the delayed
  health check must not hand out the retired executor, and a closing generation must still count against the cap.
- In a scratch worktree at `d5b0181d`, with the test and fake-worker fault copied in, the wire witness failed at the
  rotation assertion. The factory witness failed in the scratch worktree after disabling the rotation decision, when
  an acquisition went back to the saturated executor. Both witnesses passed 50 consecutive runs and one run under load
  from four CPU workers on the implementation.
- The normal differential cases exercise healthy workers or other failure branches. The new scenario changes worker
  connection count and can change which devices finish preparation. There are no golden diffs.
- Local crate suites covered Apple, iOS, macOS, fake-worker, CLI, logging and test support. The initial multi-crate run
  found the new target missing from the logging directive inventory. After updating that inventory, the logging suite
  and the CLI subscriber test passed. The CLI production smoke and vendor differential passed in the multi-crate run.
  Formatting, Clippy for all changed crates with warnings denied, the plan graph, source inventory, parity matrix,
  differential evidence and fake Apple evidence checks passed.
- A real Apple worker, provider scheduling through a full D44 retry and remote Linux were outside local verification.

## Incidental bugs

None added to the ledger.
