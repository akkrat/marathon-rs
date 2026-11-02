# Android artifact listener evidence (WP 6.6b)

The original, unchanged Kotlin `e47352f2` classes are loaded from the read-only
CLI distribution. The compiler comes from the existing Gradle cache (Kotlin
1.9.24); execution uses Android Studio JBR 21.0.10 and coroutines 1.8.1.
HOME/TMPDIR are isolated, the SDK points at a nonexistent path, and the harness
never executes adb. Each fake binds an allocated loopback port other than 5037.

```sh
python3 tools/oracle/android-artifact-probes/run > fixtures/oracle/android-artifact-probes.jsonl
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cargo build -p fake-adb
python3 tools/oracle/android-artifact-probes/scenarios
cargo test -p marathon-android --test android_artifacts
```

`ArtifactProbe.kt` calls real listeners and the private `ScreenRecorder.grepPid`
through reflection, using a proxy device only to supply shell output. It records
five ps shapes and the five-kill grep-self loop; completed-job callback ordering
and same-thread execution; Commons Text null/non-null PID substitution; adam's
wrong logger and unsupported-format non-pull; screenshot assumption deletion;
and an actually suspended profiling job still active after `testRunFailed`.
The harness explicitly resumes/joins that last job after recording the leak.

`ScenarioProbe.kt` constructs a real AdamAndroidDevice and original artifact
listeners, runs setup against the existing strict fake, and records video,
video timeout/restart, screenshot GIF, the file-sync/coverage listener half,
and perfetto. Callback delivery uses a transcript barrier for recorder/perfetto
startup and the capturer's frame count for GIF completion. Kotlin is recorded
first; Rust reads the checked-in scripts and ordered request evidence. The
video-timeout case waits for the second recorder invocation before ending.

The fake models ADB transport/sync/framebuffer, not a complete Android shell:
`kill`, `rm`, `mkdir`, and `tar` expectations prove the exact issued commands;
they do not mutate fake processes/files. No new fake protocol was built. Perfetto
success uses a completed command, and its failure cleanup uses a hanging command
cancelled/joined by the Rust listener. This is an explicit limitation for 7.1's
full device-final-state gate, not evidence that Android kill/tar semantics match.

Synthetic MP4 bytes are pulled unchanged, which implies unchanged decoded media.
GIF verification checks one decoded RGBA frame, dimensions and delay against
`screenshot-kotlin.gif`, permitting the existing 3.9c palette difference (mean
absolute channel error below 12). It does not require identical GIF byte streams.
The assigned screenshot tests also reuse the copied source framebuffer fixture.
