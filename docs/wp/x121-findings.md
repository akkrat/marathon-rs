# x121 — SSH worker supervisor hygiene

Scope: the Apple SSH implementation of D96. U276–U279 are implementation fixes. U280 was not used.
The SSH exec strings, forced-command allowlist, token operations, user configuration, result formats and
verdict rules are unchanged. The companion Perl script is installed from the same controller that uses
it. The extra receipt metadata travels on stdin of the existing `control-stream`, not in an SSH command.
No deviation, migration change or golden edit is needed.

## Claims and changes

- **U276 — Ownership at a release deadline.** `ConnectionFactory::close_lease` polls the close first,
  so it can start the executor's retained retirement task even when the release deadline is already
  expired. `SshCommandExecutor::abandon` originally marked only the data transport as locally closed.
  The supervisor now stops admitting new probes and takes the control transport even while a retire
  task or lease still holds the peer. The ownership lock is short and never held across an await.
  A separate async gate serializes dials and lets retirement use a reconnect already underway.
  After a dial, publishing under the ownership lock either
  installs the connection or abandons it locally if close won. Graceful disconnect leaves the active
  connection visible until the disconnect finishes. The factory also keeps responsibility for a
  connection handed off to a oneshot receiver but never taken before cancellation drops that receiver.
  Both paths use the existing quiet local transport-abort primitive. The real lease release bound is
  unchanged. At full abandonment, remaining remote work has the existing worker lease expiry as a
  backstop. Loss of just the data wire still allows prompt cleanup over the independent control path.
- **U277 — Cancellation during execute preparation.** The session open already raced its context's
  cancellation, but the earlier supervisor wait did not. The command now races that wait too, returns
  the original typed cancellation and reports no payload started. The bootstrap itself remains owned by
  the connection, with the original 12-second exchange deadline. Another caller can still share its
  result, and retirement still waits for an in-flight bootstrap rather than orphaning a known root.
- **U278 — Failed install retries.** A transport/protocol error reset the capability directly to unknown
  and enabled another install immediately. A worker's nonzero probe status was already cached as
  no usable Perl and did not have this defect. Retry spacing after errors is now 1, 2, 4 and at most 8
  seconds per data connection. Waiters share each probe and a new generation starts cold. Backoff
  schedules no install by itself and the wait ends on close. This preserves recovery when a later
  install would succeed, unlike latching the error for the connection's lifetime. A bootstrap that
  created a directory but whose reply never identifies it can still leave that directory behind.
- **U279 — Confirmed receipt housekeeping.** The old worker kept individual terminal files until
  `retire`. Completed leases queue only a locally sealed `Done(status)` proof on an already-open
  control stream. A later ordinary request carries up to 64 such proofs. The controller queues at
  most 512 per stream. The worker writes normal replies first, then under the installation's
  nonblocking shared lock removes only a matching terminal receipt and its diagnostic when no
  live state or lock exists. It never uses the wall clock or elapsed time as proof of delivery.
  In particular, `not-started` remains a fence against delayed exec and `unconfirmed` remains
  available through retirement. Statuses are restricted to the wire's byte range. Lost, skipped or
  unsent acknowledgements cost only disk entries until retire. Short commands never dial for cleanup.
  The retirement exchange window is now 15 seconds, still clipped by the executor's existing
  outer provider and signal bounds. Token exchanges keep their four-second window.

The controller never retries an exec with the same token. A receipt may be consulted over either
transport and keep its diagnostic until its proof seals the controller's shared result.
Housekeeping cannot change that result. Hosts with no ongoing control-stream requests still rely
on retirement for their entire root.

## Verification

The loopback witnesses hold an actual retirement on the surviving transport, cancel a command
while the bootstrap reply is gated, carry a proof on the next ordinary request, and hold a
retirement reply across the old virtual deadline. The Perl witness uses the x111 script copy and
proves matching receipts can be pruned while wrong-status, unacknowledged, unconfirmed, not-started,
and still-publishing receipts survive. The per-connection retry witness pins both the cap and a
new connection's independent state.

