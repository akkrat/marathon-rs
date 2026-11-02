# marathon-rs

A native Rust rewrite of the Kotlin marathon fork frozen at `e47352f2`,
for running Android, iOS and macOS tests with the existing Marathonfile,
Marathondevices, CLI and report formats.

**Pre-release.** Production use requires the differential, real-device,
port-queue and isolated cutover gates; packaging checks alone do not certify
end-to-end parity. See [release status and requirements](docs/releasing.md#release-readiness)
and the published tag's release notes before migrating.

- [Fork notes and compatibility scope](docs/fork-notes.md)
- [Approved differences from Kotlin](docs/deviations.md)
- [Migration, configuration and rollback](docs/migration.md)

## Install

When a release is published, download the matching zip and `SHA256SUMS` from
that tag's GitHub Release. The two build targets are:

| Runner host | Asset |
|---|---|
| Apple Silicon macOS | `marathon-<tag>-aarch64-apple-darwin.zip` |
| x86_64 GNU/Linux | `marathon-<tag>-x86_64-unknown-linux-gnu.zip` |

`<tag>` is a placeholder for an actual published tag, including any `v` prefix.
There is no Windows, Intel Mac or universal build. Follow the
[checksum and extraction recipe](docs/migration.md#install-the-native-entry-point)
and its platform qualifications. The zip contains
`marathon-<tag>/bin/marathon`; no JVM is needed by the native runner.

## Quick start

After installing and adding its `bin` directory to PATH, use **your existing,
valid Marathonfile and configured test environment**. These run examples execute
tests; they are not installation checks. Check the release readiness requirements
for your vendor.

```sh
marathon version
marathon --help
# From the directory containing your Marathonfile:
marathon
# Or choose the configuration explicitly:
marathon run -m Marathonfile --analyticsTracking false --bugsnag false
```

`version` logs a startup line before printing the bare version. Root options do
not propagate to subcommands; place `-m` after `run` or `parse`. Boolean options
take a value. Telemetry flags remain accepted but are ignored; configured
Influx/Graphite analytics are separate. See the [migration guide](docs/migration.md)
before using `parse`, which can extract bundles and contact workers.

## Build and check

Use rustup with the repository's [pinned toolchain](rust-toolchain.toml)
(currently Rust 1.99.0), a native C linker/SDK and Python 3:

```sh
# Homebrew rustup, when it is not already on PATH:
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
rustup toolchain install
cargo build --locked --release -p marathon-cli --bin marathon
./target/release/marathon version
scripts/check.sh
```

The gate runs formatting, Clippy, workspace tests and repository audits.
Reference-source audits require the pinned
Kotlin checkout (point `MARATHON_KT` at it) and report skips when it is absent. A JDK is needed for developer
oracle work, not to launch the binary. For per-target archives, prerequisites
and tag-only GitHub publication, see [release operations](docs/releasing.md).
Branch builds do not publish; GitHub's latest release replaces the old promotion
scheme. A release binary's version equals its tag; untagged builds use the
existing fallback, which is not evidence that a release exists.

Derived from marathon and distributed under [GPL-2.0](LICENSE). Existing source
attribution and third-party notices remain in their respective files.
