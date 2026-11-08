# X26: stalled rsync transfers (U163, D43)

The project owner approved these on 2026-11-07. U163 extends D31, and D43 is new. No configuration field or exit code changed. `LeavePoolReason` gained `TransferStalled`.

## U163: a stalled send

**Verified.** The row was right about the code. A stalled send returned `false`, the installer's `with_retry_send(3, 1000)` repeated it, and the device's `with_retry(30, 10_000)` repeated that. The row's 1.6 hours for a dead link was wrong in practice: see below.

**Fix.**

- `RsyncFileBridge::send` returns `VendorError::LeavePool` with `TransferStalled` and class `…rsync.TransferStalled` when rsync exited non-zero after reporting a stall. Anything else stays what it was: `false` for a process that failed, an error for one that couldn't start.
- The installer's retry stops on any `LeavePool`, as the device actor's already does.
- An iOS simulator is retired, like one that isn't admitted, so that recovery doesn't set it up again. Marking it lost, as a failed boot does, would have brought retries back by another route.
- `StallRecord` fails what waited behind the stall. A send takes a ticket before it queues, and if a stall was recorded since, it returns that error without starting a process. There are two queues, so there are two records: the bridge's mutex per worker, and `SharedTransfers`' lock per destination, whose waiters reach the bridge only after the stall has ended.
- A send that starts after the stall has no ticket to lose, and tries. The worker may have come back.
- The two parsers stop on it instead of repeating it three times on the same device. Core's own three attempts stay, since each borrows a device again.

The stall test was what D31 already used, `poll: timeout` or `io timeout after` anywhere on stderr with a non-zero exit. A false match needed that text in a line on a send that is failing anyway, such as in a file name, and would cost the simulator. Review round 2 narrowed it to whole lines and exit 30.

**New worst case**, with what each row rests on:

| Case | Before | Now | Evidence |
|---|---|---|---|
| Stalled send, one simulator | 90 sends, 7.6 h | 1 send, 305 s measured, at most about 315 s (since round 3 at most about 320 s, up to 20 s more where `ps` is slow) | Real rsync and ssh, frozen worker, keep-alive off, built binary. Run exit 1 at 335.7 s, 30 s of that disposing over the frozen connection |
| N simulators queued on one worker | About N × 7.6 h | The first stall ends all those queued at that moment: one 300 s | `stalled_send_runs`, `a_stall_fails_who_waited_*`. Not measured as a whole run with N > 1 |
| Simulators reaching their send one after another | Same | One stall each, at worst N × about 315 s (320 s since round 3, more where `ps` is slow). Installer and boot run side by side from the start of prepare, so normally they queue together | By reading |
| Stall during remote parsing | 9 sends per run, 27 with the xctest parser, by reading | 3, and all within `deviceInitializationTimeoutMillis` (180 s by default, which ends it before any 300 s can) | `a_stalled_send_while_parsing_*`, counted |
| Any other failed send | 90 sends | 90 sends, unchanged | Counted below |

**The counted runs**, on the setup the interaction audit describes: the built binary with the real scheduler, pool and device actor, fake workers, and a stand-in `RSYNC4J_RSYNC` that fails the sends to one worker, counted. The other worker's bundle gets through and its later sends fail too, which keeps the queue from ending the run early.

| Failure | Sends to the failing worker | To the other | Exit | Wall |
|---|---|---|---|---|
| `rsync: some other failure`, exit 23 | 93: 3 while parsing there, then 3 × 30 | 30 | 1 | 352.4 s, the 350 s of retry waits |
| `error: poll: timeout`, exit 20 | 1 | 1 | 1 | 1.4 s |

The suite pins the same through `crates/marathon-cli/tests/ios_stalled_send.rs`, including a healthy second worker finishing the run with exit 0.

**What a dead link costs.** Measured with the built binary, the worker process frozen with SIGSTOP as its first non-bundle send starts, so that nothing answers while the kernel still accepts connections:

