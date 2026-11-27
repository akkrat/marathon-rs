# WP x123 — Batch-level files use their type suffix (D104)

Kotlin's `FileManager.createBatchFilename` at e47352f2 puts the batch ID in the suffix position.
The Rust planner had the same branch. Historical iOS smoke and retained local iOS pairs
reproduce the doubled batch log name. The change is approved by D104.

## Design and audit

- `jcompat::file_manager::FileManager::plan_file` now takes the suffix from `FileType` for every
  request. Batch-only requests keep the order `<batchId>[-<id>].<suffix>[-<chunk>]`. Test-only
  and device-only requests do not change. The existing length limits still apply to the complete name.
- Core batch logs (including Apple listener logs), iOS batch-failure movies and device logs, and
  Android batch-failure movies and GIFs already go through the shared output adapter. Where a path
  is needed again, such as HTML's per-test lookup or Android's batch-video readback, it comes from
  the same planner. Allure and the HTML video view use recorded per-test attachment paths and do not
  reconstruct batch names. Timeline does not resolve these files. No report code globs for the old
  spelling.
- iOS and macOS xcresult bundles already publish as `<batchId>.xcresult`. The Apple bundle listener
  uses the same pull destination for publication, and Apple result parsing uses the shared
  worker bundle path. Enabling Apple coverage does not add a second report path. Android coverage
  and device file sync use their own destinations, not the doubled suffix. Worker source names,
  D97's staging and delivery receipts, and U267's validation before movie publication do not change.
- The consumer search was completed before this revision. The one-off release analysis scripts
  identified there were left unchanged under the orchestrator decision. A glob for `*.mp4`
  now sees batch-level recordings as well as per-test recordings.
- The differential comparator accepts the doubled spelling only for historical, declared batch
  log paths. The host transport allowance is for the exact historical device-log receive
  destination, after D97's staging normalization. A live manifest must have the new spelling,
  and neither path rule masks wrong suffixes, locations or payload bytes. Historical JVM
  recordings and their contracts remain unchanged. WP 7.2's release projection already compares
  batch logs by report kind, not by basename, so the retained local iOS pair comparison needs no
  additional rule for the rename. Its raw diagnostic comparator remains strict.
- The shared core adapter and iOS delivery tests now pin the new spelling. The FileManager
  oracle keeps its fixture and continues to compare its unaffected cases. Explicit native
  tests pin a plain batch log, an ID-qualified device log, chunked video with and without an
  ID, a profile and path-length edge cases. No parity-matrix row names a replaced witness.

## Golden diff

Re-blessed with `tools/differential/run bless` and reviewed the expanded diff against f7fabfa0.
Eight scenario views change: `ios-smoke`, `ios-media`, `ios-screenshot`, `ios-ignore`,
`ios-pass`, `macos-smoke`, `macos-ignore` and `macos-pass`. Nine entries move from the
doubled UUID spelling to `<batchId>.log`: one batch log per scenario plus the iOS media device
log. All nine entries have byte-identical payloads and matching metadata. The iOS media host
rsync destination and its lane key receive the same rename. No worker source or other argv
changes. The re-captured media transcript is exactly the old golden after the existing D96
normalization, so the compare-time migration of that old native golden is removed. All other
golden views are unchanged. Provenance records the new capture.

## Verification

The new path-planner witness fails on the unmodified planner in a scratch worktree, producing
`batch.batch` instead of `batch.log`. It passes with D104. The scratch worktree was removed.
No new synchronization or real-time test was added, so no concurrency stress run applies.

Local formatting and changed-crate Clippy passed. The plan DAG and the parity-matrix check
against the release source passed. The retained e47352f2 iOS smoke pair passed the strict vendor
comparison. The registry check, committed-evidence validation, and all 119 differential
harness tests passed. The CLI's live fake-vendor test passed against the regenerated goldens.
The focused core artifact and iOS U267 delivery tests passed. The combined Cargo test run for
`jcompat`, core, Apple, iOS, macOS, Android and CLI passed (2,676 passed, 34 ignored). The offline
WP 7.2 tooling checks passed (109 guard tests and 62 outcome tests, with one pre-existing gated
outcome test skipped). Per the work-package gate split, the workspace and remote gates were
left to the orchestrator. No new real-device capture was run.

## Incidental bugs

None found in scope. Existing screenshot suffix mappings (C2) and the placement of the optional
chunk after the extension were intentionally left alone.
