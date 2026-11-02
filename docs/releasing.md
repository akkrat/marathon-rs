# Native release operations

WP 7.3 follows the final user decisions in `docs/wp/7.3.md`. GitHub Actions
replaces the Kotlin source's GitLab registry pipeline. No GitLab setup is needed.

## Local builds

Install the toolchain pinned in `rust-toolchain.toml`, a native C linker/SDK,
Python 3, `file`, and `unzip`. Add Rust to PATH when needed:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
rustup toolchain install
# On an Apple Silicon Mac:
python3 -I scripts/release/release.py build --target aarch64-apple-darwin \
  --version v1.2.3 --output 'dist/macOS release'
# On x86_64 GNU/Linux:
python3 -I scripts/release/release.py build --target x86_64-unknown-linux-gnu \
  --version v1.2.3 --output 'dist/Linux release'
```

Commands must run on the selected native target. `--version` defaults to the
present `GIT_TAG_NAME` environment value, otherwise `0.10.1`. Release tooling
accepts 1–128 ASCII characters: first alphanumeric, then alphanumeric or
`._+-`, with no `..`. It rejects an empty value, separators and control
characters. It does not strip `v` or require semver. Direct Cargo compilation
retains the CLI's verbatim environment/fallback semantics.

The command runs `cargo build --locked --release -p marathon-cli --bin marathon
--target <target>` with the resolved version as `GIT_TAG_NAME`. macOS explicitly
sets `MACOSX_DEPLOYMENT_TARGET=11.0`. Cargo tracks `option_env!` changes; no
build script is needed. Cargo's package version `0.0.0` is unrelated.

Outputs per target are `marathon-<version>-<target>.zip` and `<target>.json`.
The archive contains only `marathon-<version>/bin/marathon`, a regular file with
Unix mode 0755. Parent directories are created by unzip. Assets currently used
by the binary are compiled into it; there is no JVM launcher or JAR payload.
Future resource-owning WPs must keep required assets embedded or update this
manifest before releasing. The metadata records source SHA, binary and archive
SHA-256, version, compiler, native dependency inspection and platform commands.
Existing outputs are rejected. Use a new output directory per build.

Every build unzips into a fresh path with spaces, verifies the executable mode,
and checks `version` and `--help` against the existing CLI oracle matrix. The
process runs from an unrelated cwd with an empty PATH, nonexistent JAVA_HOME,
conflicting runtime GIT_TAG_NAME, JAVA_OPTS and MARATHON_OPTS. There is no wrapper
that changes arguments, cwd or startup output. To package an existing binary
and run the same checks:

```sh
python3 -I scripts/release/release.py package --target aarch64-apple-darwin \
  --version v1.2.3 --binary target/aarch64-apple-darwin/release/marathon \
  --output 'dist/existing binary'
python3 -I scripts/release/test_release.py
python3 -I scripts/release/check_versions.py aarch64-apple-darwin
```

The last command builds the fallback and two different tags using the same
Cargo cache and checks each extracted result with a conflicting runtime value.
It works on Linux with that target substituted. No real workers are contacted.

## Static Linux (musl) target

`x86_64-unknown-linux-musl` is a fully static binary for any x86_64 Linux. It
builds on a Linux x86_64 host with `cargo zigbuild`, so it needs, on `PATH`,
`zig` and `cargo-zigbuild` (pinned versions and install steps:
`docs/wp/7.3b-findings.md`) and the Rust target
(`rustup target add x86_64-unknown-linux-musl`):

```sh
python3 -I scripts/release/release.py build --target x86_64-unknown-linux-musl \
  --version v1.2.3 --output 'dist/Linux musl release'