| Keep-alive | What ended it | Sends | Run |
|---|---|---|---|
| Default, 15 s × 4 | At 75 s the device's own SSH connection `Did not receive any keep-alive response for 60 seconds`, the device is disconnected, its prepare cancelled | 1 | Exit 1 at 75.4 s as it was the last device |
| Off | At 305 s the send's own limit | 1 | Exit 1 at 335.7 s |

So x25's 1.6 hours, 90 sends at 60 s each, does not happen on a dead link: it needs sends to fail while the command connection stays healthy. After the disconnect the simulator is D36's and U164's matter, with no more sends. The same frozen setup was not run with a second worker.

**ConnectTimeout: no.** ssh's connect to an unroutable address failed after 75 s here on macOS 27 and 134 s on Linux. A send waits that long only if new connections hang while the command connection to the same worker stays up, as with a port filter or sshd's `MaxStartups`. That is an ordinary failure, so at worst 90 × 75–134 s, 1.9 to 3.4 hours, and rare. Adding `-o ConnectTimeout=N` would shorten that, but:

- It changes `--rsh` on every send and receive on every host, for wrappers and comparators alike.
- It would make things worse elsewhere. `ConnectTimeout` also covers ssh's banner wait, so a peer silent before the session would end in ssh's `banner exchange` error in N s. That reads as an ordinary failure, so a case that now costs one 300 s stall would cost up to 90 attempts, 45 minutes at N = 30.

If that rare case ever turns up on a farm, classifying ssh's connect timeout would be the fix, not this flag.

**A cost of the fix.** A healthy worker that is silent for 300 s now loses, for the whole run, every simulator waiting to send to it at that moment. D31's 300 s was chosen to make that unlikely. Simulators don't come back if the worker does.

## D43: a receive's limit

**Why `--timeout` alone doesn't work.** The brief asks for D31's flag and stop. On rsync 3 that leaves a hole. Measured with real rsync and ssh, a throwaway sshd, 30 MiB through a loopback throttle of about 1 MiB/s, and a short `--timeout`:

| A pull with a short `--timeout` | openrsync to openrsync (macOS 27) | rsync 3.4.1 to itself (Linux, protocol 32) | rsync 3.4.1 with `--protocol=29`, as with an openrsync worker |
|---|---|---|---|
| Moving, `--timeout=2`, 32 s | Exit 0, complete | Exit 0, complete | |
| Moving, `--timeout=300`, past 30 s | Exit 0, complete, 32.5 s | Exit 0, complete, 37.0 s | |
| Moving, `--timeout=2`, under `sshpass` | Exit 0, complete | No `sshpass` there | |
| The bytes stop mid-file | `error: poll: timeout`, then waits for ssh | **Reports nothing, still waiting after 30 s with `--timeout=3`** | `[generator] io timeout`, exit 30 |
| The link stops both ways mid-file | `error: poll: timeout`, then waits | **Reports nothing, still waiting after 40 s** | |
| Worker's rsync stopped mid-file | `error: poll: timeout`, then waits | **Reports nothing, still waiting after 40 s** | `[generator] io timeout`, exit 30 |
| Worker's rsync stopped before the transfer | `error: poll: timeout`, then waits | `[Receiver] io timeout`, exit 30 | |
| Keep-alive 1 s × 2, the bytes stop | Exit 255 after 5.5 s | Exit 255 after 5.3 s | |

Reading rsync's `check_timeout` fits: on the receiving side the generator decides, and it counts its own keep-alive writes as I/O, which keep succeeding into ssh's buffers. The cause wasn't traced further than that. With the 30 s total gone, a pull between two rsync 3 installs that stops mid-file would wait for ever and hold up its batch. Kotlin's total used to end that, so it would have been a regression.

**What was done.** Both the flag and a check on the host, since neither covers everything alone. The flag gives the stalls rsync reports, and it passes on to an rsync 3 worker. The check, `nothing_arrives`, looks every 10 s, on the vendor clock, off core, at where the bytes land: a folder's whole tree, or for a file its folder and `.rsync-partial`. After 300 s with no change in count, size or latest change, it stops the process the same way. A pull that ends within 10 s never looks. A `--timeout` that is honoured gets there first or at the same time, so the check adds nothing on openrsync.

