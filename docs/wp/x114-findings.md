# x114 findings: 0.15.5-rc.1 release notes

Release documentation for 0.15.5-rc.1, a release candidate that ships x113 (U267: an iOS movie is published only if
it is a complete movie; U268 and U269: test-only fixes) on top of 0.15.4. The owner chose a release candidate first,
so that a private CI farm runs it with video on before it becomes the latest release. No code changes.

## Changes

- `CHANGELOG.md`: the `## Unreleased` bullet becomes `## 0.15.5-rc.1`, under an intro and an "iOS" heading. Evidence
  and Known limits are carried forward from 0.15.4 and changed to what is true for this release candidate. Evidence:
  the x113 findings with the two test-only fixes, a real SSH-worker check against 0.15.4, and CI. Known limits: the
  zero-byte short iOS video limit is replaced by what the structural check does not cover, and the Android video
  limit says that Android videos are not checked this way.
- `README.md` is unchanged until 0.15.5 itself, as for earlier release candidates.

## Real-host evidence

A real SSH-worker check of main `27807dd5` against 0.15.4 on throwaway simulators (Xcode 27.2):
- Healthy runs with video on, three per build: 6/6 videos published and decodable, 6/6 result bundles, report data
  equal to the reference run, no `was not delivered` line and no warning, no supervisor directory or FIFO left in the worker's `/tmp`.
- Short recordings: in the overlap scenario of earlier checks (one recording of about 0.1–0.3 s during an active
  XCTest session) the simulator left the movie empty in 3 of 10 runs of 0.15.4 and 2 of 11 of the candidate. 0.15.4
  published a 0-byte file and linked it from the report; the candidate logged one `was not delivered: the recording
  is empty` line, published nothing, and the worker's copy was removed by the simulator's next preparation. A variant
  with five instant tests in separate batches never produced an empty movie (55 recorders).
- The failing test's video was delivered and decodable in all 11 overlap runs of the candidate, with no recorder
  refusal and no busy warning.
- The healthy median wall time was 37.4 s against 34.9 s; of the candidate's runs, one had a slower preparation and
  one a slower run end. The short runs had a median of about 13.0 s on both builds.
- Not seen on the real worker: a movie cut short but not empty (`… is not a complete movie (<why>)`), and an empty
  movie with transcoding off or as the batch-failure video.

## Review

One review by another model (REQUEST_CHANGES). Taken: the iOS entry names what the check looks at (the structure,
not the length or contents), which videos it covers, that the not-delivered line is an ERROR after any transcode
ERROR, and that a file already at the path stays; the short-run times are called medians and the healthy-run
difference is described by its per-run causes; U268 says the failure was reproduced locally. Not taken: narrowing
the worker-cleanup and overlap-video claims, which the real check's per-run records support.

## 0.15.5

The private farm check of 0.15.5-rc.1 found nothing blocking. With video on, every failed attempt had a decodable
video and nothing was reported as not delivered, so no farm recording was empty. With video off, the candidate
matched 0.15.4 in total and job time and in per-test XCTest time. 0.15.5 is the candidate's code. The
`## 0.15.5-rc.1` section becomes `## 0.15.5`, and its Evidence gains the farm runs. A stub `## 0.15.5-rc.1` section
follows it, as for earlier candidates, and `README.md` names 0.15.5 as the current release.
