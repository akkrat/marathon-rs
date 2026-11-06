# Native release operations

GitHub Actions replaces the Kotlin source's GitLab registry pipeline. A release
is a pushed tag, `.github/workflows/release.yml` does the rest, and a manual run
of the same workflow rehearses everything except publication. The tag format and
assets follow the user decisions in `docs/wp/7.3.md` and `docs/wp/7.3b.md`.

## Cutting a release

1. **Changelog.** Rename the `## Unreleased` section of `CHANGELOG.md` to
   `## <version>`, and carry its *Evidence* and *Known limits* sections forward,
   changing them to what is true for this release. Reword the first sentence of
   `README.md`, which names the first public release. It becomes the top of the
   release notes. Merge it to `main`.
2. **CI green on main.** The `CI` workflow on the commit to release has passed,
   including its `musl-package` job.
3. **Dry run.** This runs the gate, both asset builds and the Alpine proof, and
   publishes nothing:

   ```sh
   gh workflow run release.yml --ref main -f version=<version>
   gh run list --workflow release.yml --limit 1   # note the run id
   gh run watch <run id> --exit-status
   gh run download <run id> --name dry-run-<version> --dir <fresh directory>
   ```

   Read `release-notes.md`, check `SHA256SUMS`, unzip both archives and run the
   one for your machine.
4. **Tag and push.** The tag is the version, with no `v`:

   ```sh
   git switch main && git pull --ff-only
   git tag -a <version> -m '<version>'
   git push origin <version>
   ```

5. **Watch it.** `gh run watch <run id> --exit-status`. The jobs run in this order:
   `preflight`, then `no-existing-release`, `gate` and `build` in parallel, then
   `musl-proof`, `publish` and `verify`. `dry-run` shows as skipped.
6. **Check the result.** The `verify` jobs passing is the signal. To check the
   provenance from your own machine, download an archive from the release, then run
   `gh attestation verify <archive> --repo <owner>/<repo>`.

## Failure recovery

Nothing is public until the last command of `publish`, so everything before it
can be retried.

| What failed | What to do |
|---|---|
| `preflight` (bad version, no changelog section, unresolved asset status, tag not on `main`) | Nothing was built or published. Delete the tag (`git push origin :refs/tags/<version>`), fix the cause through a normal WP, and tag again. |
| `no-existing-release` | A release or draft already holds this tag. If it is a leftover draft, treat it as a failed upload, below. A published release is never replaced: use the next version. |
| A flake in `gate`, `build` or `musl-proof` | `gh run rerun <run id> --failed`. Successful jobs and their artifacts are kept. Do not move the tag. Record the flake. |
| Upload or `publish` failed | First run `gh release view <version> --json isDraft,assets`: the last command can fail after GitHub has already published. If `isDraft` is `false`, the release is public, so do not delete it: check its assets against `SHA256SUMS` by hand as `verify` would, and treat any mismatch as a bad published release. Only if `isDraft` is `true` (a rerun refuses to continue while it exists), delete it with `gh release delete <version> --yes` (never `--cleanup-tag`), then `gh run rerun <run id> --failed`. A rerun attests the same archives a second time, which is harmless. |
| A bad published release | Never move the tag and never replace its assets. Add a warning to its notes with `gh release edit <version> --notes-file <file>` and ship the next patch version. |

### A failed verify job

`verify` runs after publication, so its failure cannot undo the release. The
release is already public and may already be downloaded. Read the failing step:

- **Infrastructure** (a download or API error, nothing about the assets): rerun with
  `gh run rerun <run id> --failed`.
- **Checksum, asset list, release state or version mismatch:** treat it as a bad
  published release, as above. Do not edit the assets to make the job pass.

## Repository settings

These two settings live on GitHub, not in this tree, and a new repository needs them set
again:

- **The `release` environment** that `publish` uses accepts deployments only from
  version-shaped tags: a custom deployment policy of type tag with the pattern
  `[0-9]*.[0-9]*.[0-9]*`. No branch and no other tag can deploy to it.
