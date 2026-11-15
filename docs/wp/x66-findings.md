# WP x66 findings: discovery duplicates and the redundant xctest push

Deviations: D75 (N11) and D76 (A9), approved 2026-11-15 (project owner's decision).

## D75 — JUnit3 discovery lists each test once

**Design.** `find_junit3_tests` (`crates/marathon-dex/src/parser.rs`) keeps the drop-last
re-scans of dex prefixes n…1: they are not only redundant work, a re-scan can find a
class whose superclass descriptor a later dex registered during an earlier pass. Only
the merge changes: a re-created `TestMethod` is dropped when an earlier entry has the
same content (`same_test_content`): the same name, the same annotations in order, each
annotation's values compared as a map (as `TestAnnotation`'s equals does) and decoded
arrays compared element by element, recursively. First-seen order is kept, and the
result is then sorted by name as before.

**Invariants.** `DecodedValue` equality is unchanged (array identity), so JUnit4
discovery, its `push_unique` merges and C19/C28 array identity behave as before. A
method found by both the JUnit3 and JUnit4 rules is still listed by both, as upstream
does (`find_test_methods`). Only the JUnit3 merge is by content.

**Witness.** `differential.rs::every_test_of_the_two_dex_fixture_is_listed_once_d75`:
`abstract-class-in-second-dex.apk` yields 21 tests, each name once, with both recorded
custom-annotation inputs. It fails with the old merge (checked by a temporary revert:
it and `differential_over_fixture_apks` fail).

**Differential allowance.** `differential_over_fixture_apks` and
`differential_over_probe_apks` still compare against the unmodified JVM recordings.
`d75_without_value_duplicates` drops, from the recorded list, every later entry equal to
an earlier one when array ids are ignored, and renumbers the remaining array ids by first
visit (DexOracle.java's numbering), so array sharing is still compared. The number
dropped must be exactly `D75_REPEATS` for the APK: 8 for
`abstract-class-in-second-dex.apk` (both of its rows), 0 for every other APK. Negative
tests: a repeat in an unlisted APK panics, a different count panics, and a same-name
entry with different array content is kept (so it must also be produced natively). A
JUnit3/JUnit4 pair with identical content would also be dropped by the allowance and then
fail the comparison; no recording has one (checked: the only same-name entries in both
JSONL files are the 8 repeats).

## D76 — a hosted xctest inside the app is not pushed again

**Kotlin.** `TestRootFactory.kt:259` compares `bundle.xctestBundle` (a `File`) to
`Path.of(testApp.path, "PlugIns")`, which is never equal, so the xctest is always pushed
to `<remote app>/PlugIns/<name>`. Even with matching types the intended comparison is
wrong (it compares against the `PlugIns` directory, not the bundle in it).

**Design.** `hosted` in `crates/marathon-apple/src/xctestrun/test_root_factory.rs` skips
that push (not the `mkdir -p …/PlugIns` before it) when `carried_by_application` holds:
`canonicalize(xctest) == canonicalize(app)/PlugIns/<xctest file name>`. Only the app is
resolved on the expected side, so:

- iOS app built by Xcode (`App.app/PlugIns/X.xctest`): skipped; the app push already put
  the same tree at `<remote app>/PlugIns/X.xctest` (rsync `--recursive --links --delete`
  of the whole app).
- macOS app (`App.app/Contents/PlugIns/X.xctest`): pushed, since the remote destination
  `<remote app>/PlugIns/X.xctest` is not where the app push puts it.
- xctest outside the app, a symlinked bundle or symlinked `PlugIns` (carried by the app
  push only as a link), an unresolvable path: pushed.
- A path to the app reached through a symlinked parent resolves to the same real path
  and is skipped correctly.
- `skipCopyingDerivedData` is untouched.

Canonicalisation is blocking I/O and runs on the vendor runtime's blocking pool
(`cmd::blocking`).

**Invariants.** The remote layout and the xctestrun bytes are unchanged: the factory
oracle compares the xctestrun bytes of `a9-embedded` and `baseline-ios` exactly against
the JVM, and the real-bundle frozen smoke bytes still match.

**Witnesses** (`crates/marathon-apple/tests/test_root_factory.rs`):
`a_hosted_xctest_inside_the_app_is_not_pushed_again_d76` (synthetic and real iOS
bundles; fails with the fix reverted) and
`hosted_xctests_the_app_push_does_not_carry_are_still_pushed_d76` (bundle in the runner,
symlinked bundle inside the app, real and synthetic macOS bundles). The installer oracle
`unit-copy` (4.10b) keeps its push of a bundle outside the app unchanged.

**Oracle allowance.** `d76_without_the_carried_push` removes, from exactly the JVM rows
`a9-embedded` and `baseline-ios`, exactly one shared push `{src: <app>/PlugIns/<name>,
dst: /tmp/marathon/shared/appUnderTest.app/PlugIns/<name>}` from both the ordered events
and the sends; every other row is compared unchanged. Negative tests: a listed row
without that push, a listed row whose xctest is outside the app, and an unlisted row
(left as recorded).

## Goldens

No golden changes. The native vendor cases (`fixtures/contract/native-v1`) extract the
xctest from a separate `tests.zip`, so it is never inside the app and both programs push
it; `differential_vendors` passes unchanged. No JVM recording was edited or re-recorded.
The parity-matrix rows touched by these fixes (`AbstractClassInSecondDex`, the hosted
installation tests) still hold as ported.

## Verified

See the final report for the commands run. Not verified: a real simulator or Mac run
with an Xcode-built hosted bundle (the effect on a real worker is one rsync fewer per
setup); timing on a farm. The claim that a duplicated JUnit3 test ran more than once
follows the ledger row and the adapter, which passes every discovered test on without
de-duplication (`crates/marathon-android/src/dex_test_parser.rs`); a whole Android run
with such an APK was not made.

## Incidental bugs

- Not fixed, recorded only: a method found by both the JUnit3 and the JUnit4 rules (a
  `TestCase` subclass with `@Test` on a `test…` method) is still listed twice, as
  upstream's `findTestMethods` concatenates both engines without merging.
