# x116 findings: 0.15.6-rc.1 release notes

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

From the x115 findings: on a real SSH worker with the screenshot recorder, six interrupted-batch runs of 0.15.5 left
the capture running to the end of the run and three of them logged a `Dying because` line at exit; five such runs of
the fix stopped the capture with its batch and logged none. Healthy runs of both builds delivered the same artifacts,
with median wall times of 30.0 s and 29.2 s. D99 was not run on a real worker; it is covered by the C5 replay, a real
state-machine test and a run-graph test that checks the summary text. The farm run of the candidate is to look for
`Expected to run … more` lines and `Dying because` lines at exit.
