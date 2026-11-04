# X11: the third-party xctest parser library is removed (D20)

The five `libxctest-parser.dylib` files are third-party software under their own EULA. This
repository distributed them without it, and the binary embedded them. The library is now
excluded, and an `xctest` run reads a copy the user supplies. See D20 in PLAN §3.11.

## Failure design

**Where the check runs.** Two places, because iOS learns the platform late.

- **Vendor construction**, in the `xctest` branch only (`marathon_macos::vendor` and
  `marathon_ios::ios_vendor`). It runs off core through `blocking`, before any connection,
  simulator or device wait. macOS checks `MacOSX`. iOS passes if any simulator platform has a
  readable file: a device's platform comes from its simulator type
  (`simulator_factory.rs::simulator_sdk`), so requiring `iPhoneSimulator` here would wrongly
  reject a run with only Apple TV simulators.
- **`XCTestParser::extract`**, before its three attempts and before `prepare_installation`,
  for the device's own platform. A retry cannot make a file appear, so this returns at once.
  An iOS run with files for other platforms only therefore has its first device by then, which
  may have booted a simulator. That is the one late case, and it installs nothing.

Both checks, and the read itself, open the file with `O_NONBLOCK` and then check that it is a regular
file. A plain open of a FIFO with no writer would block forever, during construction before any log
line explained why.

Physical device SDKs have no Kotlin resource and no supplied path, so they keep S3 from WP 4.8:
`No libxctest-parser.dylib for platform X`, three attempts. No v1 provider produces them.

**Exit status: 1 either way, on two existing paths.**

- At construction, a `ConfigurationException` on the uncaught path, as a missing vendor
  configuration (`ApplicationView.kt:79`, `render_uncaught`). `ignoreFailures` does not hide it,
  like any other configuration failure.
- At `extract`, a `TestParsingException` on the path of any parsing failure (`Marathon.kt:92`):
  logged, then `Marathon execution failed`, exit 1, or 0 with `ignoreFailures: true`.

Kotlin never meets this case, since its resources are in the jar. A jar without them would give
a `NullPointerException` at `XCTestParser.kt:64`, three attempts, then that same `Marathon.kt:92`
path, so the parse-time choice matches it and the construction check is the deviation's early exit.

**`nm`.** `construct_with_parser_libraries` takes a closure that only the `xctest` branch calls.
`factory_construction_is_inert_for_both_parsers` makes it panic under `nm`, and
`an_nm_run_never_mentions_the_variable_whatever_its_value` runs the real binary.

## Hidden copies

The search tool stayed in the session's temporary directory. It takes the references from git blobs of
`dfd0045c`, indexes every 32-byte window of the five files (windows with under 6 distinct byte values dropped),
and probes each target every 16 bytes, so a shared fragment of 47 bytes or more with enough
variety hits. It never prints content. It covered all 4227 blobs of the 53 commits reachable from
main (101086 decoded layers, 660 MB), recursively to depth 5: raw bytes; gzip and zlib at any
offset; zip members from local headers (also inside other data); tar, bzip2, xz, zstd and lz4;
base64 at all four alignments, URL-safe and wrapped; hex with and without separators; decimal byte
arrays; `\\u00XX` and `\\xXX` escapes; latin-1 through UTF-8; and transcript `base64` chunks
reassembled per connection, channel and event. 28 positive controls per reference, among them
rsync-style multiplexed splits in transcript rows shorter than the minimum fragment, and three
negative controls all passed before each scan.

**Results (paths reachable from main).**

| Class | Blobs | Paths | What |
|---|---|---|---|
| Whole files or transfers | 31 | 70 | The five resources, the gzip blob `730015f8…` of the `MacOSX` file, and macOS transcripts holding its rsync transfer (206152 bytes covered). Listed below. All are removed or redacted at this head. |
| Shared fragments | 52 | 371 | Sample-bundle binaries and whatever carries them. Stay. See below. |

**Fragments.** They occur in `fixtures/bundles/{ios,macos}.zip`, in the evidence and transcripts of
iOS and macOS runs that transfer those bundles, and 102 bytes in `classes2.dex` of
`fixtures/vendor-android/apk/app-debug-androidTest.apk`. In the tree after the edits there are 176
distinct fragments, and every one also occurs in those bundles or that APK, none only in a
recording. Seven are padding, at least 90% zeros, up to 8272 bytes. The other 169 total 13181
bytes, the longest 662, in `__TEXT`, `__DATA_CONST,__got`, `__LINKEDIT` and code signature blocks.
102 of those, 7509 bytes, also occur in Apple's own runner template in Xcode. The bundles are built
with Xcode from this repository's sample sources, which do not use the library, so these are shared
toolchain and system bytes. The fragments were classified by their position in our own binaries;
the library was not analysed.

