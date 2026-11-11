# WP x56 golden diff

Base for review: main `584462eb`. Original implementation: `75977f02`; audit: `0ebf2d76`.
Review fixes captured: `a75a5780`.

The owner rebase renamed `a284d0d9` to `75977f02` and `effc3453` to `0ebf2d76`.
Both old/new commit trees are identical; the rebase changed no view or code.

The generation was backed up outside the repository, then removed and re-blessed
only through `tools/differential/run bless`, as documented in its README. The
registry check passed without regeneration: no driver/input hash changed. No JVM
recording or copied fixture was edited or re-recorded.

## What changed

`tools/differential/run diff-golden --against 584462eb` emits exactly one hunk,
in `fixtures/contract/native-v1/provenance.json`:

```diff
-  "native_sha": "c85839599b3ce3d72e86826fbe2bbef4ab0917a7",
+  "native_sha": "a75a578061046818d7fb99eedf845267a324302a",
```

Driver hash, oracle source, platform and Python provenance are unchanged.

## Contract proof

All 18 `.json.gz` views are byte-identical to the base generation. An independent
entry-by-entry comparison of their decompressed objects found:

- 18 identical entry sets;
- 0 changed, added or removed entries out of 1,141;
- 0 changes in the 661 report entries (including directories);
- identical `stdout`/`stderr` contract entries in every case;
- identical worker/transcript/state/host-command entries.

The native console view is `evidence.contract_console`: it retains per-test
progress and the complete summary block, including `Marathon run finished` and
`\tRaw:`, and excludes diagnostics. Therefore the log changes do not require
changing any stored view or comparator allowance. Reports, their failure text,
commands, exit statuses and final state remain exact under the existing contract
comparison. In particular, no result-file view changed during the re-bless.

Before the re-bless, `tools/differential/run native-vendors` passed all 16 vendor
captures against the original generation. The bless itself captured those 16
cases plus `local` and `parse`; the final scoped Rust tests also run the native
contract suites. `tools/differential/test_evidence.py` passed all 107 self-tests
with the documented YAML helper built first. No comparison rule was relaxed.
