# x116 findings: 0.15.6 release notes

Release documentation for 0.15.6-rc.1, a release candidate that ships x115 (D99: a failure that meets an exhausted
total retry quota ends the test as failed; U270: SSH connections and screenshot captures are closed with their batch
or device, at the latest at the end of the run; U271: marathon's own stop of a screenshot command is not logged as an
ERROR) on top of 0.15.5. The owner chose a release candidate first, so that a private CI farm runs the screenshot-
recorder job where both ERROR lines were seen before it becomes the latest release. No code changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` entries become `## 0.15.6-rc.1`, under an intro, with their "Results" and "iOS"
  headings. Evidence: the x115 findings, the real SSH-worker runs of U270 against 0.15.5, CI, and what was not
  exercised. Known limits are carried forward from 0.15.5, with one added: a connection still being opened or released
  as the run ends can still be logged as lost at exit (x115's accepted edge).
- `README.md` is unchanged until 0.15.6 itself, as for earlier release candidates.

## Real-host evidence

From the x115 findings: on a real SSH worker with the screenshot recorder, comparing intermediate x115 builds with
0.15.5, six interrupted-batch runs of 0.15.5 left the capture running to the end of the run and three of them logged a
`Dying because` line at exit; five such runs of the fix stopped the capture with its batch and logged none. Healthy
runs of both builds delivered the same artifacts, except one run of the fix with five of six GIFs after delayed
xcodebuild output, with median wall times of 30.0 s and 29.2 s. U271's effect was not shown on the worker (too rare);
a loopback test shows it. D99 was not run on a real worker; it is covered by the C5 replay, a real
state-machine test and a run-graph test that checks the summary text. The farm run of the candidate is to look for
`Expected to run … more` lines and `Dying because` lines at exit.

## Review

One review by another model (REQUEST_CHANGES). Taken: D99 is described as a failure after the quota ran out, with the
corrected result shown in the per-test result and reports, and the refund caveat as a possible change of that test's
outcome; U270 says captures stop with their batch and that the iOS and macOS providers close remaining connections,
quarantined ones included, at the end of the run, with the per-simulator rate attributed to the farm runs; U271 is
qualified to supervised SSH commands; the Evidence names the intermediate builds, the one healthy run with five of six
GIFs, the healthy-run medians, and D99 and the quarantine close as test-only; Known limits add the 30 s close wait
after a cancelled open, and the quarantined-connection limit says it is now closed at the end of the run. Not taken:
the CI wording, since the tag is made only after CI passes on this commit.

## 0.15.6 without a release candidate

The owner approved the final 0.15.6 on the same day and asked to move the integration MRs to it as soon as possible,
so no 0.15.6-rc.1 was tagged: the `## 0.15.6-rc.1` section becomes `## 0.15.6` (no rc stub, since no candidate was
published), its Evidence says the farm run had not happened before the release, and `README.md` names 0.15.6 as the
current release. The farm check is the integration MRs' full pipelines with 0.15.6.
