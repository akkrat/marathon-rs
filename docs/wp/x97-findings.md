# WP x97 findings: release documentation for 0.15.0-rc.1

Documentation only, based on `main` at f90a6619, the commit the release candidate is cut from. It ships D82–D88 and
D90–D97 (D89 was reserved and not used) and the U rows fixed with them. No code, golden, fixture or recording changed.

## What was added or changed

- `CHANGELOG.md`: `## Unreleased` became `## 0.15.0-rc.1`, structured like 0.14.0: an intro naming the contracts that
  change, "Check before you upgrade", the changes grouped by area (runs, queue and supervision; process ownership and
  signals; Android; iOS and macOS; artifacts and reports; analytics), Evidence and Known limits. Every Unreleased bullet
  is kept, with its D refs, and each gets the U rows that `docs/found-bugs.md` records for it (U204–U213, U215, U218,
  U220–U230, U235–U250, and U172 for D90). Two bullets were split by platform (D88; D97's artifact and signal parts) and
  D96's six bullets were grouped by where they act. The `## 0.14.0` section and older are unchanged.
- `docs/deviations.md`: the intro says D82–D88 and D90–D97 ship in 0.15.0-rc.1; the range text (D1–D97, D73 and D89
  reserved) was already right. Five "Amended by" notes, in the style of D31's existing D91 note, for older text that a
  later deviation contradicts (see the disagreements below): D22 and D41 (by D85), D31 (by D96), D91's limits (by D96
  and D93) and D92's staging fallback (by D97).
- `docs/migration.md`: the post-0.14.0 notes now say they apply when moving from 0.14.0 to 0.15.0-rc.1 (and from
  Kotlin); "comes after 0.14.0" became "is new in 0.15.0-rc.1" in all 14 places; D79 joined the no-action list; the D22
  bullet under "Other Android behaviour changes" names D85; the prerequisites say `sshpass` is needed only below
  OpenSSH 8.4 and that stock Perl on Apple workers is optional and recommended.
- `README.md`: 0.14.0 stays the current release; 0.15.0-rc.1 is named as a release candidate with run-supervision,
  process-ownership and device-handling fixes (D82–D88, D90–D97), real runs of earlier commits, and runs of its own
  binary pending. The sentence on real hosts now says it was 0.14.0's release candidates.
- `docs/releasing.md`: release readiness says `sshpass` is needed only with an SSH client older than OpenSSH 8.4.

### Coverage of D82–D97 in the migration notes

Checked with a script over "Changes that need action": every D1–D97 is in the table, the no-action list or the
own-action list, except D73 and D89 (unused). D79 was in none of them (missing since 0.14.0) and is now in the
no-action list, as its own note says. For this batch: D82, D83, D84, D88, D90–D95 and D97 are in the no-action list
(D82 also in "Other Android behaviour changes", the others with a note each); D85, D86 and D96 are table rows; D87 is a
table row and in the own-action list. That matches each entry's **Migration action**. D85, D86 and D96 are table rows
without being in the own-action list, as are older rows such as D21 and D31; the list was left as it is.

## "Check before you upgrade", verbatim

- **Process groups and the reaper (D96):** every local tool marathon starts (rsync, ssh, adb clients, local xcodebuild
  and the like) runs in a process group of its own, so a signal sent to marathon's process group reaches marathon,
  which runs its bounded cleanup and then ends every tool it registered. A run-scoped reaper ends them even if marathon
  is killed. It shows as a second `marathon` process with the same arguments until the controller exits, and ignores
  INT, TERM and HUP; scripts that count processes or use `pgrep`, `pidof` or `killall` see both, so target the
  controller's PID. Background work a tool started in its group ends with it: keep a command session open until such
  work has finished. With `stty tostop`, a tool writing to the inherited terminal can stop, and so can SSH if it prompts
  on `/dev/tty` for a password or a key passphrase: use noninteractive SSH authentication and redirect inherited
  terminal output. ADB servers keep their own lifetime.
- **SIGHUP (D96, D97):** a hang-up now runs the same finish as SIGTERM and exits with 129, where it used to end marathon
  at once without reports or cleanup. The first INT, TERM or HUP seals the devices and starts the reports; cleanup runs
  within fixed cutoffs from the signal and hands over to the exit by 24 s, with up to 1 s more for console output (see
  the signal finish below). A second signal exits at once.