**Limits of the search.**

- It cannot see fragments under 47 bytes, windows with little byte variety, encrypted data, or
  encodings it does not decode, such as UTF-16, byte-swapped data or raw deflate outside zip.
- rsync compression would hide bytes from it. No recorded rsync argv in any blob has `--compress` or
  `-z`, and the scan confirmed it found no such row.
- The 13181 bytes above are judged by origin, not proven unrelated to the library byte by byte.
- It covers git objects on main only. refs/t3 were ignored as instructed. It cannot see forks, clones,
  caches, or GitHub Actions artifacts (next section).

**History rewrite: remove by these 31 blob IDs, not by the 70 paths.** Deleting the paths would
also delete `fixtures/fake-worker/apple/macos-smoke/expected/raw/transcript.jsonl`, whose sanitised
version at head must stay. Evidence blobs carry their digest as a file name, so at head they are
already under other names. These are full IDs, as `git filter-repo --strip-blobs-with-ids` wants:

```
04592c31273322c01ca97295355fcfd5809eb418
153e9a59376110bda7f5374011d61e4000de7904
1a0d2be13e5f7161c090c2b2c3e3a2aa3eb4768a
238f233d96815e8ce15b309b70a384624dbc2b9b
26a6fe9375e8e58e9ed2a4f5b1b2fca493ef2ac5
3dff078febd07aa2423b9b0e261cd3f1fd3951c9
4035c028e6b818eab09b3e90bb67ed654813c731
40c515442492cde371829d52391e74c5e566521f
4ef957ef1eb21b1537c87e959e7601621451d088
4f3e6d01357e4f9cec99c3443a59a4638c9a13a6
4f4ccac60b2669d6fa42970c11d342471eecd597
5a0d433c99b464c91843cf8b0f91ae9dcbce08ad
5a17f52ad2da8c88a631449430a724d171be533f
65f3551f05e83ed6383fa16ad512eae145a8359f
6d6a77ee80116ee4679367cfe936fafd38ebaa2c
80b2950886c46e906780766cb0627120caa05a8a
88f6047bd2f77c33e63e539ed2fc427dbac090bc
8a787baaf263eefadc8d8eec2499634f87995311
8df70928bf57943b516585163a6139ef2e9376a9
a6888fed2b18c7d0cff667a48a0f45b143b1b084
af51eaa173ef10b08d79fc476b2477db3dd8a51f
b12f949e499c6a85de9f5925d769b1daf1ae882b
b97b4bb955fce69586f7b4a16c4d6b96c857a991
bb175e5c8286f3543b72c01d7f8336a89e62f4d2
dc54666304140d3acbd7d4812301f1336d2fb904
e57978e23b53082a00e31448061e268bb4742617
eb4893414ec4bc8d7c9375ec466ca282b5f19ef9
f383ccdc3a4fc68556846c86b5c5b89fccf49ddd
f6edd7ab2244bebd7cb4cb34fafe2f9dd7a4d946
f9987cd907c134b7a24ee1b6ceb1bc4609c28266
fbe4b355a0e33018d120bb447bea147640a51f30
```

The paths those blobs occupied, with the first 12 characters of each blob ID:

```
fbe4b355a0e3 crates/marathon-apple/resources/libxctest-parser/AppleTVSimulator/libxctest-parser.dylib
8a787baaf263 crates/marathon-apple/resources/libxctest-parser/MacOSX/libxctest-parser.dylib
5a17f52ad2da crates/marathon-apple/resources/libxctest-parser/WatchSimulator/libxctest-parser.dylib
bb175e5c8286 crates/marathon-apple/resources/libxctest-parser/XRSimulator/libxctest-parser.dylib
8df70928bf57 crates/marathon-apple/resources/libxctest-parser/iPhoneSimulator/libxctest-parser.dylib
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-ignore/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-ignore/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-ignore/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-parse/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-parse/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-parse/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-pass/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-pass/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-pass/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-smoke/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-smoke/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification-c95b2128/macos-smoke/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-ignore/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-ignore/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-ignore/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-parse/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-parse/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-parse/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-pass/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-pass/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-pass/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-smoke/jvm-a/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-smoke/jvm-b/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendor-verification/macos-smoke/native/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-ignore/jvm-c95b2128/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-ignore/jvm-v2/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-parse/jvm-c95b2128/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-parse/jvm-v2/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-pass/jvm-c95b2128/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-pass/jvm-v2/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-smoke/jvm-c95b2128/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
238f233d9681 fixtures/differential/vendors/macos-smoke/jvm-v2/blobs/730015f8bcb9bd906837544c8be9aa904826d151e252cd3aeac9ed87270a9d3b
a6888fed2b18 fixtures/differential/vendor-verification-c95b2128/macos-ignore/jvm-a/blobs/88e781d4c2bb9fdaaf960d2e817e400d54c9198902cafeb5f537bf34b79c0fbe
a6888fed2b18 fixtures/differential/vendors/macos-ignore/jvm-c95b2128/blobs/88e781d4c2bb9fdaaf960d2e817e400d54c9198902cafeb5f537bf34b79c0fbe
5a0d433c99b4 fixtures/differential/vendor-verification-c95b2128/macos-ignore/jvm-b/blobs/a691e14515124cb3e661db55020996c1bc5a2f83ad59e921574594b2e42f86c1
af51eaa173ef fixtures/differential/vendor-verification-c95b2128/macos-ignore/native/blobs/461d4642b9e4f4338d90ef290f982520640f37c9bfe5f7385a0c0e439adc8d14
6d6a77ee8011 fixtures/differential/vendor-verification-c95b2128/macos-parse/jvm-a/blobs/ca211262d59b36c4ebced67500d47ec69e51257523e291af2bcc983e72c39738
6d6a77ee8011 fixtures/differential/vendors/macos-parse/jvm-c95b2128/blobs/ca211262d59b36c4ebced67500d47ec69e51257523e291af2bcc983e72c39738
153e9a593761 fixtures/differential/vendor-verification-c95b2128/macos-parse/jvm-b/blobs/b39940ec5abf74f3ece40e6a10a3877eead818edf0b4fa195258302a883caddb
40c515442492 fixtures/differential/vendor-verification-c95b2128/macos-parse/native/blobs/074129c9fc978e4f8db750b27e4898b1e80db39a6b7102df19c57df4d9e9795c
3dff078febd0 fixtures/differential/vendor-verification-c95b2128/macos-pass/jvm-a/blobs/54065129bf47b2232498282f2bc948d47c0b3391748eaa8f2d29ed18d0d3a399
3dff078febd0 fixtures/differential/vendors/macos-pass/jvm-c95b2128/blobs/54065129bf47b2232498282f2bc948d47c0b3391748eaa8f2d29ed18d0d3a399
88f6047bd2f7 fixtures/differential/vendor-verification-c95b2128/macos-pass/jvm-b/blobs/7a38ea815ac7e9a71271fc59af0778780ca6b4a861f3368fe4818c1199eee79e
65f3551f05e8 fixtures/differential/vendor-verification-c95b2128/macos-pass/native/blobs/3d10ff090283de0a5c15bd210b30e0ff6279fa9c84f8bd9fe73efc6488b3d176
4035c028e6b8 fixtures/differential/vendor-verification-c95b2128/macos-smoke/jvm-a/blobs/b21eb692ed42a616be768551c8d232e4ada1f6787fab845ae9b7db247b977e15
4035c028e6b8 fixtures/differential/vendors/macos-smoke/jvm-c95b2128/blobs/b21eb692ed42a616be768551c8d232e4ada1f6787fab845ae9b7db247b977e15
4f3e6d01357e fixtures/differential/vendor-verification-c95b2128/macos-smoke/jvm-b/blobs/b97c7ca899360cea76c43859542182c02f32acf618626255943a2a03a90ac61d
1a0d2be13e5f fixtures/differential/vendor-verification-c95b2128/macos-smoke/native/blobs/5a9d0b91dfe08bc3e631d85fb14d9d425f288f83a17673680824430467ce82e9
04592c312733 fixtures/differential/vendor-verification/macos-ignore/jvm-a/blobs/b8d33614eb246ec6cdabe46760a2c34698b3047dc5d46d6bfbfd01450f605c32
04592c312733 fixtures/differential/vendors/macos-ignore/jvm-v2/blobs/b8d33614eb246ec6cdabe46760a2c34698b3047dc5d46d6bfbfd01450f605c32
80b2950886c4 fixtures/differential/vendor-verification/macos-ignore/jvm-b/blobs/846322c04104ab76182819a2d4367bf9a930c5a6f9102162b2e2bec071f67f98
dc5466630414 fixtures/differential/vendor-verification/macos-ignore/native/blobs/84fdf236f4a947c7c895916c00ea20f06467ca53b2d470ecad891351beb23ad5
4ef957ef1eb2 fixtures/differential/vendor-verification/macos-parse/jvm-a/blobs/94034eac74bfb13aa5c5e331a8fa9cb2244e708b718f1cfd87f861024d69593f
4ef957ef1eb2 fixtures/differential/vendors/macos-parse/jvm-v2/blobs/94034eac74bfb13aa5c5e331a8fa9cb2244e708b718f1cfd87f861024d69593f
b97b4bb955fc fixtures/differential/vendor-verification/macos-parse/jvm-b/blobs/588087480678b8579bf1429c50351f3cac68923168d97bf6f264e9d0b008c68b
b12f949e499c fixtures/differential/vendor-verification/macos-parse/native/blobs/324702619000d11d5508515110d9e1f8002a1ebf9b83eaa82282e680edf53315
f9987cd907c1 fixtures/differential/vendor-verification/macos-pass/jvm-a/blobs/b0e4f37b34e3ce69e3ead6b4b01007432c215ab01b3ae7cb0d6ad5aea651e8b0
f9987cd907c1 fixtures/differential/vendors/macos-pass/jvm-v2/blobs/b0e4f37b34e3ce69e3ead6b4b01007432c215ab01b3ae7cb0d6ad5aea651e8b0
4f4ccac60b26 fixtures/differential/vendor-verification/macos-pass/jvm-b/blobs/d1bde9832eed91487e98767c96e5b9cb792109beb6381b050df8ee262e03e0bc
26a6fe9375e8 fixtures/differential/vendor-verification/macos-pass/native/blobs/f5ad4b5b8d2b26b207d544345729fcb35dec5653f121e6f4f87d89abe6a079c1
eb4893414ec4 fixtures/differential/vendor-verification/macos-smoke/jvm-a/blobs/23c736b16d0117bcd9f083df2b5830af95592413edc8fc039b52c835d54f5c47
eb4893414ec4 fixtures/differential/vendors/macos-smoke/jvm-v2/blobs/23c736b16d0117bcd9f083df2b5830af95592413edc8fc039b52c835d54f5c47
f383ccdc3a4f fixtures/differential/vendor-verification/macos-smoke/jvm-b/blobs/fcb4a301941cb49b79ef162732ab2ed8c89dd27602384458645e17ac6840ba68
e57978e23b53 fixtures/differential/vendor-verification/macos-smoke/native/blobs/e0f3af250024ff5213def20b2d7be3917529dd3647fd34886a60fd19aee2a081
f6edd7ab2244 fixtures/fake-worker/apple/macos-smoke/expected/raw/transcript.jsonl
```