- **The ruleset "Release tags cannot move"** targets all tags, with no bypass, and blocks
  updates and force-pushes. It deliberately allows deletion, because the recovery table above
  deletes a tag after a failed `preflight`. A published release is never replaced, so do not
  delete the tag of one.

## What the workflow does

| Job | Runs on | Purpose |
|---|---|---|
| `preflight` | every run | Strict version (`X.Y.Z` or `X.Y.Z-prerelease`, no `v`), a `CHANGELOG.md` section for exactly this version, and no embedded asset with an unresolved status. On a tag, also that the tag is the version and its commit is an ancestor of `origin/main`. |
| `no-existing-release` | tag push | No release or draft for the tag. It has `contents: write` because a token that can only read does not see drafts. |
| `gate` | every run | `gate.yml`, the same reusable workflow `ci.yml` calls: the full `scripts/check.sh` and packaging checks on `ubuntu-22.04` (gnu) and `macos-14`, at this commit. |
| `build` | every run | One archive per asset with the release version: `aarch64-apple-darwin` on `macos-14`, `x86_64-unknown-linux-musl` on `ubuntu-22.04`. Includes the extracted smoke. |
| `musl-proof` | every run | From the zip artifact, not the build tree: `version` and `--help` in an Alpine image pinned by digest, with no network, compared with the CLI matrix. The image is `ALPINE_IMAGE` in `release.py`. |
| `dry-run` | not a tag push | `release.py prepare`, then uploads the archives, metadata, `SHA256SUMS` and notes as the artifact `dry-run-<version>`. |
| `publish` | tag push, after all of the above | Rechecks the archives, attests each with `actions/attest-build-provenance`, creates a draft, attaches the assets and publishes. Uses the `release` environment and one concurrency group. |
| `verify` | tag push, after `publish` | On `macos-14` and `ubuntu-22.04`: checks the release state and asset list, downloads with `gh release download`, checks `SHA256SUMS`, unzips and compares `marathon version` with the tag. |

`release.py publish` refuses any event other than a tag push, and the job's `if`
says the same, so a manual run cannot publish, even one started from a tag. A plain
version is marked GitHub latest. A version with a `-` suffix is published with
`--prerelease --latest=false`. The exact tested zips are attached; there is no
rebuild and no mutable `latest/marathon.zip`.

The token has `contents: read` by default. Only `no-existing-release` and `publish`
have `contents: write`, and only `publish` has `id-token: write` and
`attestations: write`. Checkouts do not persist credentials, every job has a timeout,
every action in these workflows is pinned to a full commit SHA, and event and input
values reach scripts only through `env`. Release builds do not restore the Actions
cache. `differential.yml` is manual only: it needs self-hosted runners.

All publication uses one Actions concurrency group. GitHub does not guarantee
FIFO ordering: latest means the last successfully promoted tag, with no semver or
commit monotonicity claim. GitHub keeps only one pending run per concurrency group,
so if several tags are pushed while a publication runs, the middle ones are skipped
and have no release until an operator reruns their workflow. The serialization
covers this workflow's own runs, not manual writers. Two asset uploads are not atomic,
which is why a draft comes first: a partial upload never becomes latest. Reruns reject
**any** existing draft or published release for the tag, without replacing assets.

`CI` runs the same gate on every branch and pull request, builds the musl archive
with the placeholder version `0.0.0-ci`, and runs the same Alpine proof. Each target's
artifacts are kept for 14 days. The repository variables `MARATHON_MACOS_RUNNER` and
`MARATHON_LINUX_RUNNER` may hold a JSON label array, such as
`["self-hosted", "macOS", "ARM64"]`, to move the gate's jobs. Release assets are
always built on the GitHub-hosted runners named above.

With no Kotlin source checkout the gate explicitly skips source inventory, parity
and copied-fixture validation. Offline committed oracle validation and the Rust
workspace gate still run. Full local and Linux gates with the reference
checkout are still required before merge.