- **Password SSH (D96):** from 0.15.0-rc.1, password-based rsync uses OpenSSH's askpass on OpenSSH 8.4 or newer, which
  keeps rsync and its SSH client together in a private process group that marathon owns. `sshpass` is needed only with
  an older or unrecognized SSH client; marathon then logs one warning per run, and its cleanup covers only the processes
  it has seen.
- **Apple SSH workers (D96, D90):** stock `/usr/bin/perl` with its core modules on the worker is optional and
  recommended. With it, worker commands, remote rsync senders and video recorders run under a worker-side supervisor
  that ends them when their lease runs out without a renewal, even if marathon was killed, and keeps a small private
  directory under `/tmp` while the connection lasts. The lease is 79 s by default from admission or the last successful
  renewal; a renewal already in flight can extend it once, a recorder then gets up to 3 s more to finalize and stop, and a
  frozen worker acts once it runs again. Without usable Perl, marathon logs one WARN per worker address and keeps the
  previous command paths. An SSH forced command or filter must allow the supervisor's bootstrap, the `control-stream`
  command with stdin intact, token control requests, supervisor retirement and the detached video launch, and must
  `exec` the original command: a filter that stays in front of the wrapper can turn its failure into status 137, which
  marathon can read as a confirmed exit. Without Perl, allow the legacy video launcher, pidfile and PID-based stop
  commands. With Perl, marathon also keeps a separate SSH control connection to each worker (D96); with
  `shareWorkerConnection: true`, a worker can see one replacement data connection after a session-capacity refusal
  that follows an abandoned session (D90).
- **Only complete artifacts are published (D97):** a downloaded artifact (a video, screen capture, xcresult, log,
  coverage or file-sync file) appears at its final path only once it is verified complete: on Android, adb's DONE, a
  completed write and a byte count that agrees with the size STAT reported (its low 32 bits with legacy STAT), so a file
  that grows during its pull, such as a live app file collected by file sync, is not delivered; for rsync, a successful
  exit and confirmed cleanup, so a receive whose output pipes are still open after its group's cleanup is not delivered.
  A failed, cancelled or timed-out download leaves no partial artifact at the final path and keeps any earlier output;
  each artifact path it attempted is logged once as not delivered, and the reports have no attachment for it. On a
  filesystem that rejects rename flags, replacing an Apple directory can leave the final path absent for a moment, and
  if a failed publication cannot restore the earlier tree, it is kept in a named backup. On a signal, eligible artifacts
  drain until 12 s after it. A hard kill can leave `.marathon-artifact-*` or Android pull staging directories behind.
- **Retry placement and copy order (D82, D83):** a retry from an Android device goes to another device of the pool
  while other tests are queued, and the copies of different tests (flakiness copies, `count` repeats, a test
  discovered twice) take turns in the queue. A test can need fewer attempts and a run fewer executions; `raw.json` rows
  and their devices change; batches change, so `fixed-size` batching can start more or fewer instrumentations; and the `am instrument -e
  class` and xcodebuild `-only-testing` lists change in number, order and grouping. A test whose outcome depends on the
  device can get another verdict, and with `ignoreFailures: false` the run another exit status. Update snapshots of
  worker commands or batches for runs with retries or copies.
