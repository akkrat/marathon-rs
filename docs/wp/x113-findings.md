# x113 findings: no silent empty videos; test hygiene

Base `77a2e051` (0.15.4-rc.1). Three items: U267 (product, iOS video), U268 (fake ADB server, a test flake) and U269
(a test that left worker roots in `/tmp`). No D-row: U267 changes no contract. A movie that is not delivered is
reported through D97's existing "was not delivered" line, and the result files lose only the file and attachment of
a movie whose structure is incomplete (the Allure result already had no video for it; see
[Result files](#result-files)).

## U267: an iOS movie is published only if it is a complete movie

### Problem

`crates/marathon-ios/src/listener/video.rs` `Media::deliver` pulled the movie (the transcode when ffmpeg succeeded,
otherwise the staged original at `-temp.mp4`) and published whatever arrived. A very short recording can make simctl
exit 0 after INT, start marker seen, and still leave an empty or unfinished movie. ffmpeg then failed with
`moov atom not found`. The fallback pulled the original and published it as a 0-byte file under `report/video/…`.
The only line in the log was the transcode's ERROR. Nothing said the video was missing, and D97's complete-transfer
check passed because the transfer itself was complete. The same holds without transcoding and for the batch-failure
video, which go through the same `pull`.

### Design

The check runs inside the receive's own D97 stage, before that stage's cancellation-aware rename. There is no second
private directory.

- `marathon_apple::cmd` gains `ReceiveCheck` (a blocking `FnOnce(&Path) -> io::Result<Result<(), String>>`),
  `Received::{Published, NotReceived, Rejected(reason)}` and `FileBridge::receive_checked`. `ArtifactStage` gains
  `publish_checked(check, lost, stopped)`. The delivery must still be valid after the check, by the same test as
  without one: `lost` (the connection, for the rsync bridge) is sampled before the check, as on base, and again
  after it; `stopped` (cancellation) after it and at the rename, as D97's `publish` does. So a stop that comes while
  the check runs ends the receive as it would have just before it: a lost connection is `NotReceived` (D97's "the
  receive did not complete"), a cancellation is `Cancelled`, whatever the check found, a failure to read the file
  included. The order is base's at the rename: a lost connection, then a cancellation, then the check's own result.
  Otherwise a rejected content is discarded with the stage (`Rejected`), a check's I/O error is the receive's error
  (the stage discarded too), and an accepted one goes through D97's `publish`.
- Both staging bridges override `receive_checked`. `JvmFileBridge` (local copy, no connection to lose) runs it in its
  blocking receive. `RsyncFileBridge` runs it in the blocking step that already publishes the stage; the connection
  test it passes as `lost` is base's pre-rename `connection_stop()`.
- The trait's default `receive_checked` receives nothing and fails (`This file bridge cannot check <src> before
  publishing it`). No bridge can inherit a receive that publishes first and checks afterwards; a bridge that does not
  stage cannot offer a checked receive. The test doubles that pull movies (`WorldFileBridge`, the listener tests'
  `Bridge`, `simulator_video_pull`'s `SlowFirstPull`) use `marathon_test_support::apple::staged_receive::
  receive_checked_in_stage`: it receives into a private directory beside the destination, checks there and renames
  only an accepted file. It lives in the test-support crate, which production crates use only as a dev-dependency,
  and blocks its thread, which only a double may do.
- `AppleSimulatorDevice::pull_checked` is `pull_file` with the check. It uses the same transfer measurement, keyed on
  `Published`, so a rejected movie gets no `Transferred …` line, and the same pull-gate handling.
- `crates/marathon-ios/src/listener/movie.rs` `check(path)` is the structural check (below). `video.rs` `pull` passes
  it to `pull_checked`. `deliver` reports `Rejected(reason)` once through `undelivered`, as `Video <src> was not
  delivered: <reason>`. It registers no attachment, publishes no new file (an earlier file at that report path, if any, stays, as for any
  receive that does not publish), and does not remove the worker's copy, which D97 keeps.

Retention. D97 keeps a failed delivery's remote artifact for inspection ("A failed pull leaves its remote artifact
available for inspection"). `deliver` already does this for a receive that did not complete and for a transfer
error. A rejected movie now follows the same rule. After a failed transcode, the kept copy is the original at
`-temp.mp4`, which A19 leaves on the worker anyway. After a successful transcode, it is the transcoded movie, and
the original stays at `-temp.mp4` as A19 already leaves it. D97's line says nothing about the kept copy for the
other failures, so the rejected-movie line does not either. The simulator's next preparation removes the worker's
directory (`remove_remote_directory`), as for every kept artifact.

### The structural check

`movie::check` walks the file's top-level ISO BMFF / QuickTime boxes. It reads box headers only, with a seek per box,
and does not look inside any box. It rejects:

- an empty file (`the recording is empty`);
- a top-level box whose extent (32-bit size, 64-bit `largesize`, or size 0 = to the end of the file) runs past the end
  of the file, and a file that ends inside a box header (`… is not a complete movie (it is cut short)`). This catches
  a faststart movie cut after its `mdat` header: its `moov` is intact, its `mdat` is not;
- no top-level `moov` (`… (it has no moov box)`), or only empty ones (`… (its moov box is empty)`);
- no `mdat` with at least one byte (`… (it has no media data)`). An `mdat` with zero payload is rejected because there
  is nothing to decode, whatever the sample tables say; it is what a writer stopped right after its first header
  leaves. A fragmented MP4 keeps its samples in later `mdat`s (after `moof`s), so one non-empty `mdat` anywhere is
  enough;
- a size smaller than its own header (`the recording is not a movie`). A `uuid` box's header includes its 16-byte
  user type, so a `uuid` box shorter than that is rejected too; a last `uuid` box of size 0 that ends inside its user
  type is cut short.

The inside of the `moov` is not checked (round 4). Rounds 2 and 3 required an `mvhd` and a `trak` directly under it,
which rejected a legal QuickTime movie whose movie header is compressed (`moov` → `cmov` → `dcom` + `cmvd`, the
`mvhd` and `trak`s deflated inside; ffmpeg decodes it). Every failure seen (a 0-byte file, a movie cut short, an
empty `moov` or `mdat`) shows at the top level, so the check stays there and accepts every legal layout.

There is no box-count limit. Round 2 had one (100 000 top-level boxes) and rejected a complete ffmpeg movie with
50 000 one-frame fragments. The work is bounded by the file instead: a box takes at least 8 bytes, and the reader
keeps its 8 KiB buffer across a seek that lands inside it, so a run of small boxes costs one read per 8 KiB and a
large box one seek. The walk reads at most the file once, as the transfer just wrote it, and usually a few headers.

It is a structural check, not a playability guarantee: it does not decode, and it does not cross-check the sample
tables against the media. That would need ffprobe, which costs a worker command per video (or a local ffmpeg that
marathon does not require) and would change the worker-command stream. It runs on the bridge's blocking thread, never
on the core executor, and holds one 8 KiB read buffer. It can accept a file that is boxed but not decodable (for example garbage wrapped in well-formed boxes, or a size-0 `mdat` that runs to a clipped end of file); it targets the empty and cut-off recordings actually seen, and must never reject a legal movie. Movies whose media lives in an external reference (no local `mdat`) are out of scope: simctl and the transcode never produce them.

Witnesses in `movie.rs`: simctl's layout (moov after mdat), ffmpeg's faststart layout, a 64-bit `mdat`, a size-0 last
box, a fragmented layout with an empty first `mdat`, 200 000 one-sample fragments (400 000 top-level boxes, 3.4 MB),
a `moov` holding only a compressed `cmov`, a `moov` with any non-empty payload, and `uuid` boxes with their user type
(32- and 64-bit sizes) pass. An empty file, an unfinalized recording (no moov), a moov cut short, a faststart movie
truncated just after its `mdat` header, a file ending inside a box header, an empty `moov`, no `mdat`, an empty `mdat`,
garbage, sizes below the header and `uuid` boxes without their 16-byte user type are rejected.

### Android and macOS

Android's video listener (`crates/marathon-android/src/artifact_listeners/video.rs`) has no transcode and no
fallback, so it does not have this pattern. macOS has no video recording (`start_video_recording` is unsupported).
Neither was changed. Android publishing a pulled movie without a structural check is listed under
[Incidental bugs](#incidental-bugs) as a candidate, not seen.

### Result files

For a rejected movie, the change publishes no new (empty) file and drops its video attachment, and nothing else. Where
the attachment showed:

- Allure already skipped it, because its reporter drops empty attachment files (`reporters/allure.rs`).
- The per-test JSON (`TestResultJson`, `report/json.rs`) listed it, pointing at the empty file. It no longer does.
- The HTML report linked it, since the file existed. It no longer does.
- JUnit never referenced videos.

This is the case the brief allows: an attachment that pointed at an empty file is dropped. No golden changed: no
scenario records an incomplete movie. The receive's destination is D97's stage as before, so the CLI vendor
differential (`differential_vendors`, which compares the host rsync argv) needs no allowance. Round 1's
`U267_MOVIE_STAGE` allowance in `tools/differential/vendor_transport.py`, its test in `test_evidence.py` and the
`registry.json` hash change are reverted to base.

### Witnesses

`crates/marathon-ios/tests/ios_listeners.rs`, driving `ScreenRecordingListener` with the fake worker and a fake
bridge that writes chosen bytes per remote path (its checked receive is the test-support staged one):

- `a_failed_transcode_publishes_the_original_only_when_it_is_complete_u267`: transcode fails. With an empty original
  and with an unfinished one (moov cut off), there is exactly one `ERROR Video … was not delivered: <reason>`, no
  attachment, no file under the report root, and no `rm -rf` of the worker copy. With a complete original, it is
  published and attached byte for byte, the worker copy is removed, and there is no "not delivered" line.
- `every_pull_of_a_movie_publishes_only_a_complete_one_u267`: the same for transcoding off, a successful transcode,
  and the batch-failure video (no test, so no attachment).
- `a_movie_at_a_report_path_near_the_platform_limit_is_still_delivered_u267` (round 1's P1): a report path whose D97
  staged path is 10 bytes below `PATH_MAX` (1024 on macOS, 4096 on Linux). The test first shows that one more private
  directory level cannot be created there (round 1's layout). Then, through the real `JvmFileBridge`: an unchecked
  receive delivers (the base behaviour for that path), a checked receive of the fixture movie publishes it, and a
  checked receive of an empty movie is rejected with the published movie, the source and no stage left.

`crates/marathon-apple/tests/rsync_file_bridge.rs` `a_checked_receive_publishes_only_what_its_check_accepts_u267`,
through `RsyncFileBridge` with a stand-in rsync: an empty receive is rejected and leaves the earlier file, a non-empty
one replaces it, no `.marathon-artifact-*` directory is left, and each check saw the received file in D97's own stage
(one `.marathon-artifact-*` directory directly beside the destination, under the destination's name). Round 3 adds a
connection loss injected from inside an accepting check (`TransferStop::or_lost`, fired by the check): the receive is
`NotReceived`, as base reports a lost connection before the rename, and the earlier file stays; and a cancellation
fired from inside the check: `Cancelled`, the earlier file stays. Round 4 adds the same two stops fired from a check
that then fails with an I/O error (`Cancelled` and `NotReceived`, not the I/O error), and a failing check with no stop
(the receive's error). No stage is left in any case.

The fake bridges in `listener_support` and `simulator_video_pull.rs` used to write `media` / `video` as the "movie".
They now write the committed fixture `fixtures/fake-worker/apple/media/recording.mp4`, read and never edited. The
fake-worker world and the CLI world already used the real fixtures.

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

Fix, test only (round 3):

- The root is recorded by the process that creates it, right after its `mkdir` succeeds, with its inode. The parent writes a Perl module
  (`ROOT_LEDGER_MODULE`, `U269Ledger.pm`) and an empty lock file into its temporary directory and passes the
  directory to the child (`MRS_D96_ROOT_DIR`). The child's private sshd gives its sessions `PERL5OPT` loading the
  module, and the ledger and lock paths (`SetEnv`), so every command still runs as sshd would run it. The module
  wraps `mkdir`, which `File::Temp` calls when the worker's bootstrap (or its `init` mode) creates
  `/tmp/marathon-supervisor-XXXXXXXXXX`. Under a shared lock on the lock file it calls the real `mkdir` and, only if
  that succeeded, appends `+ <attempt> <root>` to the ledger (the attempt id is the Perl process id with that
  process's own count). `mkdir` fails on a name that exists, so only roots this test's sessions created are ever
  recorded: a root another process owns can never be retired by this test. Earlier rounds recorded the attempt
  before the `mkdir` and retracted it on failure; the fourth review showed a session killed between a failed `mkdir`
  and its retraction would leave another process's root on record, so the orchestrator chose record-after-create:
  the only cost is that a session killed between its successful `mkdir` and the record leaks that one root. The record
  carries the root's inode, and the parent retires a recorded name only while it still holds that inode: a root
  removed and its name reused by another process is skipped (the final review's probe). Other paths are untouched.
- The parent holds a guard, `SupervisorRoots`. After the child process has exited (or from `Drop`, if the parent
  panics before that), it takes the lock file's lock exclusively and keeps it. A root creation under way has then
  finished and is on record, and a later one fails at the module (nothing creates a root after the child is gone,
  but a session could still be running). It then retires each recorded root that exists as production does. It
  checks the root's exact shape (`/tmp/marathon-supervisor-` plus 10 characters) and refuses anything else. It waits
  for an exclusive `flock` on the root's `supervisor.pl`, which every running worker operation holds shared; that is
  the supervisor's retirement lock, and the abandoned commands' owners outlive the child by a moment. Then it runs the
  worker's own `/usr/bin/perl <root>/supervisor.pl retire <root> 60000`, which takes the lock again, removes the root
  and answers `MARATHON_D96_1 retired`. A `busy` answer (an operation that started between the two) waits for the
  lock again. A root with no `supervisor.pl` is one already retired, or one whose bootstrap stopped before installing
  it; its empty directory, if any, is removed.
- Every wait (the ledger lock, each root's lock, each `retire` process, killed if it overruns) runs on its own thread
  against one 60 s deadline for the whole retirement. The retirement runs once: `Drop` retires only if it has not
  started, so a failed retirement is reported, not retried for another 60 s.
- The parent asserts the child's success, then that at least one root was recorded (unless the child skipped for lack
  of sshd or Perl), then that each is gone.
- What the test checks is unchanged: the child runs the same steps on the same connection, and its commands run as
  before. Round 2's `ForceCommand` wrapper is gone.

Coverage, exactly:

- Normal exit and a failing child: retired after the child exits (the test runs this path every time).
- A panic in the parent before the retirement: `Drop` retires.
- The child, or an sshd session, killed with SIGKILL: a root is recorded right after its `mkdir` succeeds, and the
  parent's exclusive lock waits for a creation under way. A session killed between its successful `mkdir` and the
  record (one file append) leaks that root; this is accepted so that a root another process owns is never on record.
  A bootstrap killed after the record and before it installs `supervisor.pl` leaves an empty directory, which is
  removed. A bootstrap killed in the middle of writing
  `supervisor.pl` leaves a script that cannot run `retire`; the test then fails with that error and the root stays.
  Checked by hand: the child run alone with `MRS_D96_ROOT_DIR`, killed with SIGKILL as soon as the ledger had a
  record; the root was recorded and installed, and the lock-then-`retire` sequence removed it.
- Not covered: the parent itself killed with SIGKILL (as for any temporary directory), and the partly written
  `supervisor.pl` above (the test fails and names the root). A child killed with SIGKILL
  also leaves its private sshd listener running (the child's `kill_on_drop` does not run); that is base behaviour
  and outside U269.

Round 1 learned the root from a supervised `ps -o args= -p $PPID` after the probe. Round 2 recorded it from the
bootstrap's answer in a `ForceCommand` wrapper, which left a window between the answer and the record, and retired
with an unbounded `output()` and a `Drop` that could retry for another 60 s. Round 3 closes both.

Checked: a listing of `/tmp/marathon-supervisor-*` before and after one `ssh_supervisor` run showed 1 new root on base
and 0 with the fix. The other tests that run the worker script were checked the same way, with a before/after listing
around the full test runs of the crates below. The tests that create roots of their own (`d98_root`, `x111_root`,
the D96 witnesses) use `tempfile::TempDir`. `failed_worker_bootstraps_fall_back_d96` removes the root its `csh` run
creates right after reading it. The fake-worker and CLI worlds use a fake filesystem.

Not deleted by this WP: roots left on a development machine by earlier runs. The pattern is
`/tmp/marathon-supervisor-??????????` (directories owned by the developer, each with `supervisor.pl` and a few
`*.receipt` files, or only a receipt). At the start of round 2 there were none left on the development Mac (someone
had cleaned them up), and there were none after the round-2 runs.

## Review round 1 (independent review) and what changed

| Finding | Verified | Change |
| --- | --- | --- |
| P1: the movie stage was a second private directory around D97's, so a report path that fits D97's stage could run past the platform path limit and a complete movie was not delivered | Yes: the round-1 file path was the report path plus two 26-byte stage levels | No second stage. The check runs in D97's own stage, before its rename (`FileBridge::receive_checked`, `ArtifactStage::publish_checked`). Witness: the near-limit test above. |
| P2: a `moov` found first was enough, so a faststart movie truncated after its `mdat` header, or an empty `moov`, passed | Yes, by reading round 1's `check` (it returned at the first fitting `moov`) | Structural check over every top-level box, `moov` contents and a non-empty `mdat`; box-count caps. |
| P2: the worker copy was removed after reporting non-delivery | Yes; D97 keeps a failed pull's remote artifact | A rejected movie keeps its worker copy, as the other undelivered cases do. D97's line does not mention the kept copy, so neither does this one. |
| P2: the `U267_MOVIE_STAGE` differential allowance was loose | Yes | Removed with the second stage; `vendor_transport.py`, `test_evidence.py` and `registry.json` are as on base. The batch-failure destination `batch.batch` is not normalized by D97's movie rule on base either; no recorded scenario has one (see Incidental bugs). |
| P2: the U269 root was recorded only after another command, and cleanup used a process listing | Yes | Recorded by the sshd as the bootstrap answers; retired under the supervisor's lock by the worker's own `retire`, from a guard. |
| P3: the new witnesses' hang guards were 10 s and 30 s | Yes | 60 s (`HANG_GUARD` in the ADB witness; the U269 guard). The shared 10 s `TIMEOUT` of the ADB server tests, used by base tests and the client helpers, was left alone. |

## Review round 2 (independent re-check) and what changed

| Finding | Verified | Change |
| --- | --- | --- |
| P1: the 100 000-box cap rejected a complete ffmpeg movie with 50 000 one-frame fragments | Yes: two top-level boxes per fragment plus the headers is over 100 000 | No box-count limit (either cap). The walk's cost is bounded by the file: buffered seeks, at most one pass. Witness: `many_fragments_or_tracks_are_no_reason_to_reject_a_movie` (200 000 fragments, 10 000 tracks), which fails on round 2's code. |
| P2: the rsync bridge sampled the connection before the check, so a disconnect during an accepting check could still publish | Yes: `connection_stop()` ran once, before `publish_checked` | `ArtifactStage::publish_checked(check, lost, stopped)` samples the connection before and after the check and the cancellation after it and at the rename; the rsync bridge passes base's `connection_stop()`. Witness: the loss and the cancellation fired from inside the check in `a_checked_receive_publishes_only_what_its_check_accepts_u267`; the loss case fails on round 2's code. |
| P2: the default `receive_checked` published, then checked and deleted | Yes; only test doubles used it | The default refuses (an error, nothing received). The doubles use the staged helper in `marathon-test-support`. |
| P2 (test): the U269 root could be created and not recorded (the wrapper killed between the bootstrap's answer and its record); retirement used an unbounded `output()`, and `Drop` could retry for another 60 s | Yes | The root is recorded by the process that creates it (since round 4's follow-up: right after its `mkdir` succeeds, with its inode), under a lock the parent closes before reading the ledger. One 60 s guard for every wait; `retire` runs once. See [U269](#u269-the-loopback-sshd-test-removes-its-worker-root). |
| P3: a `uuid` box without its 16-byte user type passed | Yes | Its header includes the user type; witness `a_uuid_box_needs_its_user_type`. |
| P3 docs: the changelog and U267 said the report has no file; the U269 coverage was overstated | Yes: an earlier file at the report path stays | "No new file"; the U269 section lists exactly what is covered. |

## Review round 3 (third re-check) and what changed

| Finding | Verified | Change |
| --- | --- | --- |
| P1: the check required `mvhd` and `trak` directly under `moov`, so a legal QuickTime movie with a compressed `cmov` was rejected | Yes, by reading round 3's `check` | Orchestrator decision: the check stays at the top level (well-formed boxes within the file, a non-empty `moov`, a non-empty `mdat`). Witness `the_inside_of_a_moov_is_not_checked` (a `cmov` movie passes), which fails on round 3's code (`Err(EmptyMovieBox)`). The witnesses that pinned the inner rule (`mvhd` or `trak` alone, a child overrunning its `moov`, 10 000 tracks) are removed; the reason for an empty `moov` is now `its moov box is empty`. |
| P2: a check's I/O error returned at once, so a cancellation or connection loss during the check was reported as that error | Yes: `check(..).map_err(io_error)?` before the post-check samples | After the check, whatever its result: a lost connection (`NotReceived`), then a cancellation (`Cancelled`), then the check's own result. The brief put cancellation first; base's pre-rename order is the connection first (`connection_stop()`, then `publish`'s cancellation sample), and the code keeps base's. Witness in `a_checked_receive_publishes_only_what_its_check_accepts_u267`, which fails on round 3's code (the I/O error where `Cancelled` is expected). |
| P2 (test): a failed `mkdir` retracted every record of its name, so `+,+,-` for one name left the first attempt's root untracked | Yes, by reading the ledger parse | Records carry an attempt id; a retraction removes only its own attempt, and a malformed or unmatched line fails the test. Not "never drop": a name taken by another process would then be retired by this test. Witness `a_failed_root_attempt_keeps_another_attempts_root_u269`, which fails with round 3's parse. |
| P3 docs: the U269 row said only a parent SIGKILL leaves roots | Yes: a partly written `supervisor.pl` also does | The row and the coverage list say both. |

## Revert checks

Round 1: a scratch worktree at `77a2e051` with this branch's test files copied in (`ios_listeners.rs`,
`listener_support/mod.rs`, `simulator_video_pull.rs`, `crates/marathon-adb/src/server/tests.rs`), product code
unchanged. The scratch worktree was removed afterwards.

- U267: both listener witnesses failed: no line where one `ERROR Video … was not delivered: the recording is empty`
  is expected. The other 42 `ios_listeners` tests passed with the new support code, so the fakes' switch to a real
  fixture is neutral on base.
- U268: `a_client_resetting_between_requests_is_not_a_warning_u268` failed with
  `connection 1: Connection reset by peer (os error 54)` after `host:transport:d1`, the CI failure's shape.
- U269: a listing of `/tmp/marathon-supervisor-*` before and after one base run of `ssh_supervisor` showed one new
  root.

Round 2: no scratch run. The new bridge witnesses use `receive_checked`, which base does not have. The near-limit
witness checks its own premise at run time: one more private directory level cannot be created at its path, which is
round 1's layout. The listener witnesses keep round 1's assertions and add one (the worker copy is kept exactly when
the movie is not published), so they still fail on base. The movie checks that are new in round 2 (a faststart movie
truncated after its `mdat` header, an empty `moov`) are cases round 1's check accepted, by reading its code.

## What was run

Round 4, on this Mac:

- Revert check in a scratch worktree at round 3's head (`30cf7343`) with round 4's tests copied in (the `movie.rs`
  tests, `rsync_file_bridge.rs`, and `ssh_supervisor.rs` with round 3's ledger rule), product code unchanged:
  `the_inside_of_a_moov_is_not_checked` failed (`Err(EmptyMovieBox)`), `a_checked_receive_publishes_only_what_its_check_accepts_u267`
  failed (the I/O error where `Cancelled` is expected) and `a_failed_root_attempt_keeps_another_attempts_root_u269`
  failed (no root recorded). The scratch worktree was removed.
- `cargo fmt --check`; `cargo clippy -p marathon-ios -p marathon-apple -p marathon-test-support --all-targets -- -D
  warnings`: clean. `cargo test --no-fail-fast -p marathon-ios -p marathon-apple -p marathon-test-support`: 1192
  passed, 0 failed, 26 ignored. `cargo test --no-fail-fast -p marathon-adb -p marathon-android -p marathon-macos -p
  marathon-cli -p fake-adb -p fake-worker`: 817 passed, 0 failed, 8 ignored (the CLI differential and iOS suites
  included). `python3 -I tools/differential/test_evidence.py` and `make_registry.py --check`: OK.
- Stress of the changed witnesses (the `movie.rs` tests, the rsync checked-receive witness, the ledger witness and the
  U269 loopback test): 50 rounds, 0 failures; 20 rounds under CPU load (18 `yes` processes), 0 failures.
  `/tmp/marathon-supervisor-*` had 0 entries before and after.

Round 3, on the final code:

- `cargo fmt --check`, and `cargo clippy -p marathon-apple -p marathon-ios -p marathon-test-support --all-targets --
  -D warnings`: clean.
- `cargo test --no-fail-fast -p marathon-ios -p marathon-apple -p marathon-test-support`: 1190 passed, 0 failed, 26
  ignored. `cargo test --no-fail-fast -p marathon-adb -p marathon-android -p marathon-macos -p marathon-cli -p fake-adb
  -p fake-worker`: 817 passed, 0 failed, 8 ignored, including `marathon-cli`'s `differential_vendors`,
  `config_differential`, `ios_production` and `ios_stalled_send`. `/tmp/marathon-supervisor-*` had 0 entries before
  and 0 after.
- `python3 -I tools/differential/test_evidence.py`: OK, after `cargo build -q -p snakeyaml --example yaml_tree`.
  `python3 -I tools/differential/make_registry.py --check`: OK.
- Revert check in a scratch worktree at round 2's head (`9f8fa19d`) with round 3's `movie.rs` tests and
  `rsync_file_bridge.rs` copied in, product code unchanged: `many_fragments_or_tracks_are_no_reason_to_reject_a_movie`
  failed (`Err(NotAMovie)`), `a_uuid_box_needs_its_user_type` failed (`Ok(())` for a `uuid` box without user type),
  and `a_checked_receive_publishes_only_what_its_check_accepts_u267` failed (`Ok(Published)` where a connection lost
  during the check must give `Ok(NotReceived)`). The scratch worktree was removed.
- Stress, each round running every new or changed witness: the `movie.rs` unit tests, the `ios_listeners` U267
  witnesses, `simulator_video_pull`, the rsync checked-receive witness and the U269 loopback test. 50 rounds: 0
  failures. 20 rounds under CPU load (18 `yes` processes): 0 failures. No supervisor root left afterwards.
- By hand: the loopback child run alone with `MRS_D96_ROOT_DIR` set and killed with SIGKILL as soon as the ledger
  had a record. The root was recorded and installed, and the lock-then-`retire` sequence removed it. (The killed
  child's sshd listener, left running as on base, was stopped by hand.)

Round 2 ran the same suites (2006 passed) and the same stress counts. Round 1 also ran the installer test 200 times,
then 150 under load (0 failures), and the U268 witness 60 + 60.

Not run (the orchestrator's gates): `scripts/check.sh`, `scripts/remote-check.sh`, `cargo test --workspace`. The
Linux build of `ssh_supervisor.rs` was not compiled here; `SupervisorRoots` is `#[cfg(target_os = "macos")]` like the
tests that use it.

## Not verified

- No real simulator run: the U267 fix was not exercised against simctl's own short-recording failure. The structural
  check was exercised on ffmpeg-made movies (the fixtures) and synthetic layouts only.
- The rsync bridge's checked receive was exercised with a stand-in rsync, not with a real worker.
- U268's CI flake was reproduced locally under load. The CI failure's own transcript was not available, so a reset
  inside the push itself (a sync transfer, which still warns by design) was not ruled out for that one CI failure.
  The push of the small fixture APK takes milliseconds, against the 500 ms timeout.
- The test doubles' staged receive (`receive_checked_in_stage`) runs its check and rename inline on the test's
  executor. It is test support only; both production bridges check on their blocking threads.
- A real ffmpeg movie with tens of thousands of fragments was not run through the check here; the witness is
  synthetic boxes of the same layout (the re-check measured the ffmpeg case).

## Incidental bugs

- Android video (`crates/marathon-android/src/artifact_listeners/video.rs`): a pulled movie is published without a
  structural check. No empty Android movie has been seen; `screenrecord` finalizes on SIGINT. Candidate only, not
  changed.
- The batch-failure iOS video is published as `report/video/<pool>/<serial>/batch.batch` (no `.mp4` extension), as
  the listener tests show. Pre-existing, not changed. Not checked against the Kotlin marathon.
