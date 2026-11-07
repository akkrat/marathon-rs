# X37: Android follow-ups from the review of x18, x24 and x19

A cross-family review after the merge of WP x18 (D25's cut streams), U159 (the removal before a folder push) and WP x19
(D41) found three faults in our own code and one timing claim that does not hold. Its throwaway witnesses became the
regression tests below; each failed before its fix. No deviation number, configuration field, CLI option or exit code
changed.

## S-x24-1: the removal before a folder push was not quoted

`before_test_run_in` removed a pushed folder's destination with `remove_remote_path`, Kotlin's `rm -r <path>`. A folder
named `payload data` gave `rm -r /sdcard/payload data`, which deletes a sibling `payload` and then `data` relative to the
shell's working directory, normally `/`: on a device whose adb runs as root that is `/data`.

**Fix.** A separate `RemoteFileManager::remove_push_destination`, used only by U159's call site, quotes the path with
adam's `bash_escape` (single quotes, an embedded `'` as `'\''`, the helper the push request already uses) and passes
`--`. `remove_remote_path` and every Kotlin-ported caller send the same commands as before; their own lack of quoting is
Kotlin's, ledger rows N41 and N42.

**Test.** `a_folder_push_with_a_space_removes_only_its_own_destination_d27` takes the command the fake received and runs
it with `sh` on a private temporary directory: the sibling stays, the destination goes, and a second run on the absent
destination exits 0.

## S-x24-2: a failed removal was ignored

The removal's exit status was not read. With `Permission denied` setup went on, the stale files stayed where the tests
read them, and the fresh folder landed inside, at `/sdcard/payload/payload/data.txt`.

**Fix.** The command is `rm -r -- '<d>' || { [ ! -e '<d>' ] && [ ! -L '<d>' ]; }`: it succeeds when `rm` did, or when
nothing is left, which covers an absent destination without a separate round trip. `-e` follows a link, so `-L` keeps a
dangling link that `rm` could not remove from reading as gone (review round 2, S-x37-1). Anything else fails the batch's file sync setup with
`Could not delete remote file(s): <path>: exit code <n>: <rm's output>`, class `CommandRejectedException`, as a refused
critical shell command does; the batch's tests come back as uncompleted. `[` is a shell builtin in mksh and toybox's
`sh`, and toolbox's and toybox's `rm` take `--` through `getopt`.

**Test.** `a_folder_push_fails_when_its_leftover_cannot_be_removed_d27`. It was proved against the fix itself by making
only the exit check inert: the test then saw the stale file at the root and the fresh one nested. The existing
`a_folder_push_removes_a_leftover_first_d27` now pins the new command. `a_dangling_link_left_by_a_failed_removal_fails_the_push_d27`
replays the command with `sh` and an `rm` that fails: with `-e` alone it exited 0 and left the link.

## S-x18-1: a bare `INSTRUMENTATION_CODE` counted as the end

`ended_without_result` accepted the word anywhere in the unparsed buffer. A runner that omits `numtests` gives `transform`
no count to miss, so a parse cut after the word, before its number, passed its partial list: in the whole-run witness the
run passed with 1 of 20 tests.

**Fix.** Only a whole code line counts when its line break is missing: the last line starts with
`INSTRUMENTATION_CODE:` and the rest, trimmed, is an integer, read as `parse_instrumentation_code` reads it. A code line
with its line break was already an atom and sets `session_ended`, unchanged. x18's rule for blank output and a cut inside
the first word is unchanged and still splits on `\n` only; the recordings test
(`recordings_end_with_their_code_except_the_ones_cut_off_d25`, including adam's `log_13`, which ends without a line break)
and the refusal tests pass unchanged.

**Tests.** `a_stream_cut_inside_its_code_line_is_cut_off_d25` (unit: the word, `:`, `: `, `: -`, a code after other text,
and a code split across chunks), `remote_parser::a_parse_cut_inside_its_code_line_is_retried_d25`,
`reconnect_test::a_run_does_not_take_a_parse_cut_inside_its_code_line_for_the_list_d25`. The last two failed with the old
rule restored, with `(["one"], 1)` and `(true, 1, 1)`.

## S-x19-1: another device's return probe delays a grace (docs only)

Each device that returns from a grace is asked for its properties inside the tracking loop, one after another, each
bounded by `timeoutConfiguration.shell`, 20 s by default. Its logcat (up to 1 s) and reverse port forwards
(`portForward` each, defaulting to `shell`) are restored there too. Meanwhile no other device's grace is checked. The
review witnesses held a second device past 15 s with its 10 s limit, and, with two returned devices and `shell` at 30 s,
still at 50 s. D41's text in CHANGELOG.md, `docs/deviations.md` and `docs/migration.md` now says a limit can run longer
by up to the sum of the outstanding probes' timeouts, 20 s each by default, plus those restores. The first version of
this text said 30 s, the iOS default, and a single probe; review round 2 corrected both (S-x37-2). Recorded as U170, open: a fix would move the
probe out of the loop, which is code under other work. The witness was not kept, as it fails by design until then.

## Docs corrected

- D25 no longer says a remote parse "never returns a partial list". One cut is still indistinguishable: a stream that
  stops after other text, before the first status atom, reads as output that never was the protocol and gives no tests
  for that bundle, with no retry. Added to CHANGELOG's known limits and to the migration guide.
- D27 and U159 say what a failed removal does; the migration guide says what to do about it.

## What was run

`cargo fmt --check`, `cargo clippy -p marathon-adb -p marathon-android --all-targets -- -D warnings`,
`cargo test -p marathon-adb`, `cargo test -p marathon-android`, `cargo test -p marathon-cli`, which depends on both,
and `python3 -I tools/differential/make_registry.py --check`. All passed. Not run, by the
brief: `scripts/check.sh`, `scripts/remote-check.sh` and the workspace suite. No real device or adb server was touched;
the `sh` replay runs on a temporary directory.

## Remaining limits

- The quoting and exit check were not run on a real device or an old Android shell; `--` with toolbox `rm` before
  Android 6 is from reading its `getopt` use, not tested.
- A code line cut inside its number, `-1` arriving as `-`, is caught; `-10` cut to `-1` would not be, but no runner prints
  such codes.

## Incidental bugs

- N41 (Kotlin): the after-batch removal of a pushed folder sends its path unquoted, with the same `/data` hazard for a
  name with a space on a root adb. Fixed in a separate last commit, after review round 2: `remove_pushed_path` quotes the
  path, as the push destination is quoted, only when it holds a character other than ASCII letters, digits and
  `/._-+,:@%`, so every ordinary path, and every recording and test, keeps Kotlin's command byte for byte. Its exit code
  is still not read. `the_removal_after_a_batch_quotes_only_a_path_that_needs_it_d27` failed before, with
  `rm -r /sdcard/payload data`.
- N42 (Kotlin, kept): pull entries' `rm -r` and `mkdir` before a batch, and the `run-as` moves of `APP_DATA` folders,
  are unquoted too. Kept for the parity of commands sent to devices.