The blobs of `crates/.../PROVENANCE.md` and the older `manifest.json`, `evidence.json` and registry
versions hold only names and hashes.

**Outside git.** CI artifacts of earlier runs (`native-*`, `release-*`, `dry-run-0.99.0`) hold
binaries that embed the library. This WP deletes nothing there.

**Verification of the removal.**

- The final tree: no file holds a whole library; largest match 22724 bytes in total, longest 8272
  (padding), the fragment class above.
- `aarch64-apple-darwin` release build: no whole library; 33 distinct fragments, 1870 bytes in total,
  longest 96, sparse structures with 61–82% zeros among the long ones. 21 of them also occur in
  unrelated programs (`cargo`, `ssh`, `rsync`, `ls`, `plutil`). `marathon version` and `--help` work, and
  `cargo test --release -p marathon-cli --test ios_production` runs the `nm` path against a fake worker
  with that release binary.
- `x86_64-unknown-linux-musl`: see Evidence.

## Stand-ins

- **Parser tests:** `STAND_IN_LIBRARY` in `crates/marathon-apple/tests/support/parser.rs`, written by
  each test, says that it is a fake.
- **Differential, native side:** `PARSER_STAND_IN` in `tools/differential/evidence.py`, 76 bytes. The
  harness writes it to `<work>/supplied-parser/MacOSX/` and sets the variable for macOS native
  captures only. iOS cases use `nm` and get no variable.
