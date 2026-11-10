# WP x53: documentation for 0.13.0-rc.1

- Added user-facing D55–D65 entries, with old/new behaviour, unchanged contracts,
  migration actions and links to PLAN and the WP evidence. The guide now covers
  D1–D65 and distinguishes Phase 8 artefact removal from 0.12.0's Kotlin bug fixes.
- Added migration rows for diagnostic format/wording and debug-off visibility,
  failure text and `config-dump`, CLI text, Allure/annotation values and filters,
  image bytes, queue order/grouping, Unicode/socket text, Basic credentials,
  path cuts, serde output formatting and exact Apple idle expiry. D64/D65 need
  no configuration action; D65 can correct the run's results and existing status.
- Added the exact release heading and upgrade checklist, grouped changes,
  evidence and known limits to the changelog. README keeps 0.12.0 as current and
  identifies this rc separately. Real-device and farm runs of this rc remain
  pending and must be documented before 0.13.0.

## Source checks and discrepancies

- Checked `file_manager.rs`'s UTF-16 cuts and empty-component failure,
  `influx.rs`'s UTF-8 Basic encoding, annotation rendering/filtering, generated
  failure descriptions and `config_dump.rs`'s `kind` field. They match the final
  D55–D59 descriptions sampled here.
- Checked `OrderedQueue` insertion keys/FIFO iteration and Scheduler's retained
  pool-index join/aggregation. The final D60/D65 descriptions match those sites.
  Checked the tracing formatter/filter and shared framebuffer/GIF/resize paths
  against the logging and image documentation.
- Earlier briefs describe their implementation-time baseline: WP 8.2 retains
  `JavaPath`, and WP 8.5 keeps Java exception names. WPs 8.3 and 8.6b supersede
  those statements. WP 8.6b's initial claim that no native golden contains a
  generated failure is superseded by its preparatory EOF scenario and final
  six-occurrence audit. The final WP evidence agrees with the sampled code.
- The older user guide still claimed stable identities, exception class names,
  79-column help, heap tie order and Kotlin logger names as current behaviour.
  Updated those claims or marked their 0.12.0 scope, linking the replacements.
- PLAN §3.11 contains D55–D65 but records **no individual approval dates** for
  them; §11 also stops at D54. Each new guide entry states this gap instead of
  treating §3.11's historical 2026-10-12 heading or a commit date as an approval
  date. Supplying those dates requires the owner's approval record.
- Audited U184–U193: all are marked fixed. No additional unresolved Phase 8 limit
  comes from those rows. Carried forward the applicable 0.12.0 limits and called
  out the retained Apple event/line log-window race, whose differential-only
  gates do not change production. The image evidence explicitly distinguishes
  unchanged registered goldens from supplemental changed-image tests.

## Verification

- Release tooling extracts `0.13.0-rc.1` successfully.
- Relative documentation links and heading anchors in the five changed files
  resolve; `git diff --check` is clean.
- The required local check passes before commit and at final verification.
- No code, fixture or golden changed. As required for this documentation-only
  WP, no `scripts/check.sh`, remote gate or workspace tests were run. The
  changelog's macOS/Linux gate evidence is the existing Phase 8 verification,
  not a new execution by this WP.

## Incidental bugs

No new code defect found in the sampled sites. The missing approval dates and
pending rc device/farm evidence are the outstanding documentation qualifications.