The offline tests in `scripts/release/test_release.py` record the exact `gh` command
lines and cover strict versions, the required targets, gnu exclusion, archive file
lists and modes, the third-party file, notes, prerelease and latest arguments, the
refusal of non-tag events, and a dry run that never calls `gh`.

## Archive contents

Each asset is `marathon-<version>-<target>.zip` and holds exactly:

| Path | Mode | Contents |
|---|---|---|
| `marathon-<version>/bin/marathon` | 0755 | The executable. Assets it uses are compiled in; there is no JVM launcher or JAR payload. |
| `marathon-<version>/LICENSE` | 0644 | This repository's licence. |
| `marathon-<version>/THIRD-PARTY-LICENSES` | 0644 | Generated for that target. |

`scripts/release/third_party_licenses.py` writes the third-party file when the archive
is built. `cargo tree -e normal` selects `marathon-cli`'s dependencies for the target,
with the features a real build uses, and `cargo metadata --locked` gives each crate's
SPDX expression and source directory. It includes the vendored `vendor/russh`. It
excludes dev and build dependencies, and this repository's own crates, which `LICENSE`
covers. For each crate it writes the name, version and SPDX expression, and the text of
every `LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`, `COPYRIGHT*` and `UNLICENSE*` file
in its source, including those of bundled C libraries in subdirectories. A crate with no
such file gets a line saying so and giving its SPDX expression.

Files compiled in that are not crates get their own section, from
`scripts/release/embedded_assets.json` and the texts in `scripts/release/asset-licences/`:
the report bundles and the colour profiles. It also carries the notice of adam, which
`crates/marathon-adb` ports. `release.py build` compares that list with the non-Rust workspace
files in rustc's dep-info for the binary (`target/<target>/release/marathon.d`) and fails on any
difference, in either direction; this is the authoritative check, and a documentation include
counts too. An offline test also compares the list with the production `include_bytes!`,
`include_str!` and `include_dir!` calls, and fails on a path that is not a plain string literal. It
is only the fast check, because it reads source text. So a new asset needs an entry, with an
upstream, a licence text and a status, before it can be released.

`release.py preflight`, which a tag and a dry run both start with, accepts the asset statuses
`open-source` and `redistributable` and refuses anything else. `redistributable` means the asset is
not strictly open source but its terms allow what we do, and its entry records the decision and who
made it. A `review` entry blocks a release until its terms are decided. Package builds and CI do not
check the status, so ordinary CI stays green.

The source is `GPL-2.0-only` (`LICENSE`). `libxctest-parser` is excluded: neither the source
tree nor an archive contains it, this repository's licence does not cover it, and an `xctest`
run uses a copy that the user supplies (D20). Do not add it or its EULA to an archive.

Future resource-owning WPs must keep required assets embedded or update this manifest
before releasing. The `<target>.json` metadata beside each archive records the source
SHA, the digest and mode of every archive entry, the archive SHA-256, the version, the
compiler, native dependency inspection and platform commands. Existing outputs are
rejected, so use a new output directory per build.

## Local builds

Install the toolchain pinned in `rust-toolchain.toml`, a native C linker/SDK,
Python 3, `file`, and `unzip`. Add Rust to PATH when needed:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
rustup toolchain install
# On an Apple Silicon Mac:
python3 -I scripts/release/release.py build --target aarch64-apple-darwin \
  --version 1.2.3 --output 'dist/macOS release'
# On x86_64 GNU/Linux. This build is not a release asset:
python3 -I scripts/release/release.py build --target x86_64-unknown-linux-gnu \
  --version 1.2.3 --output 'dist/Linux release'
