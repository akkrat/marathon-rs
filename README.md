# marathon-rs

A native Rust rewrite of the Kotlin marathon fork frozen at `e47352f2`,
for running Android, iOS and macOS tests with the existing Marathonfile,
Marathondevices, CLI and report formats, apart from the approved differences in
[the deviations guide](docs/deviations.md). A Marathonfile must set a finite `uncompletedTestRetryQuota`.

**0.13.0 is the current release; 0.11.0 was the first public release.** 0.13.0 drops JVM artefacts under the Phase 8
contract (D55–D65) and changes SIGQUIT and console completion handling (D66, D67). The Kotlin tests are ported or marked not
applicable with a reason, differential suites compare whole runs with the JVM program, and 0.13.0's release candidates ran on
real iOS, macOS and Android hosts, as the changelog records. Outside this repository the maintainer also ran it beside the
JVM build on private CI farms, which cannot be checked here.
No isolated shadow or canary results are recorded in this repository. Trial it in a
disposable environment and keep your JVM distribution for rollback. See the
[changelog](CHANGELOG.md) for known limits and
[release readiness](docs/releasing.md#release-readiness) for what the evidence covers.

- [Fork notes and compatibility scope](docs/fork-notes.md)
- [Approved differences from Kotlin](docs/deviations.md)
- [Migration, configuration and rollback](docs/migration.md)

## Install

Download the matching zip and `SHA256SUMS` from a version's GitHub Release. Each
release's notes carry the exact commands. There are two assets:

| Runner host | Asset |
|---|---|
| Apple Silicon macOS | `marathon-<version>-aarch64-apple-darwin.zip` |
| x86_64 Linux, any distribution (static binary) | `marathon-<version>-x86_64-unknown-linux-musl.zip` |

Versions and tags have no `v` prefix, for example `0.11.0`. There is no Windows,
Intel Mac, Linux on ARM or universal build. Follow the
[checksum and extraction recipe](docs/migration.md#install-the-native-entry-point)
and its platform qualifications. The zip contains `marathon-<version>/bin/marathon`,
`LICENSE` and `THIRD-PARTY-LICENSES`; no JVM is needed by the native runner.

The macOS binary is not notarised, so a browser download is quarantined and hangs when
run. Download with `curl` or `gh`, or clear it with `xattr -d com.apple.quarantine` before
the first run; see the [recipe](docs/migration.md#install-the-native-entry-point).

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

Derived from marathon. The source is distributed under [GPL-2.0-only](LICENSE). Existing
source attribution and third-party notices remain in their respective files, and each
release archive carries a generated `THIRD-PARTY-LICENSES` for the crates and other files
compiled in.

`libxctest-parser` is excluded. This repository and its archives do not contain it, and
this repository's licence does not cover it. It is third-party software under its own
EULA, needed only by runs that select the `xctest` test parser, which use a copy you supply
([D20](docs/deviations.md#d20--the-xctest-parser-library-is-supplied-by-you)).