Its limits:

- Something else writing to that folder delays the stop. Review round 2 narrowed the look to the pull's own files.
- rsync 3 writes in pieces of 256 KiB, so a link below about 1 KiB/s can read as stalled. That's a guess from rsync's buffer size, not a measurement.
- An rsync that never writes where expected, such as one given a different `--partial-dir` by a wrapper, reads as stalled after 300 s.

**300 s, as D31.** The reasons for a shorter limit are that a false positive costs an artifact rather than a device, and that the wait per pull is the cost here. A worker's silent stretches on a pull are its file-list walk of a large result bundle, under the same load that D31 allows for. 120 s would make the rare wedged case 2.5 times cheaper and nothing else, as a dead link already ends in 60 s. One value also means one flag for wrappers, comparators and the router to allow, and one rule for users.

**What it costs, and what bounds it.**

| Stuck pull | Before | Now |
|---|---|---|
| Worker wedged, keep-alive answered, openrsync | 35 to 45 s (x21) | 300 s + 5 s for rsync to end + at most 5 s more for TERM, plus up to 5 s each for listing under `sshpass` and for the pipes: about 305 to 320 s (since round 3 about 305 to 330 s, up to 20 s more where `ps` is slow) |
| Same, rsync 3 to itself | 35 to 45 s | The same, up to 10 s later |
| Dead link, default keep-alive | 35 to 45 s | About 60 s. The device is disconnected at about 75 s |

Nothing bounds the total over a run. Pulls have no retries, and each stuck one costs its own limit: per batch, a video per test that the policy keeps, the last-batch video and the device log, all inline, holding up that simulator's batch. By reading, `testBatchTimeoutMillis` and `testOutputTimeoutMillis` bound xcodebuild and not its listeners, so they don't help, and an inline pull doesn't cause a false output timeout either, since a reader blocked behind it finds any waiting output first. On iOS result bundles go through the `ArtifactProcessor`, which `close_and_join(300_000, 60_000)` cuts at the end of the run. On macOS they are pulled inline too. This needs rsync to hang while the worker's commands keep working: a dead link ends far sooner, and a worker with hung commands loses its device. The owner may want a limit here, such as giving up on a worker's pulls after one stall, but that loses artifacts that might still have arrived, so it was left out.

**Listeners that ignore the result**, none of which retries, as before:

| Caller | On `false` |
|---|---|
| `listener/video.rs:136` | Logs `Pulling finished`, then removes the remote video. The video is lost, with `.rsync-partial/<name>` left |
| `listener/device_log.rs:90` | Logs it the same way, with the partial file left |
| `result_bundle_run_listener.rs:186` | `pull failed`, recorded by the processor, and the remote bundle is kept |
| `apple_simulator_device/worker.rs:340` | The screenshot returns `false` |

**What the worker and wrappers see.** Measured: an rsync 3 host sends the worker `--server --sender -ltpre.LsfxCIvu --timeout=300`, and an openrsync host `--server --sender --delete-before -l -p -r -t --dirs --delay-updates`, without the flag. `RSYNC4J_RSYNC` wrappers get it on every transfer. Debug's `--version` call doesn't.

**Comparators.** `vendor_transport.with_d43_flag` gives older records' receives exactly the flag, beside `with_d31_flag`. `test_d31_d43_*` shows that a transfer of either kind with the flag missing, with another value or elsewhere differs, that the version form gets nothing, and that an older record with it already is refused. The router now accepts `--timeout=300` with `--sender`, and refuses other values in both directions. `tools/differential/registry.json` was regenerated, and only the hash of `vendor_transport.py` changed. `fixtures/oracle/connections/deadlines.jsonl` stays as the witness, along with the JVM argv recordings. No fixture was edited.