```

Commands must run on the selected native target. `--version` defaults to the present
`GIT_TAG_NAME` environment value, otherwise `0.10.1`. Local builds accept 1–128 ASCII
characters: first alphanumeric, then alphanumeric or `._+-`, with no `..`. They reject
an empty value, separators and control characters, and they do not strip `v` or require
semver. Only `preflight`, `prepare`, `publish` and `verify` require the strict release
format. Direct Cargo compilation retains the CLI's verbatim environment and fallback
semantics.

The command runs `cargo build --locked --release -p marathon-cli --bin marathon
--target <target>` with the resolved version as `GIT_TAG_NAME`. macOS explicitly
sets `MACOSX_DEPLOYMENT_TARGET=11.0`. Cargo tracks `option_env!` changes; no
build script is needed. Cargo's package version `0.0.0` is unrelated.

Every build unzips into a fresh path with spaces, verifies the modes and the licence
files, and checks `version` and `--help` against the existing CLI oracle matrix. The
process runs from an unrelated cwd with an empty PATH, nonexistent JAVA_HOME,
conflicting runtime GIT_TAG_NAME, JAVA_OPTS and MARATHON_OPTS. There is no wrapper
that changes arguments, cwd or startup output. To package an existing binary
and run the same checks:

```sh
python3 -I scripts/release/release.py package --target aarch64-apple-darwin \
  --version 1.2.3 --binary target/aarch64-apple-darwin/release/marathon \
  --output 'dist/existing binary'
python3 -I scripts/release/test_release.py
python3 -I scripts/release/check_versions.py aarch64-apple-darwin
```

The last command builds the fallback and two different tags using the same
Cargo cache and checks each extracted result with a conflicting runtime value.
It works on Linux with that target substituted. No real workers are contacted.

## Static Linux (musl) target

`x86_64-unknown-linux-musl` is the Linux release asset: a fully static binary for any
x86_64 Linux. It builds on a Linux x86_64 host with `cargo zigbuild`, so it needs `zig`
and `cargo-zigbuild` on `PATH` and the Rust target
(`rustup target add x86_64-unknown-linux-musl`). The Rust target stays out of
`rust-toolchain.toml`, so other builds do not download it. CI pins both tools in
`.github/actions/setup-musl-tools/action.yml`, checking the zig tarball by SHA-256; the
manual install steps are in `docs/wp/7.3b-findings.md`.

```sh
python3 -I scripts/release/release.py build --target x86_64-unknown-linux-musl \
  --version 1.2.3 --output 'dist/Linux musl release'
python3 -I scripts/release/release.py prove-musl --version 1.2.3 \
  --archive 'dist/Linux musl release/marathon-1.2.3-x86_64-unknown-linux-musl.zip'
```

The build refuses a binary that `file`/`ldd` do not report as static. Output naming,
layout, smoke checks and metadata match the other targets; the metadata adds the zig
and cargo-zigbuild versions. `prove-musl` needs Docker.

## Platforms and tools

macOS output requests macOS 11.0 or later on arm64. The `macos-14` (arm64) release
runner's binary has LC_BUILD_VERSION 11.0, SDK 14.5, with dependencies on Security,
CoreFoundation, libiconv.2 and libSystem.B only. This is a linker deployment floor, not
a certification that an old OS or real workers have passed tests.

The macOS binary is ad-hoc signed by the linker and not notarised, since that needs an
Apple Developer account. Each release's notes and `docs/migration.md` explain the
quarantine a browser download gets and how to clear it before the first run.

The Linux release binary is static, so it has no libc requirement. `ubuntu-22.04` on
x86_64 (glibc 2.35) remains the gnu gate platform. Linux builds need `ldd` and
`readelf`; macOS builds need `otool`.

## Release readiness

`version` and help need no Java, configuration, device or external executable.
Running tests requires tools appropriate to the configured vendor: an ADB server and
devices for Android; Apple's development tools on Apple workers; `rsync`, and
`sshpass` for password-based rsync.

Packaging checks validate the archive and entry point; they do not establish parity.
The whole-run evidence is in the findings of [WP 7.2](wp/7.2-findings.md) and
[WP 7.5](wp/7.5-findings.md), and it ran on an earlier commit than any release. WP 7.6's
isolated shadow and canary runs (PLAN §6.7) have no results recorded in this repository,
so the PLAN §10 gate on shadow runs is not shown to be met here. Consult a release's
notes for what was validated on it.

References: [GitHub CLI create](https://cli.github.com/manual/gh_release_create)
(draft uploads and tag verification), [runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
