# x112 findings: 0.15.4-rc.1 release notes

Release documentation for 0.15.4-rc.1, a release candidate that ships x110 (run-end timing lines) and x111 (the SSH
worker supervisor waits on events) on top of 0.15.3. The owner chose a release candidate first, so that a private CI
farm runs it before it becomes the latest release. No code changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` bullets become `## 0.15.4-rc.1`, under an intro and an "iOS and macOS workers"
  and a "Logging" heading. Evidence and Known limits are carried forward from 0.15.3 and changed to what is true for
  this release candidate. Evidence: the x110 and x111 findings, a real SSH-worker check against 0.15.3, CI, and what
  was not exercised. Known limits: x111's two timing edges are added.
- `README.md` is unchanged until 0.15.4 itself, as for earlier release candidates.

## Real-host evidence

A real SSH-worker check of main `3e83abaa` against 0.15.3 on throwaway simulators (Xcode 27.2): healthy runs, an early
and a mid-run stop, a crash with lease expiry, D98's busy recovery and the 0.15.3 overlap scenario all passed; the
supervisor used about a third less CPU per run, and the per-test times matched. It found one pre-existing,
low-severity issue: a recording only a few hundred milliseconds long is sometimes published as a zero-byte video with
no warning when its transcode fails (2 of 5 overlap runs on 0.15.3, 3 of 5 on the candidate). Not changed here; noted
for the stability work.

## Incidental bugs

The zero-byte short video above.