The stock-OpenSSH witness used executor abandonment as a shortcut for data-only loss. That left
the control transport usable in the old code and was the very leak U276 fixes. It now cuts only the
data socket at a loopback relay and still requires the real worker process to disappear before its
lease expiry. It abandons both transports after asserting that independent cleanup. No Kotlin
parity row names this Rust-only witness.

The five loopback and Perl witnesses passed 50 consecutive runs each and one more run with
four CPU load processes. With just the new witnesses copied to an unmodified scratch checkout
of the base, all five failed for their intended reason. Cancellation and abandon could not
complete behind their gates, the worker rejected proof metadata, no proof was transmitted,
and large-root retirement hit the old cutoff. Separately, resetting back to a cold capability
on install failure made the backoff unit witness fail.

Offline validation covered the differential registry and evidence, Apple scenario evidence,
the plan DAG, the release tooling and the parity matrix. There were no Kotlin test reclassifications
and no fixture or golden changes. The full workspace and Linux gates are reserved for the
orchestrator.

Local scope: workspace formatting, Clippy with warnings denied for `marathon-apple` and
`fake-worker`, and the `marathon-apple`, `marathon-ios`, `marathon-macos`, `marathon-cli`
and `fake-worker` test suites. This includes the built-binary and registered native vendor
differential tests. Pre-existing SSH tests that require opt-in fixtures stayed ignored.

The local APFS benchmark created terminal receipt files in one private root, then timed the stock
Perl `retire` operation only. It took **1.432 s for 20,000** and **9.665 s for 100,000** files,
including Perl startup and root removal. These are one measurement per size on a shared machine,
not latency thresholds. For scale, 40 simulators that each capture and pull one screenshot per
second generate roughly 80 receipts per second in aggregate, or 48,000 in ten minutes before
removals and other commands. Receipt roots are per data connection, not per run or per simulator.
Spread across multiple SSH workers, the per-root count is smaller. The pruning change does not
depend on reducing screen-capture traffic in a different work package.

### Real SSH worker

Ran the same six-test iOS video sample with the published 0.15.6 binary and the local x121 build,
in that order, on two disposable simulators and one shared data connection. Both used the same
throwaway forced-command authorization and a dedicated port forward. Package checksums and raw
logs are retained outside the repository.

| Build | Exit and results | Readable video and xcresult | Wall | Device release |
|---|---|---|---:|---:|
| 0.15.6 | 1, expected: 3 pass / 3 fail / 0 incomplete | 6/6 each | 157.291 s | 151 ms |
| x121 | 1, expected: 3 pass / 3 fail / 0 incomplete | 6/6 each | 38.295 s | 105 ms |

Normalized raw, JUnit and Allure results matched. All movies decoded with ffmpeg and all bundles
were readable with xcresulttool. Each trace had 107 supervised foreground
commands, six detached recorders, 12 supervised rsync senders and one control stream and retirement.
Neither run had a supervisor warning, an undelivered artifact, a `Dying because` line, a surviving
controller child or a supervisor root remaining. Both logged the same worker shell startup
diagnostic, unrelated to the supervisor. The baseline's Xcode logs show two overlapping batches
taking 120 and 107 seconds. One sample in each order does not establish a speedup or attribute the
difference to this change.

Both owned simulators, the staging directory, the throwaway authorization, the local private
key and the dedicated forward were removed. The shared SSH connection was not closed. Real
signal, outage and large-farm runs were not part of this acceptance.

## Incidental bugs

No neighboring product fixes. The single-connection reply handoff guard is part of U276 because
that same abandoned control dial uses the factory. The default bounded-release shim for injected
connection owners logs abandonment without calling it. Production lease release uses
`ConnectionFactory::close_lease`; the shim was left alone.
