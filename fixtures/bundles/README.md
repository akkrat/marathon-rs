# Real Apple bundle products

`ios.zip` and `macos.zip` contain the required app, hosted unit-test and UI-runner
products. They are real builds, including complete Info.plists and Mach-O
executables, with no DerivedData caches, modules or dSYMs. Archive sizes total
about 213 KB. `build-profile.json` records tool versions and archive hashes;
`*-tree.json` records every payload's size, mode and SHA-256.

The exact source/build provenance is in `fixtures/PROVENANCE.md`. Rebuild into
disposable staging with `MARATHON_KT=/path/to/kotlin-checkout fixtures/bundles/build.sh`
(the source checkout must contain `e47352f2`), then run
`python3 fixtures/bundles/pack.py <printed-stage-directory>`. The packer writes
`<stage>/packed`, preserving payloads, modes and symlink targets and using sorted
entries/fixed ZIP entry timestamps. It never overwrites committed fixtures.
Compiler outputs need not have reproducible bytes across source/DerivedData
paths or Xcode versions; the checked-in hashes identify this specific build.

The scenario runner selects and repacks original entries into disposable app
and test archives; the actual Kotlin CLI extracts and validates those archives.
Kotlin's extraction does not retain Unix mode bits, and its captured chmod
command is exercised. Replay and CI need no Xcode or signing credentials.