**`stop_process` and `stop_stalled_send` are one function again**, `stop_stalled`, as x21 had split them only for the receive's deadline. The diagnostic `Timeout of 30 seconds reached, terminating process...` went with the deadline.

## Failure without each fix

By temporary revert, one at a time:

| Reverted | Fails |
|---|---|
| Installer stops on `LeavePool` | `a_stalled_send_is_not_retried_and_takes_the_simulator_out_of_the_run_u163` |
| Retiring at prepare | The same test |
| Failing the bridge's queue | `a_stalled_send_that_waits_for_a_hung_ssh_is_stopped_on_term_d31`, through `stalled_send_runs` |
| Failing the destination's queue | `a_stall_fails_who_waited_for_that_destination_and_nobody_else_u163` |
| The stall as its own outcome | `a_stalled_send_on_the_only_worker_ends_the_run_after_one_send_u163` |
| Parser stopping on it | `a_stalled_send_while_parsing_is_sent_once_per_borrowed_device_u163` |
| The flag on a receive | `empty_options_match_*`, `host_argv_matches_*`, `a_stalled_receive_*`, `rsync_that_ignores_term_on_a_stalled_receive_*` |
| The check of arrivals | `a_silent_receive_that_stops_moving_is_stopped_d43` |
| Arrivals resetting the wait | `a_silent_receive_that_keeps_moving_past_the_limit_finishes_d43` |

## What was verified, and how

- `a_pull_longer_than_the_old_deadline_*`: real rsync, real ssh, a real `sshpass` typing the fake worker's real password prompt or key authentication, the fake worker's real `rsync --server`, 11 MiB behind a throttle in about 34 s at factor 1. The bytes are compared.
- `a_pull_that_stops_moving_*`: the same with the bytes stopped and the socket held, keep-alive off.
- Those ran on this Mac with openrsync, and on a Linux box with rsync 3.4.1, where the second is the only thing ending the pull: the clock is compressed by 20 so that 300 s is 15 s. `sshpass` variants skip on Linux, since it isn't installed there, so **rsync 3 under `sshpass` was not run**.
- The lab table above ran against a real sshd, under `sshpass` for the moving row with key authentication underneath.
- The bridge suite passes on both machines. Crate tests were run as listed in the report.

## What was not verified

- rsync 3 on the host with a real openrsync worker. `--protocol=29` stands in for it.
- A real packet-dropping link. SIGSTOP and a held socket were used, and TCP's connect timeout was measured apart.
- A second worker alongside the frozen one, and N > 1 simulators through the whole run.
- The false-positive rates of 300 s on real farms, and whether 1 KiB/s links exist there.

## Review round 2

A cross-family review found three major issues and one minor one. Each fix has a test that fails without it, shown by temporary revert: all seven new integration tests fail with the bridge of round 1, the first two groups also with only their own fix reverted.

