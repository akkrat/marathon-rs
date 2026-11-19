# x96 artifact publication and signal finish

Base `9ce312ff`. D97 and U235–U242.

## Design and invariants

- The Android pull owns a private file on the destination filesystem. ADB marks the receive complete only after DONE,
  exactly the STAT count, and writer closure. Cancellation before rename leaves the previous destination intact.
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
  and device verdicts, lifecycle and leases have until 23 s. The existing CLI exit callback owns the final second.
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
  The core relay witnesses check status 143, status 130, the report head start and handoff by 23 s.
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

The release acceptance reports were inspected. Real-device and Linux verification, and integration with x95's
process manager and finalized-recorder receipt, remain for integration. The standalone CLI signal witness checks the
worker's TERM attempt, reports, and status. The shortened device wait can finish before the original D30 escalation.
The x95 integration supplies process exit and containment at that point. A filesystem operation already entered
by the owner is completed under that owner's receipt. The signal budget limits how long the runner can wait to
observe the operating system's result.

Generated GIF encoders and report writers have separate production paths from vendor file receipt. In particular,
Android's GIF listener can construct an attachment after a failed capture job. This was found while auditing the
consumers of vendor transfers and is outside the transfer and signal changes in this package.