- **Android farms (D85, D88):** marathon reads `/proc/sys/kernel/random/boot_id` and writes, reads and removes a
  `.marathon-ready-<uuid>` marker under `/data/local/tmp`; allow them if you filter adb shell commands. During an adb
  outage or a brief `offline`, a device whose boot ID changed, or whose marker is confirmed missing or different, is set
  up again; if the marker's creation could not be confirmed, the boot ID alone is checked, and without readable evidence
  of a reset ordinary recovery goes on. A still-listed device gets one more setup after its preparation has used its
  retries. Android device monitoring that ends with an unexpected error or a panic (not the provider's own termination)
  now fails the run with the monitor's cause (exit 1, 0 with `ignoreFailures: true`) where the run went on without
  device events; a failing analytics call for a device no longer ends it.
- **What a stuck device or tool costs now (D87, D91–D95):** with the defaults,
  - a device that keeps failing slowly to get ready starts no new retry, and an iOS simulator no new replacement, after
    one preparation deadline; work already running can finish. It leaves in about 9 to 12 minutes for an iOS simulator,
    19 for an Android device (about one more if D85 sets it up once more) and 14 for a Mac (scenario estimates), where it
    took up to 33, 96 and 30, with a WARN saying it is not prepared within its preparation time. Raise
    `deviceInitializationTimeoutMillis` to give slow devices longer (D87);
  - the runner waits at most 40 s for a stopped rsync transfer to an Apple worker to be cleaned up and confirmed. If
    that fails, an ERROR names the worker and rsync's pid, nothing more is sent into that exact destination in the run,
    and a device that needs it leaves its pool (D91);
  - a stopping Android device has 60 s for its in-flight work and cleanup, 65 s in the Scheduler's wait (D92);
  - when a run stops after a failure, each sealed iOS simulator and Mac has one 70 s window for its work and its
    disposal together. The Scheduler waits up to 75 s for the devices, and the device waits take up to 80 s in all,
    apart from the iOS artifact drain, where they could take 145 s. A device whose work is not confirmed to have ended
    gets one WARN naming it and no on-dispose lifecycle, and its lease is quarantined: the connection stays open and
    takes no new device until the run ends, while devices already sharing it can go on using it. Look at that worker
    before the next run (D93);
  - pool and queue drains share 65 s, with one WARN per stalled pool or queue, and a cancelled device subscription has
    its own 65 s and warns if it stalls; committed results are kept, but work a stalled handler has not committed can
    be lost. iOS artifacts get 30 s to drain and 40 s for cancellation after an internal failure or owner cancellation,
    and 300 s and 60 s otherwise (it was 300 s and up to 120 s), and the log names bundles left undelivered (D94);
  - the last Apple worker lease has up to 40 s to disconnect, clipped to the device's or provider's deadline; the history
    backend 30 s to close, after a separate 3 s refresh join; the Android provider 65 s and the iOS provider 70 s after
    its artifacts. An expired deadline logs a warning and the finish goes on. In an ordinary finish the reports always
    finish writing, and one that takes longer than a minute logs the active reporter (D95); after a signal, D97's
    cutoffs apply.
- **A failing run's own machinery (D84):** a run whose queue, pool, device listener or run task fails or panics now
  ends with exit 1 (0 with `ignoreFailures: true`), writes its reports and logs `Marathon run failed` with the first
  cause, where it used to hang until killed or report no cause.
- **Metrics (D86):** Graphite and InfluxDB writes go through a bounded queue and can be dropped. Use local reports for
  result counts; if a dashboard has gaps or a history-based strategy changes, look for `The analytics backend is
  dropping test metric events`, the rejection count or `The analytics backend did not close` in the run log.

## Known limits: what changed from 0.14.0

Each 0.14.0 line was checked against `docs/found-bugs.md`, `docs/known-kotlin-bugs.md`, the deviations guide and the
x80–x96 findings, and against the code where that was cheap.

| 0.14.0 line | Now | Why |
|---|---|---|
| Static Linux binary: "both 0.14.0 release candidates ran on real emulators, rc.2 also on a CI farm" | rewritten | A CI build of an earlier commit of this rc ran on real emulators; the farm runs were 0.14.0-rc.2's. |
| An emulator restarted during an adb outage repeats failing batches (D22) | rewritten | D85 (U207) sets it up again when its boot ID changed or its marker is gone; what remains is a restart that keeps both, and a snapshot rollback that keeps the boot ID when the marker write was not confirmed (D85's own text). |
| A worker whose SSH server ignores signals can keep xcodebuild; the KILL warning (D30, U183) | rewritten | D96 (U230): with stock Perl the worker's supervisor ends the group, at the latest at lease expiry (79 s by default), also after a SIGKILL of marathon, as the real-worker runs showed. Without Perl the old limit stands. |
| A failed readiness probe costs 26 to 33 minutes (D38) | rewritten | D87 (U209): about 9 to 12 minutes, at most about 14.5. The last-device part (D48's 180 s) is unchanged, as D87 says. |
| A failed xcodebuild close retires its simulator; the pool waits (D48), then ends (D30) | kept, extended | Still true; since D93 such a disposal is unconfirmed: no on-dispose lifecycle and a quarantined connection. |
| An Android device out of its pool but in adb's list returns only if the list changes (D48) | rewritten | D85 gives a still-listed device one more setup after a failed preparation; D49's limit does not trigger it (D85's text). |
| A failing emulator takes its test back at once and uses up the quota (C90) | replaced | Fixed by D82 (U204). The remaining case is x80's follow-up: a device that fails every test still uses one attempt of each test it takes and takes its own retries when nothing else is queued. |
| Copies of one test are queued together (D60) | removed | Fixed by D83 (U205). |
| Coverage on a lost Android device: logged missing, and its cleanup failed, at ERROR (D81) | rewritten | Since the D97 commits the listener returns before the removal when the file is missing (`artifact_listeners/coverage.rs`, diff against 3e53e6a4), so only the "missing" ERROR is left; x96 pins one coverage error. |
| No Windows build; ad-hoc signing; `xctest` library (D20); U103; no shadow results; Graphite setup (D71); D75; D25; A20; D70's listener files | kept | Unchanged by this batch; U103 is still open. |

Added, each from the brief or a findings document:

- Xcode's shared service hosts (`DTServiceHub`) can outlive a run, on purpose (D96; both real Apple runs saw them).
- A D88 monitor failure was not provoked on a real device (x96 Android acceptance skipped it; tests with fake adb only).
- Zero-byte Android videos in a real run, judged a recording issue that predates this release; D97 delivers a complete
  zero-byte file (x96 Android acceptance, D97).
- The password and askpass path, D90's rotation, and partitions or delayed control replies were not exercised on a real
  worker (both Apple acceptance reports; x95f's U248–U250 came after both runs).
- Under `sshpass`, cleanup covers only processes a listing saw (D91's limit, narrowed by D96; U228).
- D96's spawn-to-registration gap, the 4,096-entry reaper table, and the supervisor directory left on a worker after an
  abrupt loss of both paths.
- A quarantined connection stays open until exit and keeps one of the two generation slots (D93, D90).
- Rename-flag fallback race and the uncuttable filesystem call (D97, x94, x96).
- Android remote state after a forced close (D92, x91 "Remote state after forced closure").
- Two consecutive adb outages can make D48's wait count from the later one (x83 incidental bug).
- Android's GIF listener can attach a GIF after a failed capture (x96 incidental bug).

Not carried as known limits:

- x82's incidental bugs (a panic in retained Apple work reported as `channel closed`; Android logcat not restarted after
  a panic in its task; macOS logging a non-fatal disposal error at DEBUG). Each needs a panic or an internal error to
  show, and whether later WPs changed them was not checked.
- x86's known limit that a wedged iOS send outlives the preparation's 70 s: fixed by D91.
- x82's "Android declares no cancelled-call bound": fixed by D92.
- x84's note that real analytics services were outside its verification: the Android acceptance held an InfluxDB
  response (D86); no limit is listed for it.
- No `found-bugs.md` row in U204–U250 is open. U214, U216, U217, U219 and U231–U234 are unused (x90 and x92 say "no
  U217", "no U219"). U202 and U203, open test flakes from 0.14.0, involve no product code and are not listed again.

## Disagreements found, and what was done

1. **D84's Unreleased bullet** said the run waits for each device's cancelled work "without a limit on Android". D92
   gave Android 60 s (65 s in the Scheduler); the D84 entry and the migration note already say 65 s. The changelog bullet
   now says 65 s.
2. **D92's staging fallback.** The D92 entry said that if staging is unavailable, pulls write directly to the destination
   with one warning. The code allocates staging and maps a failure to a publication error
   (`crates/marathon-android/src/transfers.rs`, `pull_staging.allocate(...).map_err(publication_error)`), and U236 says
   D97 reports such an artifact as undelivered. D92 now has a note saying so.
3. **D22 and D41** still described a restarted or rebooted device as not prepared again ("This is a known limit"; "taken
   for a blip"); D85 changed that. Both have an "Amended by D85" note, and the migration guide's D22 bullet names D85.
4. **D31** said rsync is not put in a process group of its own; D96 runs every local tool in its own group. D31 has an
   "Amended by D96" note. Ctrl-C and a CI cancel now reach marathon, which ends the group (D96's text).
5. **D91's limits** said containing rsync's descendants needs a process group (D96 since did it) and that a Mac's sends
   are not registered (D93 since did it). Both now say so in place.
6. **Prerequisites** in the migration guide and release readiness required `sshpass` for any password rsync; D96 needs it
   only below OpenSSH 8.4 (`crates/marathon-apple/src/cmd/remote/rsync/password.rs`, `(major, minor) >= (8, 4)`).
7. **D79** was in neither migration list since 0.14.0; added to the no-action list.
8. **D96's Perl warning**: the entry says "the worker warns once" and the brief "one WARN per worker". The code keeps a
   shared set keyed by host and port (`supervisor/mod.rs`, `Warnings`), so it is once per worker address in a run.
   The changelog said "each worker logs one WARN"; after review round 1 it, the migration notes and the D96 entry say
   the controller logs one WARN per worker address.
9. **x88's findings** record evidence but no review round, unlike every other WP of the batch. The changelog says so.
10. **The x96 iOS acceptance report** called two findings release-blocking: the surviving `DTServiceHub` helpers and the
    loss of all six videos in a run on simulators whose recorders had been killed at lease expiry. The first is D96's
    documented contract; the second is U246, fixed in x95e and seen fixed in the x95e repeat on 7a486ea1. Its third
    finding, the slower healthy SSH run, led to U247 (x95e). The changelog records all three that way.
11. **CI on `main` during the batch** (from `gh run list`, not from a findings document): six runs failed in a test (the
    macOS local batch-timeout witness twice, which became U245; a D43 real-rsync pull test twice; a D93 SSH-hook test;
    a D68 pool test on Linux) and two runs had a cancelled macOS job. Every later run, and the run at f90a6619 on macOS,
    Linux and the musl package, passed. Whether the D43, D93 and D68 failures were fixed or passed by chance is not
    recorded; x96 notes that users of the shared SSH outbound-exec test hook were serialized, which may be the D93 one.

## Real-device evidence, as described

The private acceptance reports are described generically, with no host, user, path or run id. Exactly which commits:

- 1d88c0a9 (after x95d, before x95e and x95f): the CI static Linux build on two API 36 Android emulators on a Linux
  host; the CI macOS build ran iOS 18.6 and 26.4 simulator tests and hosted macOS XCTest locally on this project's Mac,
  and iOS 18.6 simulator tests over SSH to a second Mac as a real worker (stock Perl, key authentication, an
  `exec`-based forced command, `shareWorkerConnection: true`). macOS tests did not run over SSH.
- Control runs matched the earlier per-test outcomes and their normalized report fields compared equal; Android
  omitted references for undelivered videos; stalled xcresult pulls stayed unpublished, and where a pull was still
  unconfirmed at disposal the device skipped its lifecycle and its lease was quarantined.
- 7a486ea1 (x95e, before x95f): the SSH cases repeated on the second Mac. All six videos and xcresults were delivered
  in each completed control, including the one on the same simulators after a SIGKILL; U247's extra control round trips
  were gone; the cause of one slow app launch remained undetermined.
- f90a6619 (this rc): no real-device run. x95f's commits (2f512004, f90a6619; U248–U250) were in neither run.

## Verification

- `python3 -I -c "import sys; sys.path.insert(0,'scripts/release'); import release; print(release.changelog_section('0.15.0-rc.1')[:200])"`
  prints the section's intro.
- `python3 -I scripts/release/test_release.py`: 81 tests, OK.
- A link checker over `CHANGELOG.md`, `README.md`, `docs/deviations.md`, `docs/migration.md`, `docs/releasing.md` and
  this file: every relative link and heading anchor resolves (it reports a bogus anchor and a missing file when given
  them).
- Not run, per the brief: `scripts/check.sh` and the workspace tests.

## Incidental bugs

None new. The documentation disagreements above were fixed in place.

## Review round 1 (cross-model review at 1b6378ac: 8 MUST, 3 SHOULD, 1 OPTIONAL)

Each claim was checked against the deviations guide, the migration notes, `docs/found-bugs.md`, the findings
documents, the private acceptance reports and, where named, the code before the text changed. None was rejected.

| # | Finding | Status | Checked against, and what changed |
|---|---|---|---|
| 1 | Password rsync process group | done | `marathon_process::Command::new` sets `process_group(0)`, so askpass rsync and its SSH client share a group marathon owns, not the controller's. The `sshpass` warning is a process-wide `WARNED_LEGACY_PASSWORD` flag (`rsync_file_bridge.rs`), so once per run, and it also covers an unrecognized client (`password.rs` probe). Changed: changelog upgrade and detail bullets (now "from 0.15.0-rc.1"), the migration prerequisite and table row, release readiness. |
| 2 | Signal finish cutoffs | done | `signal_finish.rs` (reports head start, 12 s, 18 s, 23 s, exit at the cancel limit) and D97's text (10 s, 17 s, 24 s, 1 s console). The SIGHUP bullet and the signal detail bullet state the cutoffs; "reports always finish writing" now applies to the ordinary finish, with D97's cutoffs after a signal. The migration D97 note says "through 12 s" and "by 24 s plus up to 1 s". |
| 3 | D97 verification details | done | `client/sync/mod.rs` (DONE, writer close, count modulo 2^32 with legacy STAT); `artifact_stage.rs` (`replace_without_exchange` keeps the old tree in a `.marathon-artifact-*` backup when a restore fails); the Android report (a separate diagnostic named the enclosing folder). Changed: upgrade bullet, Android and artifacts detail bullets, Evidence, the rename-flag known limit and the migration D97 note. |
| 4 | D91/D93 bound waits | done | D91 ("cleanup is confirmed or reported", exact destination path) and D93 ("the bound after a stop": 70 s, 75 s, 80 s, artifact drain outside it; shared connections keep their users). Changed: the upgrade bullets, the D91 and D93 detail bullets and the D91 and D93 migration notes. |
| 5 | Lease semantics, Perl warning, D90 connection | done, changed wording | D96's renewal paragraph (79 s, one in-flight renewal, frozen worker), the recorder's INT 2 s plus TERM 1 s, and `supervisor/mod.rs` (`Warnings` keyed by host and port, logged by the controller). Changed: the Apple workers upgrade bullet, the D30/D96 known limit and the D96 entry's "the worker warns once". The upgrade bullet names D96's control connection ("with Perl") and D90's replacement data connection. |
| 6 | D85 detection, D87 deadline | done | D85 (confirmed missing or different marker; boot ID alone when the marker write is unconfirmed; no evidence, ordinary recovery) and D87 (refuses new work only; D85's setup continues the deadline with one first attempt; +1 minute on Android). Changed: the upgrade bullet, the D85 and D87 detail bullets, the D22 and D41 amendments, the migration table row and D22 bullet, the known limit; the migration D38 bullet's "26 to 30 minutes" now gives D87's estimate, and D38 has an "Amended by D87" note. |
| 7 | Witness and review claims | done | x82 and x85 list unverified parts; x88's D90 witness prepares a simulator on loopback SSH but is not a whole run; x92 records eleven review rounds and a closing audit. Evidence now says what the findings record, not that every fix has a witness. x88's missing review round is still noted. |
| 8 | Acceptance evidence | done | The three private reports: macOS hosted XCTest ran only locally; Android dropped 13 video references for undelivered recordings; R02a disposed both devices, R02b and R02c quarantined the stalled one; on 7a486ea1 every completed control delivered six videos and six xcresults, one-shot control requests went from 30 to zero, and the slow launch's cause was undetermined; the zero-byte MP4s were also in the first warm control. Changed: Evidence, README, the static Linux known limit (0.14.0-rc.2) and the zero-byte known limit. |
| 9 | D94, D95, D88 details | done | D94 (subscription's own 65 s; uncommitted work can be lost), D95 (40 s clipped; 30 s after a separate 3 s refresh; expiry warns and finish goes on), D88 (provider termination excluded). Changed: upgrade and detail bullets. |
| 10 | U references | done | `found-bugs.md`: U235–U237 Android, U238–U240 Apple artifacts, U242 the provider's recovery during termination (now on the signal bullet), U210/U211 on the Apple preparation and loader bullet without D88; U243, U244 and U248–U250 reworded to their fixed text. |
| 11 | D31/D91 amendments | done | D96's password paragraph and the remote sender supervision. D31's "Amended by D96" and D91's limits now distinguish direct rsync and askpass (owned group) from `sshpass` (separate session, listings) and mention D96's sender supervision on Perl workers; D91's Mac-sends note already named D93. |
| 12 | Intros and this document | done | Changelog intro names bounded waits, metrics buffering and connection replacement; the deviations intro says "D84–D88 and D90". This document's verbatim list and real-device summary were refreshed. |

Verification after the round: `release.changelog_section('0.15.0-rc.1')` prints the intro; `test_release.py` 81 tests OK;
the link and anchor check over `CHANGELOG.md`, `README.md`, `docs/deviations.md`, `docs/migration.md`,
`docs/releasing.md` and this file reports no broken link (it flags a planted bad anchor and a missing file); the local
naming check is clean. `scripts/check.sh` and cargo were not run (docs only; the orchestrator gates).
