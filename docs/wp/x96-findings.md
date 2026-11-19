# x96 artifact publication and signal finish

Originally based on `9ce312ff` and rebased onto `93c3cc4d` with D96 step 1. D97 and U235–U242.

## Design and invariants

- The Android pull owns a private file on the destination filesystem. ADB marks the receive complete only after DONE,
  a count matching STAT, and writer closure. STAT v1 only exposes the low 32 bits. Cancellation before rename leaves
  the previous destination intact.
  A timeout that races a commit already admitted by the owner can still return its successful receipt. Failed pulls
  produce one named non-delivery diagnostic, even when the timeout prevents the pull from starting.
- iOS and macOS receives own a private destination directory through copy or rsync and publication. Rsync must exit successfully
  with confirmed cleanup. A success status with inherited pipes still open is insufficient. Local copies complete
  traversal before publication. An xcresult commits directly under its batch name with exclusive rename. A failure
  retains the worker's source. D54 still lets an owned receive continue after its waiter leaves.
- Artifact listeners check receipts before emitting attachments or requesting removal. Android APP_DATA extraction
  checks the archive pipeline, then uses the file receipts before deleting application data. iOS uses the recorder's
  existing stop result. Failed transcoding selects the original finalized movie rather than ffmpeg's output.
- The relay stamps the first signal on the shared scheduler clock. Stop seals devices before actor cancellation,
  starts reports, and includes the first console flush in the report head start. Reports can hold provider and
  history finish for 10 s. Artifact admission and drainage have a 12 s cutoff, cleanup receipts have until 18 s,
  and device verdicts, lifecycle and leases have until 23 s. A stalling SSH command close requests KILL by 17 s.
  The exit path takes over by 24 s and has one more second for console output.
  Routine and internal-failure tests verify their respective 300 + 60 s and 30 + 40 s budgets.
- Provider shutdown and final lease waits already running observe the new deadlines. iOS stops recovery and retry
  producers at termination entry and joins them after artifact finalization. A device with unconfirmed work
  follows D93 quarantine. An idle device can dispose and release immediately.

## Witnesses

- Android fake ADB sends a short DONE, all bytes with no DONE, a missing DONE for an empty file, and a valid empty
  receive. The staged timeout witness waits for bytes before advancing the virtual clock. Both failure tests keep a
  prior destination intact. The staging obstruction test counts one record per attempted artifact. Android and iOS
  listener tests assert absent attachments and retained sources for missing or failed delivery. The transfer
  measurement test excludes the success line for a failed file while retaining the two successful measurements.
- Local and rsync test bridges inject an incomplete tree. Both leave earlier data and report no successful
  delivery. The rsync tests also cover success status with output pipes still held, and real local rsync and SSH
  preserve exact healthy payloads.
- The virtual iOS provider witness tightens a drain already running. The pull owner stops at 12 s, residual work is
  reported once at 18 s, and the confirmed device completes its lifecycle and releases its lease without extra
  timer delay. It observes recovery cancellation before the drain and the recovery join after it. The separate
  connection witness starts the final close first and observes abandonment at 23 s, with a sibling still connected.
  The core relay witnesses check status 143, status 130, the report head start and handoff by 24 s.
- Scratch-worktree reverts reproduced four expected failures. Renaming an errored Android receive replaced prior
  output in both the timeout and short-DONE cases. Removing the iOS signal cutoff returned to the longer drain.
  Removing the signal's lease cutoff returned to the 40 s close. The five timing witnesses passed 50 runs each
  and one additional run each while six CPU workers were busy.

## Golden diffs

Old result expectations exposed missing Android video, screen capture or trace attachments and an iOS video after a
false receive. The D97 witnesses now assert absent attachment entries and remote retention. The old Android timeout
witness expected a one-byte file at the report path. Its replacement expects the prior artifact byte for byte.
For xcresult failures, a diagnostic names non-delivery and the retained source with `local copy: none`. Successful
manifest names and payloads are covered by the existing artifact and differential suites. The JVM recordings were
left intact. The changed Kotlin command or diagnostic expectations are marked `n/a` in the parity matrix.
The D97 differential allowance removes the private directory component only from a native xcresult, video or
device-log receive's final host argv entry. The worker source, all flags, and public result paths and payloads remain covered by the stored views.
The first live comparison caught an extra source directory nested by openrsync. Receiving into an existing
private directory and renaming its named child restores the expected result tree on both rsync variants.