| Finding | Fix | Tests |
|---|---|---|
| S-x26-1: `poll: timeout` anywhere on stderr counted as a stall, so an ordinary failure naming that text, such as exit 23 for a missing file, left the pool with its queued successor | A stall is exit 30, rsync 3's `RERR_TIMEOUT`, or rsync's own report as a whole line: openrsync's `rsync(<pid>): error: poll: timeout`, measured on this Mac's openrsync, whose status after it was 16 here and depends on how ssh ends, so it isn't used; rsync 3's `[<role>] io timeout after <n> seconds -- exiting`, without the role before 3.1, from its source and round 1's Linux runs (`[generator]`, `[Receiver]`, exit 30). A line still being written doesn't count | `only_rsyncs_own_timeout_reports_are_stalls_d31` (unit, with names, other I/O errors and partial lines), `a_failed_send_that_names_the_timeout_report_keeps_its_retries_d31` (exit 23 twice, 1, 12 and 255, each keeping `false` and the next send), `a_send_that_exits_with_rsyncs_timeout_status_stalled_d31` |
| S-x26-2: a pull's idle check counted every write in its folder and all of `.rsync-partial`, so another pull there kept a stalled one going | The check looks only at the pull's own files: the destination (its whole tree for a folder), `.<name>.XXXXXX` beside it and `.rsync-partial/<name>`. Measured with this Mac's openrsync: `.<name>.` and 10 letters or digits beside the destination, moved to `.rsync-partial/<name>` when interrupted. rsync 3 uses 6 (`mkstemp`) and cuts names over 247 bytes, by its source, not run here, so a prefix of 200 bytes or more is accepted. A folder destination with a remote source ending in `/` still counts the whole folder | `a_receive_watches_its_own_name_its_temporary_files_and_its_partial_file_d43` (unit), `a_stalled_receive_is_stopped_while_another_into_the_same_folder_keeps_writing_d43`: two pulls into one folder, the stalled one returns `false` while the other is still writing, then the other finishes. Without the fix it is never stopped and the test fails at its 60 s catch |
| S-x26-3: a stop signalled only the child and, under `sshpass`, its direct children, and an rsync that exited in its first 5 s skipped the rest, so ssh survived rsync 3 or `sshpass` | `Descendants` lists processes with one `ps -A -o pid=,ppid=,lstart=` at 1 s, every 10 s while the transfer runs, and at the stop, and keeps every process whose parent it already knew, with its start time. A stop TERMs those still there after rsync's 5 s, whether or not rsync ended, waits 5 s and KILLs those still there with the same start time. `pgrep` and `started_at` are gone | `a_stalled_send_that_exits_while_ssh_holds_its_stderr_returns_and_leaves_no_ssh_d31` (renamed: it now also asserts the held child is gone), `a_stalled_send_under_sshpass_that_exits_leaves_no_ssh_d31`, `a_stalled_receive_under_sshpass_that_exits_leaves_no_ssh_d43`, `a_stalled_receive_whose_ssh_ignores_term_leaves_nothing_d43` (with and without `sshpass`), and the real-tool tests, which now count ssh on every host |
| S-x26-4: D43 said both listeners remove the remote file after a failed pull | Reworded: the video listener removes the remote video, the device log listener leaves the remote log (`device_log.rs` has no removal) | Docs only |

**Why not a process group.** The review suggested spawning rsync in its own process group and signalling the group. That alone doesn't cover `sshpass`: it calls `setsid` for its child, so rsync and ssh are in another group anyway, whose id is known only from a listing. And a group of its own takes rsync with key authentication out of the group that a terminal's Ctrl-C or a CI job's cancel signals, while marathon on a signal exits without stopping its children, so a transfer would outlive the run where Kotlin's does not. Listing keeps that as it was.

**The harness's exemption is gone.** `run_stalled_send` used to answer only for rsync on Linux and for a stand-in's held child nowhere. It now asserts that nothing the transfer started survives a stop, on every host. It treats a zombie as gone: a KILLed orphan waits for init to reap it, and still answers signal 0, which made one run here fail under load. U158's runs are still exempt, since there rsync ends by itself, nothing is stopped, and the test asserts the holder is still up. The stand-ins that leave ssh behind wait until a listing the bridge has read includes that ssh (`listing_ps`, `wait_till_listed`), as it would have long before a real 300 s. A first version waited for any listing, and failed once under load when the listing came before the stand-in started its ssh. The real-tool tests do the same, with `--timeout=5` instead of 1 so that rsync reports well after the first listing.

**The CLI's stand-in.** `ios_stalled_send.rs` used `rsync: error: poll: timeout` with exit 20, which no rsync prints. It now prints openrsync's real line, `rsync(<pid>): error: poll: timeout`, with the 16 measured here. The counted runs above used the old stand-in and were not repeated.

**Docs.** CHANGELOG, D31, D43, PLAN §3.11 and the migration guide now state the conditions: a pull isn't cut while rsync reports no stall and its own files change at least once every 300 s; a stall is the report as a whole line or exit 30; a stop reaches the processes `ps` has seen. A stuck pull costs about 305 to 325 s: 300 to 310 s for the look, 5 s for rsync, 5 s after TERM and up to 5 s for the pipes, and up to 15 s more where `ps` is slow.