- **Evidence, 32 records:** the `730015f8…` blob is deleted and its entry is `withheld`, keeping path,
  mode, size and digest. In `transcript.jsonl`, the rsync stdin rows of that one session hold zeros of
  the same size with a recomputed row digest and `"redacted": "D20"`. No other row changed.
- **`fixtures/fake-worker/apple/macos-smoke/expected/raw/transcript.jsonl`:** the same edit, and its
  `manifest.json` follows.

None of these is a copy from the Kotlin repository; all are recordings of ours. Details are in
`fixtures/PROVENANCE.md`. A new JVM capture applies both steps itself (`withhold_parser`,
`redact_parser_transfer`, also in `apple_evidence.collect`), and `validate` rejects a record that
stores the digest, or a recorded transcript with non-zero bytes in that session.

**New and recorded native evidence differ.** `write_record` withholds for JVM captures only. A live
native capture of the real digest is refused before anything is stored: it would be a regression,
and must not turn into a valid `withheld` entry that compares equal to the stand-in. `validate`
accepts `withheld` on a native record only from the two commits that recorded the older evidence
(`HISTORICAL_NATIVE_SHAS`, equal to the two manifests' `native_sha`). A new native transcript may
hold no redaction marker and at most 16 KiB in the library's transfer.

**A marker proves nothing by itself.** `redact_parser_transfer` used to skip any row that had a
`redacted` key, so nonzero bytes under a marker passed. Each marked row must now hold exactly `D20`,
belong to `stdin` of the library's transfer, and be all zeros. `apple_evidence.validate` checks the
same for the older recording.

## Comparator change

`normalized` gives one role string to `worker/shared/libxctest-parser.dylib` when the entry is
`withheld`, or when it is a native record holding exactly the stand-in. The scope is that path, in
`vendor-macos-*` contracts, with digest `730015f8…` and size 206152. Everything else about the entry
is still compared, including mode and presence.

What this gives up: the comparison no longer shows that the pushed bytes equal Kotlin's. It does show
that a supplied file arrives at the same path with the same mode, through the same rsync argv and the
same commands. Nothing else needed relaxing: all 15 native captures passed unchanged in every other
respect.

Tests are in `tools/differential/test_evidence.py` (`test_d20_*`). Positive: withheld against stand-in,
and against a recorded native. Negative: other bytes, an empty file, another mode, the real digest
stored, a wrong digest or size or flag on a withheld entry, `withheld` on another path, the stand-in
in a JVM record, library bytes in a JVM transcript, a live native capture of the real digest, a new
native record that withholds or carries a marker or a library-sized transfer, all marked chunks
refilled with nonzero bytes under valid digests, a wrong or misplaced marker, and any committed
record holding a library blob.
`make_registry.py` regenerated `registry.json`; both verification manifests were updated for the
changed `evidence.json` digests by the one-off rewrite, which asserted a byte-exact round trip first.

`tools/real-e2e` accepts `MARATHON_XCTEST_PARSER_DIR` in a host's `environment` and blocks a native
`xctest` scenario without it. Those runs keep using the real library from the host's Kotlin
distribution, on both sides.

## Embedded assets

Audit of every production `include_bytes!`, `include_str!` and `include_dir!`. There are no prebuilt
binaries, JAR or APK resources, or fonts among them now, and no `build.rs` in the shipped crates.

| Asset | Upstream | Terms |
|---|---|---|
| `reporters/html/{app.min.css,index.html}`, `reporters/timeline/{chart.css,chart.js,index.html}` | marathon's own report sources | `GPL-2.0-only`, as `LICENSE` |
| `reporters/html/app.min.js` | webpack bundle of 17 listed npm packages (versions from the Kotlin `package-lock.json`) | MIT, and `randomcolor` CC0-1.0; licence files taken from each npm tarball |
| `reporters/timeline/iframeResizer.contentWindow.min.js` | iframe-resizer 4.3.2 | MIT, David J. Bradshaw |
| `adam/colorProfiles/DisplayP3.icc` | byte-identical to skcms `profiles/mobile/Display_P3_parametric.icc` (git blob `3f0126be`), through adam 0.5.8 | BSD-3-Clause |
| `oracle/adb-request-jdk-srgb.icc` | output of the reference JDK, tags name Little CMS | GPL-2.0 with Classpath exception; lcms MIT |
| `adam/colorProfiles/sRGB.icc` | Hewlett-Packard, through adam 0.5.8 | **HP's own terms; `status: review`** |

`react-loading` and `load-script` ship no licence file; their sections hold the standard MIT text with
what their package states. `scripts/release/third_party_licenses.py` now fails when the list and the
production includes disagree, or when a production include has anything but a plain string literal as
its path, such as `concat!(env!("CARGO_MANIFEST_DIR"), ...)`, since such a path cannot be checked.
`scripts/release/test_release.py` tests this. It ignores comments and test code, including
`#[cfg(test)]` items and test-only modules.

**To decide: `sRGB.icc`.** Its grant allows use, copying and distribution without fee provided the file
is unchanged, including its copyright tag, and HP's name stays out of publicity. It is not one of the
usual open-source licences. It is embedded unchanged, and the generated file reproduces those terms. The list's `review` status
keeps it visible, and a test pins it as the only such asset. `release.py preflight` refuses any status
other than `open-source`, so a tag and a dry run both fail while it stands; ordinary CI and package
builds do not check it. Replacing it would change the input of the framebuffer conversion, so it needs a decision.

**adam.** `crates/marathon-adb` ports adam 0.5.8 (Apache-2.0), with a `Port of adam` header per file.
Adam's NOTICE and licence are now carried in `THIRD-PARTY-LICENSES`.

**vendor/russh.** Compared with the crates.io archive by its recorded SHA-256: 13 files differ. Section
4(b) was not met, since only `README.marathon.md` described the changes. Each of the 13 now starts with
a `Modified by marathon-rs` line, tested offline, and the README lists them. The archive has no NOTICE
file. `LICENSE` is carried.

## Evidence

- `scripts/check.sh` passes locally (rc=0, macOS, with the release source for the parity, inventory
  and fixture checks). The gate includes `scripts/release/test_release.py`, the differential and
  fake-worker Python tests, and the workspace tests with `differential_vendors`.
- `tools/differential/run native-vendors`: PASS, 15 complete vendor captures. In `macos-smoke` and
  `macos-parse` the file on the fake worker is the 76-byte stand-in, mode 0600.
- CI run 41815292623 on `b34f9bae`: gate on macos-14 and ubuntu-22.04 and `musl-package` green. This
  document followed that commit; the runs on the final head are in the WP report.
- The `x86_64-unknown-linux-musl` archive from that run's artifact: no match at all, in the zip, its
  members or the binary, and `THIRD-PARTY-LICENSES` does not name the library. CI ran `version` and
  `--help` in the pinned Alpine image. In the same image (x86_64 under Rosetta, no network), the
  default `nm` configuration reached `No workers found` and exit 1 without naming the variable, and
  an `xctest` configuration exited 1 with the supply message, 52 times of 52 repeats.
- One earlier `xctest` invocation there, the first after the VM started, ended with SIGSEGV (139) and no
  output, before the startup line. It did not recur in those 52 repeats, nor in 12 of `nm`. Its core file
  was deleted before it was read, so the cause is unknown. Nothing shows it comes from this change,
  which runs after that line, and Rosetta cannot be ruled out; see Limitations.

## Limitations

- An `xctest` run with the real library was not repeated in this WP. The differential suites use the
  stand-in, and the real-e2e harness needs real hosts. The code after the read is unchanged.
- The published history and CI artifacts still hold the bytes until the rewrite.
- The one SIGSEGV above is unexplained. The musl binary has not run an `xctest` configuration on native
  x86_64 hardware; CI proves only `version` and `--help` there.

## Incidental bugs

- The Kotlin `html-report/package.json` declares `license: ISC`, while the Kotlin README says the
  codebase is GPL 2.0. The section follows the README, and this is not a change to anything.
- The timeline page loads moment.js, D3 and a web font from third-party sites when opened. This is
  Kotlin's behaviour, kept, and noted in `THIRD-PARTY-LICENSES`.
- The heading of `vendor/russh/README.marathon.md` says "for the fake worker", but production SSH
  uses the crate too. Left as it is.