## Local verification

Formatting and targeted Clippy passed. The changed-crate suites and the CLI vendor differential were
run locally. The iOS and macOS disposal suites were rerun after restoring the routine receipt wait's
original timeout ownership. The CLI's real-signal harness covered both debug modes. Plan graph,
source inventory, parity matrix, differential registry, and 111 evidence tests passed. The native
golden diff was empty after the D97 allowance.

## Verification limits and incidental bugs

The release acceptance reports were inspected. Real-device, network-filesystem and Linux verification, and integration
with the finalized-recorder receipt, remain pending. The CLI signal witness checks reports, TERM, KILL and status.
SSH signals target the command channel. D96 independently sweeps local process groups at expiry and exit.
A filesystem operation already entered by the owner is completed under that owner's receipt.
The signal budget limits how long the runner can wait to observe the operating system's result. A hard kill can leave
Apple `.marathon-artifact-*` directories or Android pull staging on disk.

Generated GIF encoders and report writers have separate production paths from vendor file receipt. In particular,
Android's GIF listener can construct an attachment after a failed capture job. This was found while auditing the
consumers of vendor transfers and is outside the transfer and signal changes in this package.

Two existing `simulator_world` tests replace the process-wide SSH outbound exec test hook. The parallel iOS rerun
hit the 60-second watchdog in one of them. All seven tests in that target passed with one test thread. The rebase
includes the upstream fix that serializes users of that hook.

## Round 1 review witnesses

- Injected EINVAL, ENOTSUP and ENOSYS exercise exclusive-rename fallback with and without a pre-existing bundle.
  The exchange fallback witness exercises a successful merge and restoration after a failed publication. The fallback
  has the old presence-check race, and an exchange replaced by two renames can leave a short absence at the final path.
- The Android device witness invokes the scheduler's signal-seal entry point before waking a cancelled actor.
  It holds an ADB pull at a partial payload, checks that the actor cannot open another pull, and verifies cancellation
  at 12 s, timely disposal and retention of the previous artifact. The provider witness starts routine remote cleanup
  first and delivers a signal cutoff with the receipt cutoff already due. The remote command retains its 23 s window.
- A virtual SSH witness sets the signal deadline after the TERM wait has begun and observes KILL before CLOSE at 17 s
  and receipt expiry at 18 s. The CLI witness observes TERM and KILL on the worker in both debug modes. The core relay
  and CLI cutoff witnesses exercise the 24 s exit handoff.
- ADB's DONE tests inject a count above 4 GiB to distinguish STAT v1 wrapping from a v2 mismatch without allocating
  a multi-gigabyte artifact. Android listener witnesses check one missing coverage error, quiet missing APP_DATA, and
  deletion only after an explicit extraction receipt and successful pulls. The rsync invocation already excludes SSH
  configuration via `-F /dev/null`. An existing receive witness covers a success exit with inherited pipes still open.
- Scratch reverts removed the Android deadline override, restored the Android fail-fast join, removed the shortened
  SSH escalation, and restored the 23 s relay handoff. Each corresponding witness failed.
- Five virtual timing witnesses and two real-signal CLI witnesses each passed 50 isolated runs and one further
  run under six busy CPU workers.

## D96 rebase

- The signal relay retains the D96 expiry hook and recognizes SIGHUP with status 129. D97 retains the 23 s device
  bound and 24 s handoff. The CLI's D96 exit sweep and console allowance continue to run off core.
- A successful rsync receive publishes when D96 cleans up a pipe holder in the managed group. A detached holder
  keeps the receive unconfirmed and the staged bytes private. Both paths use the staged receive in the witness.
- The connection factory shares the provider's deadline with its SSH connections as soon as they open. Channel
  closes also observe any device admission deadline. A fake-worker witness starts a provisioning command without
  device admission, waits for TERM to be ignored, and observes KILL when the provider's clock reaches the cutoff.
  Removing the connection wiring in a scratch worktree left only TERM. The witness passed 50 isolated runs and
  one further run under six busy CPU workers.