**Remaining limits.**

- A process started and left behind between two listings is missed: one forked and orphaned within the first second, or within 10 s later on. ssh and `sshpass`'s child start at once, so for them that would take rsync reporting within 1 s of starting.
- Where `ps` fails or takes over 5 s, only the bridge's own child is signalled, and a descendant whose start can't be confirmed at KILL time gets no KILL.
- U158 is unchanged: a process that ends by itself without a stall leaves its children, such as a hung ssh, as Kotlin does.
- A wrapper that writes elsewhere, or a file name with a newline that makes a whole report line, can still mislead the two checks.
- Linux, and rsync 3 under `sshpass`, were not run in this round: no other host was used. The orchestrator's Linux gate runs the same suite, where the real-tool tests now also count ssh.
- A transfer that runs longer than 1 s now costs a `ps` of every process then and every 10 s.

## Review round 3

The re-check of round 2 asked for rework. The project owner decided how each finding is fixed. Every fix has a test that fails without it, by temporary revert: without the cleanup after an exit, all six "leaves nothing" and "leaves no ssh" tests fail; without the time rule, `a_failed_send_that_names_the_timeout_report_keeps_its_retries_d31` fails; without the stop-limit check, `a_send_stopped_at_rsyncs_requested_limit_keeps_its_retries_d31` fails; with round 2's name matching, the unit test fails.

| Finding | Fix | Tests |
|---|---|---|
| S-x26-3: if rsync's exit was seen before its report was read, or it exited 30 with no report, the stall was decided after the process had gone and nothing it started was stopped | `process()` now decides the stall itself, and every stall ends what the transfer started (`Descendants::end`), however it was decided | `a_send_that_exits_with_rsyncs_timeout_status_stalled_and_leaves_nothing_d31`, `a_stalled_receive_that_exits_as_it_reports_leaves_nothing_d43`, and round 2's exit tests, which now report after the limit |
| S-x26-1: a file name with newlines can forge a whole report line; S-x26-6: exit 30 also ends `--stop-after` and `--stop-at` | The owner's rule: a stall needs, besides the report or exit 30, 285 s with no progress the bridge saw, counted on the vendor clock in steps of 1 s, since the start for a send and since the last change of its files for a receive. 285 s is the 300 s less 10 s, how late a change can be seen, and 5 s of tolerance. A report that comes sooner is noted, and stops the transfer once the time is reached. Exit 30 doesn't count after `stopping at requested limit`, which rsync 3's io.c prints for those options | `a_failed_send_that_names_the_timeout_report_keeps_its_retries_d31` with the forged line and a quick exit 23, and a quick `[sender] io timeout` with exit 30; `a_send_stopped_at_rsyncs_requested_limit_keeps_its_retries_d31`; `a_timeout_report_that_comes_at_once_keeps_both_retries_d31` through the built binary. The legitimate stalls are the existing hung tests |
| S-x26-2: rsync 3's temporary name for `.back` was missed, and another file's truncated one counted for a long name | `is_temporary_of` follows rsync 3.4.1's `get_tmpname` (receiver.c, read for this round): one leading dot dropped, at most 246 bytes without splitting a multi-byte character, one trailing dot dropped, `.XXXXXX`. For openrsync, measured here with names `.back`, `trail.`, `x.y.` and 250 bytes: the name as it is, cut to 243 bytes, and 10 random characters. Two names that are both cut and share their first 246 (or 243) bytes still share a temporary name, and that leak is accepted | `a_receive_watches_its_own_name_its_temporary_files_and_its_partial_file_d43`, with both witnesses, the dots, the cuts and a split `é` |
| S-x26-5: pid and `lstart` could reach a process that isn't ours | Right before each signal the bridge lists the processes again and signals only one with the same pid and start time whose parent is one of the transfer's or init (pid 1, launchd on macOS). The reaped child is never signalled: it is signalled through its handle only while unreaped, and counts as a parent only then. A pid reused within the same second, between that listing and the signal, can still be hit | None, as decided |
| S-x26-7: D31 promised at most about 15 s to stop | D31, D43, PLAN and the older tables above now say 5 to 20 s, up to 20 s more where `ps` is slow: four `ps` calls of up to 5 s each at most | Docs |

