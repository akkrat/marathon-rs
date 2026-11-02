# Measured Kotlin log partition (diagnostic, not acceptance evidence)

Unmodified report manifests from full reference runs:

* with-batch-tail: `apple-macos-smoke-20261023T222505014344`;
* without-batch-tail: `apple-macos-smoke-20261023T222538595168`.

The latter failed the initial overly strict family-count comparison. Both have
three per-test logs/attachments and identical outcomes. The first has a fourth,
batch-level file containing `** TEST EXECUTE FINISHED **`; the second puts that
line in the last test log. Raw log content hashes differ and remain visible.
See Kotlin bug A20 and the explicit single optional batch-log allowance in
`apple_evidence.report_families`. These two manifests are regression inputs,
not complete certifying suites.
