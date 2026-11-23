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
  `publish_checked`: it runs the check on the staged content and, if the check rejects it, discards the stage and
  returns `Rejected`. Otherwise it is D97's `publish`: the stop is sampled at commit, then the rename.
- Both staging bridges override `receive_checked`. `JvmFileBridge` (local copy) runs it in its blocking receive.
  `RsyncFileBridge` runs it in the blocking step that already publishes the stage, after the connection-stop check.
  The default implementation is for bridges that write in place, which only test doubles do. It checks the file where
  it landed, inline, and removes it if rejected.
- `AppleSimulatorDevice::pull_checked` is `pull_file` with the check. It uses the same transfer measurement, keyed on
  `Published`, so a rejected movie gets no `Transferred …` line, and the same pull-gate handling.
- `crates/marathon-ios/src/listener/movie.rs` `check(path)` is the structural check (below). `video.rs` `pull` passes
  it to `pull_checked`. `deliver` reports `Rejected(reason)` once through `undelivered`, as `Video <src> was not
  delivered: <reason>`. It registers no attachment, publishes no file, and does not remove the worker's copy.

Retention. D97 keeps a failed delivery's remote artifact for inspection ("A failed pull leaves its remote artifact
available for inspection"). `deliver` already does this for a receive that did not complete and for a transfer
error. A rejected movie now follows the same rule. After a failed transcode, the kept copy is the original at
`-temp.mp4`, which A19 leaves on the worker anyway. After a successful transcode, it is the transcoded movie, and
the original stays at `-temp.mp4` as A19 already leaves it. D97's line says nothing about the kept copy for the
other failures, so the rejected-movie line does not either. The simulator's next preparation removes the worker's
directory (`remove_remote_directory`), as for every kept artifact.

### The structural check

`movie::check` walks the file's top-level ISO BMFF / QuickTime boxes and then the children of the first `moov`. It
reads box headers only, with a seek per box. It rejects:

- an empty file (`the recording is empty`);
- a box whose extent (32-bit size, 64-bit `largesize`, or size 0 = to the end of the enclosing extent) runs past the
  end of the file or of its `moov`, and a file or `moov` that ends inside a box header (`… is not a complete movie (it
  is cut short)`). This catches a faststart movie cut after its `mdat` header: its `moov` is intact, its `mdat` is not;
- no top-level `moov` (`… (it has no moov box)`);
- a `moov` without an `mvhd` or without any `trak` (`… (its moov box has no track)`);
- no `mdat` with at least one byte (`… (it has no media data)`). An `mdat` with zero payload is rejected because there
  is nothing to decode, whatever the sample tables say; it is what a writer stopped right after its first header
  leaves. A fragmented MP4 keeps its samples in later `mdat`s, so one non-empty `mdat` anywhere is enough;
- a size smaller than its own header, or more than 100 000 top-level boxes or 4 096 boxes inside `moov`
  (`the recording is not a movie`). The caps bound the work; a fragmented MP4 has two top-level boxes per fragment,
  and simctl and ffmpeg write a handful.

It is a structural check, not a playability guarantee: it does not decode, and it does not cross-check the sample
tables against the media. That would need ffprobe, which costs a worker command per video (or a local ffmpeg that
marathon does not require) and would change the worker-command stream. It runs on the bridge's blocking thread, never
on the core executor, and holds one 8 KiB read buffer.

Witnesses in `movie.rs`: simctl's layout (moov after mdat), ffmpeg's faststart layout, a 64-bit `mdat`, a size-0 last
box, a fragmented layout with an empty first `mdat` pass. An empty file, an unfinalized recording (no moov), a moov
cut short, a faststart movie truncated just after its `mdat` header, a file ending inside a box header, an empty
`moov`, a `moov` with only `mvhd` or only `trak`, a child overrunning its `moov`, no `mdat`, an empty `mdat`, garbage,
sizes below the header and both caps are rejected.

### Android and macOS