**Why not supervised process groups.** A group of its own, with marathon forwarding SIGINT and SIGTERM to it, would cover key authentication. It still wouldn't reach what `sshpass` runs under `setsid`, so the listing would stay. And it needs marathon to stop every live group whenever it exits, which is new machinery in the signal relay.

**The tests and the new rule.** A stand-in that reports at once is no longer a stall, so the stand-ins that stall wait out the 285 s first. In the bridge tests the clock is compressed, by 100 in most of them, so that takes 2.85 s and the steps take 50 ms. The tests with real rsync now run at that factor too, which ends ssh with rsync rather than after a real 5 s. `rsync_under_sshpass_gets_its_step_after_term_d31` runs at 10, with a handler of 0.05 s against a 0.5 s step. Stand-ins that exit by themselves wait 6 s first (`PAST_THE_LIMIT`).

The CLI's three stall tests run the built binary on the real clock, so each stall takes 295 s. They are `#[ignore]`d, on the owner's decision, and run by hand. In the two-worker test the first send to the healthy worker, other than the test bundle that parsing needs, now waits 340 s, so that worker is still busy when the stall is decided. That test's Marathonfile now has `keepAliveInterval: PT15S`, because the fake worker drops a connection idle for 300 s, which an earlier attempt hit. Run here:

```
cargo test -p marathon-cli --test ios_stalled_send -- --ignored a_stalled_send_costs_its_simulator a_stalled_send_on_the_only_worker
test a_stalled_send_on_the_only_worker_ends_the_run_after_one_send_u163 ... ok
test a_stalled_send_costs_its_simulator_one_send_and_the_other_worker_finishes_the_run_u163 ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 341.56s
```

`a_stalled_send_while_parsing_is_sent_once_per_borrowed_device_u163` was not run here: three stalls in a row take about 15 minutes, more than one foreground run here allows. Its `deviceInitializationTimeoutMillis` was raised to 20 minutes so that it can pass. The reviewer of round 3 ran all three ignored tests, and all passed, the parsing one in 886.7 s.

**Remaining limits.**

- Each signal can still hit a pid reused within the same second: between the last listing and the signal, and earlier, when a process the bridge saw exits, a new one takes its pid within the second it started, and is orphaned to init before the last listing, so it passes every check.
- An orphan that a subreaper other than init adopts, as on some Linux desktops, isn't signalled.
- A process started and abandoned between two listings is missed, and where `ps` fails or takes over 5 s only the child is stopped.
- A stall now takes at least 285 s by the bridge's count. An rsync that reports sooner, such as one a wrapper gives a shorter `--timeout`, isn't taken for a stall.
- Two long names that share their first 246 bytes count each other's temporary files.
- Linux and rsync 3 under `sshpass` were not run in this round either.

## Review round 4

The re-check of round 3 said "merge after fixes", for two code findings and one in the docs. It ran the three ignored CLI stall tests, and all passed, the parsing one in 886.7 s.

