# x127 — 0.15.7 release notes

## Evidence audit

- Compared the starting changelog and README with the 0.15.6 notes and the release procedure. The 0.15.6 farm runs
  in x116 are historical evidence only. The 0.15.7 farm run belongs after this WP merges and before the tag, with
  no release candidate.
- Checked D100 and D102–D106 against PLAN §3.11, the deviations and migration guides, the x118–x125 findings,
  the changes since 0.15.6 and the U272–U287 ledger range. U280 and U283 were unused. The x122 harness changes
  are not new product behavior. Its ADB setup diagnostic changes wording, not the outcome, and the independent
  `sshpass` reproduction leaves U173 open.
- Separated the API 36 emulator and remote iOS simulator comparisons from the fake-adb, loopback and differential
  evidence. The recorder, frame transport and log receive timings describe those operations, not a general suite
  speedup. The x121 real-worker run is a healthy smoke comparison, not a real outage or deadline test. D103 and
  U162 were not run on Android devices, and D105 and U284 were not tested against production backends.
- Confirmed the macOS and Linux gates passed on integrated code at `46273fc7`. The delta to `a3b07d4f` is docs
  only. At the CI check during this WP, that later commit's Linux gate was red, its macOS gate was still running,
  and its musl package check had passed. The notes refer to the passing code run, not to CI on the final tag commit.

## Corrections and migration scope

- Replaced the old Android zero-byte-video and failed-GIF limits with D102's structural MP4 validation and finished
  GIF rule. Validation is not decoding. Corrected x119's D70 statement because its own gated review and U286 show
  that the video stop runs before accounting. U287 still applies when two tests share a report path.
- D103 requires fresh same-pool peer progress for each strike. A strike does not establish a device fault, and
  retirement can change retry placement, test verdicts and exit status even though quotas are unchanged.
- Removed the old 30 s screenshot close-acknowledgement delay from the carried limits. D100 no longer waits for
  that receipt on the capture path. U276 closes the established control transport as well as the data transport
  at the release deadline. Neither change promises that an SSH connection still opening or a worker root whose
  bootstrap reply was lost cannot outlive its owner.
- D104 changes batch artifact names, including device-log names. The doubled device-log name in the x125 comparison
  is valid for its pre-D104 base. D106 itself changes only the controller's OpenSSH option on that receive. The
  log still blocks its batch, and a receive error can still lose a simulator. The D100 forced-command change is
  separate. D105 changes admission, not send or shutdown budgets. Carbon reuse still allows loss on a racing close.

## Checks and owner follow-up

- The offline release tooling suite passed all 81 tests. Offline preflight passed for `0.15.7`. The workspace and
  remote gates are left to the orchestrator.
- Confirm the macOS and Linux gates on the final release commit before tagging. Replace the farm placeholder with
  the results from the post-merge run and retain the macOS UI coverage limit unless that scenario was exercised.
  If the farm lacks the opt-in recorder or device-log settings, keep the scope of those measurements explicit.
