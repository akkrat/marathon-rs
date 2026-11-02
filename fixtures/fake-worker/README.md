# Fake-worker fixtures and evidence

`scenario.json` is the reusable transport/file scenario. `generated.xctestrun` is the exact 285-byte payload written by the actual Kotlin boundary runner, not an edited Kotlin resource. `russh/` holds unchanged public upstream test literals; see `PROVENANCE.md` and `vendor/russh/README.marathon.md`.

Historical runs without a schema-2 terminal manifest remain readable but are rejected by the current comparator. Round-1 runs add declared suite/case/artifact inventories and preserve stdout/stderr bytes in normalized copies too.

The evidence under `evidence/<run>/raw` is curated from disposable loopback runs. `normalized` holds field-normalized copies; raw bytes are never rewritten. No private keys, server config, compiled classes, JARs or backing filesystem are included. All password arguments are explicitly labelled fixture credentials. Each `results.json` records outcomes, expected/actual exit codes and elapsed time; `versions.json` records exact tool output and binary SHA-256 hashes. Server transcripts contain lossless base64 payloads. Host `argv.jsonl` distinguishes original argv from isolation/askpass additions.

| Run | Platform and scope | Outcome |
|---|---|---|
| `macos-transfer` | macOS, latest transfer/auth/argv suite, including absolute and unsafe symlink preservation | 72 completed, 4 failed; runner exit 1 |
| `macos-faults` | macOS, full fault suite including long T15/T15h | 40 completed; runner exit 0 |
| `linux-full` | Linux box, complete client/transfer/fault suite including long T15/T15h | 113 completed; runner exit 0 |
| `round1-macos-standard` | macOS standard plus real-transfer containment regressions | 95 completed, 3 U4 failures; runner exit 1 |
| `round1-macos-faults` | macOS full faults, including T15/T15h | 40 completed; runner exit 0 |
| `round1-linux-standard` | Linux box standard plus real-transfer containment regressions | 97 completed; runner exit 0 |
| `round2-macos-standard` | macOS standard after TERM/transcript cancellation fix | 94 completed, 4 U4 failures; runner exit 1 |
| `round2-macos-faults` | macOS full faults after cancellation fix, including T15/T15h | 40 completed; runner exit 0 |
| `round2-linux-standard` | Linux box standard after cancellation fix | 97 completed; runner exit 0 |
| `linux-transfer` | Linux box, subsequent transfer suite including added absolute/unsafe symlink assertions | 75 completed; runner exit 0 |

In the historical runs, the four macOS failures are baseline sshpass attempts 05, 13, 15 and 18 (exit 255 instead of the fixture command's expected 7). They remain failures, with original stderr and server auth events. There were no retries. Historical baseline results are macOS 16/20 and Linux 20/20; the explicit fixture askpass workaround passes 10/10 on each platform and rejects the wrong password. U4 remains open. Counts are runner assertions/subprocess outcomes, not numbers of independent Rust tests.

macOS uses Apple openrsync protocol 29, OpenSSH 10.3p1, sshpass 1.06 and JBR 21.0.10. Linux uses Samba rsync 3.4.1 protocol 32, OpenSSH 10.2p1, sshpass 1.10 and OpenJDK 21.0.12.1. Samba evidence does not certify macOS parity. Known-hosts hashes were unchanged after success and failure paths on both platforms.

See `docs/wp/0.4a-findings.md` for exact reproduction commands and limits and `docs/design/fake-worker.md` for the frozen API. To curate a new run (destination must not exist):

```sh
python3 tools/fake-worker/validation/evidence.py curate tools/fake-worker/runs/new-run fixtures/fake-worker/evidence/new-run
python3 tools/fake-worker/validation/test_evidence.py
python3 tools/fake-worker/validation/evidence.py compare fixtures/fake-worker/evidence/new-run fixtures/fake-worker/evidence/new-run
```

The last command returns 1 for failed, timed-out, skipped, empty or missing results, even on a self-comparison. It deliberately returns 1 for `macos-transfer`. Raw/normalized evidence supports inspection and future differential clients; cross-platform protocol byte streams are not asserted equal.

Round-1 macOS baseline sshpass succeeds 17/20; attempts 05, 06 and 15 fail. Linux succeeds 20/20. Explicit askpass succeeds 10/10 on both. No password attempt is retried; U4 remains open. All 22 containment outcomes pass on both platforms.

Round-2 macOS baseline sshpass succeeds 16/20; attempts 00, 01, 10 and 19 fail.
Linux succeeds 20/20. Explicit askpass succeeds 10/10 and rejects the wrong password
on both; all 22 containment outcomes pass on both. New evidence is retained separately
and all failed outcomes remain ineligible for equality. `round2-checks` holds contract,
stress, deterministic regression, source-hash and gate logs; those logs are validation
records, not schema-2 real-client suites for `evidence.py compare`.