| Finding | Fix | Tests |
|---|---|---|
| S-x26-8: the time with no progress counted finished one-second delays, so a bridge that wasn't scheduled for a while lost time, and a real stall after 300 s could come back as an ordinary failure with a child left running | The bridge stores when progress was last seen, on its clock: the start for a send, the look that saw its files change for a receive. It measures from there when rsync reports, as the process ends however it ends (before its pipes are waited for), and at each look. The looks at files are timed on the clock as well. `VendorClock` has `now_millis` (added by x31 as well; on main it reads 0 for a clock with no time of its own, so test clocks that don't override it never see a stall), the scheduler's timeline in production (`SendClock`), and the delays' compression in the tests' `ScaledClock` | `a_stall_is_measured_on_the_clock_while_the_bridge_is_not_scheduled_d31`: the bridge's delays come ten times late (`ScaledClock::lagging(10, 100)`), rsync 3 reports and exits after 6 s, 600 s on the clock but only 60 delays; it must still be a stall that ends rsync's child. It fails on round 3's bridge |
| S-x26-2: rsync 3 can keep nothing of a name, such as 125 `é`: its trailing-dot rule then takes the dot it wrote first, and the temporary file is `.XXXXXX`, which was not recognised | The matcher builds rsync 3's prefix as `get_tmpname` does: a dot, what is kept of the name, then one trailing dot dropped, whichever dot that is. A name `..` gives `..XXXXXX` the same way | The unit test, with the 125 `é` witness, `..`, and a pull into that name whose `.a1B2c3` grows. It fails with the old rule |
| S-x26-5 (docs): the documented pid-reuse window was too narrow | D31 and round 3's remaining limits now also describe the earlier window, which the policy doesn't change | Docs |

The CHANGELOG, D31 and the migration guide also now say what a stall costs on each platform: the send isn't retried and the device leaves its pool; only an iOS simulator is retired for the rest of the run.

Run here after the fixes: `cargo test -p marathon-cli --test ios_stalled_send -- --ignored a_stalled_send_costs_its_simulator a_stalled_send_on_the_only_worker`, `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 341.61s`, which goes through the production clock's `now_millis`.

**Remaining limits**, besides round 3's:

- A receive's progress is stamped when a look's result has come back. (Round 4 stamped it before the look and wrongly said a late look could only make the quiet time shorter; round 5's review showed a look returning 400 s late then read as a stall, S-x26-9.) A pull whose looks were held up for a long while can need that much more time before its report counts.

## Review round 5 (orchestrator's fixes after the final review)

- **S-x26-8 (major, still present in round 4).** The start of a transfer was read after the spawned child reached the bridge, so a late delivery lost process time and a real 300 s timeout could return `Ok(false)` with its child left running. The start is now read on the vendor clock inside the blocking spawn and returned with the child.
- **S-x26-9 (major, round-4 regression).** A receive stamped its progress before awaiting the look at its files, so a look whose result came 400 s late read as 300 s without progress while files were changing. The stamp is now taken after the look returns.
- **S-x26-10 (minor, documented as a limit).** The 285 s are counted to when the bridge sees the exit, so a bridge paused across a quick failure can still classify timeout-looking stderr as a stall. D31 and CHANGELOG say so.

The reviewer ran both timestamp fixes against its witnesses in a private copy; they passed there. The reviewer also ran the two long CLI stall checks (341.45 s).

## Incidental bugs

- **`sshpass` on macOS can leave ssh with no terminal.** With sshpass 1.06 and macOS 27, about one real password pull in four here failed at once: `read_passphrase: can't open /dev/tty: Device not configured`, then `Permission denied`. The cause is not established, and whether it happens outside these test wrappers is not known. Under Kotlin the same chain would fail the same way. A send retries, but a pull does not, so an artifact could be lost. `real_pull_child` tries again on that text.
- **A stalled send during remote parsing ends the run, even with a healthy second worker.** Borrowing picks a device at random, and three failures on the stuck worker, or the 180 s `deviceInitializationTimeoutMillis`, fail the run. Kotlin does the same. A borrow that skips a device that just stalled would help, but no such state exists at that stage.
- **No test reads `fixtures/oracle/connections/deadlines.jsonl`**: it is a witness by documentation only, and was before this WP.
- **Password runs of the fake worker's `ssh` shim still offer the developer's default keys.** `IdentitiesOnly=yes` with no `-i` tries `~/.ssh/id_*`, seen in `-vvv`. They are public keys offered over loopback to a throwaway server, which rejects them. Harmless, but not the isolation the shim's comment suggests.
