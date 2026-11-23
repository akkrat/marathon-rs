# x113 findings: no silent empty videos; test hygiene

Base `77a2e051` (0.15.4-rc.1). Three items: U267 (product, iOS video), U268 (fake ADB server, a test flake) and U269
(a test that left worker roots in `/tmp`). No D-row: U267 changes no contract. A movie that is not delivered is
reported through D97's existing "was not delivered" line, and the result files lose only the file and attachment of
a movie no player could open (the Allure result already had no video for it; see [Result files](#result-files)).

## U267: an iOS movie is published only if it is playable

### Problem

`crates/marathon-ios/src/listener/video.rs` `Media::deliver` pulled the movie (the transcode when ffmpeg succeeded,
otherwise the staged original at `-temp.mp4`) and published whatever arrived. A very short recording can make simctl
exit 0 after INT, start marker seen, and still leave an empty or unfinished movie. ffmpeg then failed with
`moov atom not found`. The fallback pulled the original and published it as a 0-byte file under `report/video/…`.
The only line in the log was the transcode's ERROR. Nothing said the video was missing, and D97's complete-transfer
check passed because the transfer itself was complete. The same holds without transcoding and for the batch-failure
video, which go through the same `pull`.

### Design

`crates/marathon-ios/src/listener/movie.rs` (new):

- `check(path)` walks the file's top-level ISO BMFF / QuickTime boxes: it reads each 8-byte header (16 with a 64-bit
  size), skips the box by its size, and succeeds at a top-level `moov` that fits in the file. An empty file is
  `Unplayable::Empty`. No `moov`, or a box whose size runs past the end of the file, is `Unplayable::NoMovieBox`.
  A box of size 0 runs to the end of the file, as the format says. Each box is at least 8 bytes long, so the walk
  ends; there is no box-count cap, which could reject a valid movie.
- `MovieStage` is a private directory created beside the report path, with D97's `.marathon-artifact-` prefix and
  mode 0700, so a hard kill leaves the kind of leftover D97 already documents. The receive writes into it under the
  report file's own name, so the transfer log line (`Transferred <name> …`) is unchanged. `publish()` checks the
  movie. A playable movie is renamed onto the report path (same directory, so the rename is atomic and replaces an
  earlier file). The directory is then removed, either way. `discard()` removes it after a failed or cancelled
  receive.

`video.rs` `Media::pull` now returns `Pulled::{Published, NotReceived, Unplayable(reason)}`. Creating, publishing and
discarding the stage run on the vendor runtime's blocking pool (`listener::blocking`), as the screenshot listener's
file work does. `deliver` reports `Unplayable` once through `undelivered`, as `Video <src> was not delivered: the
recording is empty` (or `… the recording is not a playable movie (it has no moov box)`), and removes the worker's
copy as it does after a delivery. The worker copy is unplayable too, and the next run's preparation would remove it
anyway. No attachment is registered and no file is published.

Why a box scan and not only `size > 0` or ffprobe:

- `size > 0` misses the other shape of the same failure, an unfinished movie (`ftyp` + `mdat`, no `moov`), which is
  just as unplayable. That is what a writer stopped before finalizing leaves.
- ffprobe would cost a worker command per video (or a local ffmpeg dependency that marathon does not require), and a
  worker command would also change the worker-command stream. The box scan reads a few headers of the local file.
  It costs no worker round trip and needs no ffmpeg.
- False rejections: every MP4 and QuickTime writer, fragmented MP4 included, writes a top-level `moov`. The tests
  cover simctl's layout (moov last, the fixture `recording.mp4`), ffmpeg's faststart layout (`transcoded.mp4`), a
  64-bit `mdat` and a size-0 last box.

Why a private directory and not "pull, check, delete": a test that runs twice in one batch records to the same
local path. Deleting after publishing would remove the earlier run's video, which its attachment already points at.
With the stage, a rejected movie never touches the report path.

### Android and macOS

Android's video listener (`crates/marathon-android/src/artifact_listeners/video.rs`) has no transcode and no
fallback, so it does not have this pattern. macOS has no video recording (`start_video_recording` is unsupported).
Neither was changed. Android publishing a pulled movie without a playability check is listed under
[Incidental bugs](#incidental-bugs) as a candidate, not seen.

### Result files

For a rejected movie, the change removes the published empty file and its video attachment, and nothing else. Where
the attachment showed:

- Allure already skipped it, because its reporter drops empty attachment files (`reporters/allure.rs`).
- The per-test JSON (`TestResultJson`, `report/json.rs`) listed it, pointing at the empty file. It no longer does.
- The HTML report linked it, since the file existed. It no longer does.
- JUnit never referenced videos.

This is the case the brief allows: an attachment that pointed at an empty file is dropped. No golden changed: no
scenario records an unplayable movie. The
CLI vendor differential (`differential_vendors`) compares the host rsync argv. Its D97 rule stripped exactly one
`.marathon-artifact-XXXXXX/` level from a native artifact receive's destination; a movie now has one more.
`tools/differential/vendor_transport.py` gains `U267_MOVIE_STAGE`: exactly one more optional private directory,
for `.mp4` destinations under `video/` only. The negative tests
(`test_u267_a_movie_receive_may_sit_in_exactly_one_more_private_directory`) cover a missing inner directory, two
extra directories, a non-stage directory, a JVM record, and xcresult and device-log destinations, which keep one
level. `tools/differential/registry.json` was regenerated with `make_registry.py` (only `vendor_transport.py`'s hash
changed).

### Witnesses

`crates/marathon-ios/tests/ios_listeners.rs`, driving `ScreenRecordingListener` with the fake worker and a fake
bridge that writes chosen bytes per remote path:

- `a_failed_transcode_publishes_the_original_only_when_it_is_playable_u267`: transcode fails. With an empty original
  and with an unfinished one (moov cut off), there is exactly one `ERROR Video … was not delivered: <reason>`, no
  attachment, no file under the report root (and no stage directory left), and `rm -rf` of the worker copy. With a
  playable original, it is published and attached byte for byte, with no "not delivered" line.
- `every_pull_of_a_movie_publishes_only_a_playable_one_u267`: the same for transcoding off, a successful transcode,
  and the batch-failure video (no test, so no attachment). An empty movie gets one ERROR and no file. A playable one
  is published as before.
- `movie.rs` unit tests: the box walk on the fixtures and on synthetic layouts, the rejections, and a stage that
  leaves an earlier report file in place on rejection and replaces it on publication, with no directory left either
  way.

The fake bridges in `listener_support` and `simulator_video_pull.rs` used to write `media` / `video` as the "movie".
They now write the committed playable fixture `fixtures/fake-worker/apple/media/recording.mp4`, read and never
edited. The fake-worker world and the CLI world already used the real fixtures.

Without the fix (base with the new tests copied in a scratch worktree): see [Revert checks](#revert-checks).

## U268: a reset between requests is a hang-up, not a warning

### Cause

Reproduced on base: 2 of 100 runs of `legacy_install_keeps_its_message_when_the_md5_never_matches_d23` failed under
CPU load (18 `yes` processes on an 18-core Mac). Both failed at `finish()`'s `assert_eq!(server.warnings(), 0)` with
`connection N: Connection reset by peer (os error 54)`. The transcript showed the warning's connection had sent only
`host:transport:emulator-5554`. The test's real 500 ms `push_file` timeout cancelled the next `md5` shell command
inside its transport switch. The client dropped the socket with the server's `OKAY` unread, so the kernel sent a
reset instead of a FIN. The fake server's `read_request` already treats a clean EOF before the next request as a
hang-up (`Ok(None)`), but it surfaced the reset as an error, and the connection task warns about every error that is
not a hang-up.

### Harness or product

Harness. Cancelling a command at that point is correct (the Kotlin marathon's `withTimeout` does the same), and the
real ADB server just closes the socket. So it is not a product row in the sense of a marathon bug. The found-bugs
row records it as our harness's bug, fixed.

### Fix (structural)

`crates/marathon-adb/src/server/protocol.rs` `read_request`: an error on the first read of a request's length (no
byte of the next request yet) is marked with `output_cancelled`. `connection()` in `server/mod.rs` marks errors from
answering a host request (`host::handle`, which writes the transport switch's `OKAY`) the same way. That covers a
reset that arrives before the server has written its answer. Both now record the existing `client_closed` event
instead of a warning, as a clean close does. `output_cancelled` marks only broken pipe, reset and abort. A reset
after part of a request, and any reset during an upload, sync transfer or device service, still warns.
`a_reset_during_an_upload_warns_in_strict_mode` is unchanged and passes.

The test itself is unchanged. Its 500 ms timeout is the mechanism it tests (D23: the enclosing push timeout fires
first and keeps its message), and wherever that timeout now lands, the outcome is the same.

### Witnesses

- `a_client_resetting_between_requests_is_not_a_warning_u268` (`crates/marathon-adb/src/server/tests.rs`): with
  `SO_LINGER 0`, so the close is always a reset, after `host:transport` with the `OKAY` read and with it unread, the
  result is 0 warnings and a `client_closed` event. Two length bytes of a next request, then a reset, is 1 warning.
- Stress on the installer test after the fix: 0 failures in 200 runs, and 0 in 150 runs under the same CPU load.
  Before: 2 failures in 100 under load and 0 in 100 without load.

## U269: the loopback-sshd test removes its worker root

`worker_commands_and_senders_stop_over_independent_real_ssh_d96` (`crates/marathon-apple/tests/ssh_supervisor.rs`,
macOS only) runs `real_worker_commands_and_senders_child` in a child process against a private sshd on loopback.
The child's executor installs the supervisor under `/tmp/marathon-supervisor-XXXXXXXXXX`. The test ends with
`executor.abandon()`, deliberately, to show that abandonment kills the worker command. Nothing retires the root, as
for a hard-killed run (documented under D96/x95e). Each run left one root.

Fix, test only:

- Right after the supervisor probe, the child runs one supervised `ps -o args= -p $PPID`. A supervised payload's
  parent is the worker script, started as `/usr/bin/perl <root>/supervisor.pl …`. The child writes `<root>` to the
  file named by `MRS_D96_ROOT_FILE`, in the parent test's temporary directory.
- The parent removes that root after the child process has exited, whatever its outcome (success, assertion failure
  or panic, as long as the child got as far as recording the root). It first checks that the path has the root's
  exact shape (`/tmp/marathon-supervisor-` plus 10 characters) and refuses anything else. It asserts the child's
  success first and then that the root is gone.
- The abandoned commands' owners outlive the child by a moment and still write their receipts into the root. The
  first version removed the root right away and failed once in 23 runs: a receipt arrived during the removal and
  left a root holding only that receipt. The parent now waits until `pgrep -f <root>/supervisor.pl` finds no
  process, with a 30 s hang guard, and only then removes the root. After that: 0 failures and 0 leftover roots in
  60 runs, and 0 in 60 under CPU load.
- What the test checks is unchanged. The added command runs before any of its steps, on the same connection.

Checked: a listing of `/tmp/marathon-supervisor-*` before and after one `ssh_supervisor` run showed 1 new root on base
and 0 with the fix. The other tests that run the worker script were checked the same way, with a before/after listing
around the full test runs of the crates below. The tests that create roots of their own (`d98_root`, `x111_root`,
the D96 witnesses) use `tempfile::TempDir`. `failed_worker_bootstraps_fall_back_d96` removes the root its `csh` run
creates right after reading it. The fake-worker and CLI worlds use a fake filesystem.

Not deleted: the roots already on the development machine. The pattern is `/tmp/marathon-supervisor-??????????`
(directories owned by the developer, each with `supervisor.pl` and a few `*.receipt` files, or only a receipt).
About 285 are there now, two of them from this WP's own measurements: the base run that showed the leak, and the
race that the second U269 commit fixes.

## Revert checks

A scratch worktree at `77a2e051` with this branch's test files copied in (`ios_listeners.rs`, `listener_support/mod.rs`,
`simulator_video_pull.rs`, `crates/marathon-adb/src/server/tests.rs`), product code unchanged. The scratch worktree
was removed afterwards.

- U267: both listener witnesses fail. With no line, where one `ERROR Video … was not delivered: the recording is
  empty` is expected. The other 42 `ios_listeners` tests pass with the new support code, so the fakes' switch to a
  playable fixture is neutral on base.
- U268: `a_client_resetting_between_requests_is_not_a_warning_u268` fails with
  `connection 1: Connection reset by peer (os error 54)` after `host:transport:d1`, the CI failure's shape.
- U269: no scratch run needed. A listing of `/tmp/marathon-supervisor-*` before and after one base run of
  `ssh_supervisor` showed one new root.

## What was run

On this Mac (macOS, 18 cores):

- `cargo fmt --check`, and `cargo clippy -p marathon-ios -p marathon-adb -p marathon-apple --all-targets -- -D
  warnings`: clean.
- `cargo test --no-fail-fast -p marathon-adb -p marathon-android -p marathon-apple -p marathon-ios -p marathon-cli
  -p fake-adb -p fake-worker -p marathon-test-support`: 1932 passed, 0 failed, 34 ignored. This includes
  `marathon-cli`'s `differential_vendors` after the U267 allowance; it failed before the allowance, which is how the
  extra directory was found. `/tmp/marathon-supervisor-*` had no new entries afterwards. This ran before the two
  review commits; their tests (`marathon-ios` listener tests, `ssh_supervisor`'s loopback test) were rerun after them.
- `python3 -I tools/differential/test_evidence.py`: OK, after `cargo build -q -p snakeyaml --example yaml_tree`.
  `python3 -I tools/differential/make_registry.py --check`: OK.
- Stress:
  - The installer test: 200 runs, then 150 under load (18 `yes` processes).
  - The U267 and U268 witnesses: 60 runs each, then 60 each under load.
  - The U269 test: 60 runs, then 60 under load.
  - All 0 failures.

Not run (the orchestrator's gates): `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`. The
Linux build of `ssh_supervisor.rs` was not compiled here; the new helper is `#[cfg(target_os = "macos")]` like the
test that uses it.

## Not verified

- No real simulator run: the U267 fix was not exercised against simctl's own short-recording failure. The box
  check was exercised on ffmpeg-made movies (the fixtures) and synthetic layouts only.
- U268's CI flake was reproduced locally under load. The CI failure's own transcript was not available, so a reset
  inside the push itself (a sync transfer, which still warns by design) was not ruled out for that one CI failure.
  The push of the small fixture APK takes milliseconds, against the 500 ms timeout.
- U269's cleanup on a child failure was checked by reading the code, not by forcing a child failure.

## Incidental bugs

- Android video (`crates/marathon-android/src/artifact_listeners/video.rs`): a pulled movie is published without a
  playability check. No empty Android movie has been seen; `screenrecord` finalizes on SIGINT. Candidate only, not
  changed.
- The batch-failure iOS video is published as `report/video/<pool>/<serial>/batch.batch` (no `.mp4` extension), as
  the listener tests show. Pre-existing, not changed. Not checked against the Kotlin marathon.
