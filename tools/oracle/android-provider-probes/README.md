# Android provider probes (WP 6.5)

`python3 tools/oracle/android-provider-probes/run` compiles AndroidProviderProbe.kt
against the reference CLI distribution, without modifying it. It probes sdklib's
AndroidVersion, Java address text, chunk lines, and the actual provider on tracking
EOF. The byte server remains available, so another subscription would be visible.

After `cargo build -p fake-adb`, `python3 tools/oracle/android-provider-probes/scenarios`
records the actual Kotlin provider/device/installer against strict fake servers in
`fixtures/fake-adb/android-install-matrix` and `android-provider-multi-server`.
No adb process is invoked. Scripts and observations are described in
`docs/wp/6.5-findings.md`. Rust replays them in `tests/scenarios_test.rs`.