Android's video listener (`crates/marathon-android/src/artifact_listeners/video.rs`) has no transcode and no
fallback, so it does not have this pattern. macOS has no video recording (`start_video_recording` is unsupported).
Neither was changed. Android publishing a pulled movie without a structural check is listed under
[Incidental bugs](#incidental-bugs) as a candidate, not seen.

### Result files

For a rejected movie, the change removes the published empty file and its video attachment, and nothing else. Where
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
bridge that writes chosen bytes per remote path (so the default `receive_checked`):

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
(one `.marathon-artifact-*` directory directly beside the destination, under the destination's name).

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

Fix, test only:

- The root is recorded where it is created. The child writes its private sshd's configuration, and when the parent
  sets `MRS_D96_ROOT_FILE` it adds a `ForceCommand` script. For the supervisor bootstrap (the command starting with
  `/usr/bin/perl -MMIME::Base64 -e `) the script runs it, appends the root from its `MARATHON_D96_1 <root>` answer to
  that file and passes the answer and exit status on. Every other command is `exec /bin/sh -c "$SSH_ORIGINAL_COMMAND"`,
  so the process that runs it is the same command sshd would have started through the login shell. A root is
  therefore recorded whatever the child does after its bootstrap. A child killed with SIGKILL right after the
  bootstrap still had its root recorded (checked by hand; see [What was run](#what-was-run)).
- The parent holds a guard, `SupervisorRoots`, over that file. After the child process has exited it retires every
  recorded root as production does. It checks the root's exact shape (`/tmp/marathon-supervisor-` plus 10 characters)
  and refuses anything else. It waits for an exclusive `flock` on the root's `supervisor.pl`, which every running
  worker operation holds shared; that is the supervisor's retirement lock. The abandoned commands' owners outlive the
  child by a moment and still write their receipts, so this wait is needed. Then it runs the worker's own
  `/usr/bin/perl <root>/supervisor.pl retire <root> 60000`, which takes the lock again, removes the root and answers
  `MARATHON_D96_1 retired`. A `busy` answer (an operation that started between the two) waits for the lock again. The
  wait is a 60 s hang guard. A root already retired is skipped. If the parent panics before retiring, `Drop` retires.
- The parent asserts the child's success, then that at least one root was recorded (unless the child skipped for lack
  of sshd or Perl), then that each is gone.
- What the test checks is unchanged: the child runs the same steps on the same connection. Only the sshd's command
  dispatch changed, as above.

Round 1 learned the root from a supervised `ps -o args= -p $PPID` after the probe and removed it once `pgrep` found
no process running the root's script. The review found a leak window (a failure between the bootstrap and that
command) and that a process listing is not the supervisor's own lock. Both are gone.

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

On this Mac (macOS, 18 cores), round 2, on the final code:

- `cargo fmt --check`, and `cargo clippy -p marathon-core -p marathon-apple -p marathon-ios -p marathon-adb
  --all-targets -- -D warnings`: clean.
- `cargo test --no-fail-fast -p marathon-adb -p marathon-android -p marathon-apple -p marathon-ios -p marathon-macos
  -p marathon-cli -p fake-adb -p fake-worker -p marathon-test-support`: 2006 passed, 0 failed, 34 ignored. This
  includes `marathon-cli`'s `differential_vendors`, `differential_local`, `differential_parse` and `ios_production`,
  with no U267 allowance. `/tmp/marathon-supervisor-*` had 0 entries before and 0 after.
- `python3 -I tools/differential/test_evidence.py` (117 tests): OK, after `cargo build -q -p snakeyaml --example
  yaml_tree`. `python3 -I tools/differential/make_registry.py --check`: OK.
- Stress, each round running every new or changed witness: the three U267 listener witnesses, the `movie.rs` unit
  tests, the rsync checked-receive witness, the U269 loopback test and the U268 ADB witness. 50 rounds: 0 failures. 20
  rounds under CPU load (18 `yes` processes): 0 failures. No supervisor root left afterwards.
- By hand: the loopback child run alone with `MRS_D96_ROOT_FILE` set and killed with SIGKILL as soon as the file was
  written. Its root was recorded, and the lock-then-`retire` sequence removed it.

Round 1 also ran the installer test 200 times, then 150 under load (0 failures), and the U268 witness 60 + 60.

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
- The default `FileBridge::receive_checked` runs its check inline. Only test doubles use it: both production bridges
  override it and check on their blocking threads.

## Incidental bugs

- Android video (`crates/marathon-android/src/artifact_listeners/video.rs`): a pulled movie is published without a
  structural check. No empty Android movie has been seen; `screenrecord` finalizes on SIGINT. Candidate only, not
  changed.
- The batch-failure iOS video is published as `report/video/<pool>/<serial>/batch.batch` (no `.mp4` extension), as
  the listener tests show. Pre-existing, not changed. Not checked against the Kotlin marathon.
