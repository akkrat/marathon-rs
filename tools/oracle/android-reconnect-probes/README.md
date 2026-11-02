# x1: Android device that reappears with the same serial

`generate` writes the strict fake-adb scripts (`fixtures/fake-adb/android-reconnect/script`). `run` launches the Kotlin
and Rust CLIs against fresh loopback fakes per scenario (like `android-runner-probes/cli-scenarios`; no adb is invoked)
and writes `expected/`:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build -p fake-adb -p marathon-cli
python3 tools/oracle/android-reconnect-probes/generate
python3 tools/oracle/android-reconnect-probes/run              # all scenarios, both CLIs, ~7 minutes
python3 tools/oracle/android-reconnect-probes/run same-server --impl kotlin
python3 tools/oracle/android-reconnect-probes/run --resummarize  # recompute `compare` from the stored transcripts
(cd fixtures/fake-adb/android-reconnect && shasum -a 256 script/* expected/* > SHA256SUMS)
```

The CLIs run in real time (a test takes four real seconds in the fake), so the timeline events land a few seconds after
marathon starts; the scripts keep the second emulator out of the remote parser's way. The Rust test
`crates/marathon-android/tests/reconnect_test.rs` runs the same scripts in virtual time and compares its outcome with
the Kotlin `compare` rows. See `docs/wp/x1-findings.md`.