```

The command refuses a binary that `file`/`ldd` do not report as static. Output
naming, layout, smoke checks and metadata match the other targets; the metadata
adds the zig and cargo-zigbuild versions. `publish` attaches this archive when
it is among the downloaded artifacts.

## Platforms and tools

macOS output requests macOS 11.0 or later on arm64. The observed local binary's
LC_BUILD_VERSION is 11.0, SDK 27.0, with dependencies on CoreFoundation,
libiconv.2 and libSystem.B only. This is a linker deployment floor, not a
certification that an old OS or real workers have passed tests. The default
Actions macOS runner is `macos-14` (arm64).

The default Linux runner is `ubuntu-22.04` on x86_64 (glibc 2.35). Treat its OS
and libc as the release build baseline until the first native build's `ldd`
and `readelf --version-info` metadata and smoke results establish the actual
symbol requirements. No Linux dependency/minimum-libc observation was made on
this Mac. A newer self-hosted runner may raise requirements; review its
metadata before release. GNU libc is required; no musl substitution is made.
Linux builds need `ldd` and `readelf`; macOS builds need `otool`.

## Release readiness

`version` and help need no Java, configuration, device or external executable.
Running tests later requires tools appropriate to the configured vendor: an
ADB server/devices for Android; Apple's development tools on Apple workers;
`rsync`, and `sshpass` for password-based rsync. The CLI uses `ProductionRunner`:
Android and macOS vendors are wired, and iOS vendor wiring is supplied by
WP 5.5b. An iOS build must include that work package.

Packaging checks validate the archive and entry point; they do not establish
release readiness. A successful archive smoke does not satisfy WP 7.2, 7.5,
7.6 or PLAN §10 release gates. Those validation and cutover gates remain
prerequisites before publishing a production release. Consult the published
tag's release notes for the validation completed on that release.

## GitHub setup and policy

The orchestrator and user configure the GitHub repository and enable Actions.
The committed workflow makes no repository, tag, secret, runner or release by
itself. `.github/workflows/ci.yml` runs the full `scripts/check.sh`, release
helper tests, version/cache checks and native package smoke on both platforms.
Each target's checked artifacts are kept for 14 days. Branches and PRs test
and build the fallback version; only a **tag push** can publish. Manual workflow
runs cannot publish. Source checkout credentials are not persisted.

Optional repository variables `MARATHON_MACOS_RUNNER` and
`MARATHON_LINUX_RUNNER` contain JSON label arrays, for example
`["self-hosted", "macOS", "ARM64"]`. Defaults are `macos-14` and
`ubuntu-22.04`. Native architecture is checked before building. The workflow
provisions CLI prerequisites and the pinned Rust toolchain; self-hosted
machines need rustup, Homebrew or apt/sudo, Python, the native SDK/linker and
GitHub CLI (`gh`). Provisioned packages include rsync, sshpass and xmllint; the gate
uses fake/offline workers, with no real-host credentials.

With no Kotlin source checkout the gate explicitly skips
source inventory, parity and copied-fixture validation. Offline committed
oracle validation and the Rust workspace gate still run. Full local and Linux
orchestrator gates with the reference checkout are still required before merge.

The publish job waits for **both** target jobs. Its token has `contents: write`;
test/build jobs have read permission. It validates target, version, source SHA,
archive and binary hashes and archive modes, and derives `SHA256SUMS` plus
release notes listing both archives. The exact tested zips are attached to a
new draft through `gh release create --verify-tag --draft`, then the completed
release is published and explicitly marked GitHub latest. There is no rebuild,
branch-head query, registry namespace or mutable `latest/marathon.zip`.

All publication uses one Actions concurrency group. GitHub does not guarantee
FIFO ordering; latest means the last successfully promoted tag, with no semver
or commit monotonicity claim. Two asset uploads are not atomic. Failed uploads
leave a draft for operator review; reruns reject **any** existing draft or
published release for the tag, without replacing assets. An operator must
inspect and explicitly resolve that draft before retrying. Do not force tags
or silently overwrite release files. Protect release tags and limit release
write permissions; the workflow's serialization covers its own runs, not
manual external writers. GitHub keeps only one pending run per concurrency
group: if several tags are pushed while a publication runs, the middle ones
are skipped and have no release until an operator reruns their workflow.

The offline tests record publication command order and payload checks, reject
branch/PR/manual events, verify target isolation and corruption/missing-target
failures, reject existing drafts/releases, and stop before latest promotion on
API/upload errors. Live uploads, GitHub-hosted jobs and minimum-platform
certification require the configured infrastructure and were not performed
as part of this implementation.

References: [GitHub CLI create](https://cli.github.com/manual/gh_release_create)
(draft uploads and tag verification), [runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
