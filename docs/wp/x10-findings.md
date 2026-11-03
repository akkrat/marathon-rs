# X10: a release workflow for the first public release

This WP prepares the release of 0.99.0 and publishes nothing. No tag, release, draft,
environment, variable, secret or repository setting was created or changed.

## Design

**One gate.** `.github/workflows/gate.yml` is a reusable workflow holding the gate that
`ci.yml` used to define: `scripts/check.sh`, failure evidence, the version cache checks
and the native package smoke, on `ubuntu-22.04` (gnu) and `macos-14`. `ci.yml` and
`release.yml` both call it, so neither repeats its logic. Gate job names change to
`gate / test-build (...)`.

**Release jobs.** `docs/releasing.md` has the table. `release.yml` runs `preflight`, then
`no-existing-release` (tag only), `gate` and `build`, then `musl-proof`. A tag then runs
`publish` and `verify`; anything else runs `dry-run`.

Design choices the brief left open:

- **Drafts need a write token.** The releases API lists drafts only for tokens with push
  access, so a `contents: read` preflight would miss the draft it is meant to find. The
  draft check is therefore its own tag-only `no-existing-release` job with
  `contents: write`, running after the read-only `preflight`. `publish` checks again.
- **Crate selection.** `third_party_licenses.py` takes the crate set from
  `cargo tree -e normal` and the licence data from `cargo metadata --locked`. Metadata
  alone resolves features across the whole workspace, so it could list crates that the
  release build does not compile. Both commands come with cargo, so there is no new tool.
- **Rust musl target.** `rustup target add` in `.github/actions/setup-musl-tools`, not
  `rust-toolchain.toml`, so local gates and other builds do not download it.
- **Cache.** `ci.yml` caches cargo-zigbuild. Release builds pass no cache flag and install
  it themselves (35 s), so a release binary never depends on a cache another run wrote.
- **Annotated tags.** `preflight` and `publish` peel `GITHUB_SHA` with `^{commit}`, because
  the metadata and the ancestry check are about the commit, not a tag object.
- **Release notes links.** `CHANGELOG.md` uses repository-relative links, which break on a
  release page, so `collect` rewrites them to the tag's blob URLs.
- **CLI proof.** `prove-musl` reuses the extracted smoke's comparison with the CLI matrix,
  and checks that the container has the musl loader and no glibc loader.

## Evidence

**Dry run.** Run `41725314010`, head `7a52384f`, on GitHub-hosted runners, through a
temporary branch-push trigger treated as a dry run of 0.99.0.

| Job | Result | Time |
|---|---|---|
| `preflight` | success | 29 s |
| `build (aarch64-apple-darwin)` | success | 4 m 22 s |
| `build (x86_64-unknown-linux-musl)` | success, zig tarball `OK` against its SHA-256 | 5 m 46 s |
| `gate` on macos-14 and ubuntu-22.04 | success | 18 m 50 s, 19 m 31 s |
| `musl-proof` | success | 25 s |
| `dry-run` | success | 17 s |
| `no-existing-release`, `publish`, `verify` | skipped | |

The trigger was then removed. Against that run's file, the final `release.yml` differs only
in the `branches:` filter and the `VERSION` expression.

Those three jobs cannot run on any event but a tag push. Each has
`if: github.event_name == 'push' && github.ref_type == 'tag'`, and `verify` also needs
`publish`. The only other trigger is `workflow_dispatch`, and `release.py publish` separately
refuses anything but a tag push. The preflight log shows the checkout fetching
`+refs/heads/*:refs/remotes/origin/*` and `main -> origin/main`, which the tag ancestry check
needs.

**Dry-run artifact, inspected in a fresh directory.**

| Asset | SHA-256 | Crates listed |
|---|---|---|
| `marathon-0.99.0-aarch64-apple-darwin.zip` | `35ec9831958e8e11c2e3ec2a41ea615409ee7cdb9f4736867ad26a3956364ffd` | 256 |
| `marathon-0.99.0-x86_64-unknown-linux-musl.zip` | `10edc563438d4960ab6fd7afecd71058e915b0388f2b6ed9c7c4c785943e115a` | 255 |

- Each zip holds exactly `bin/marathon` (0755), `LICENSE` and `THIRD-PARTY-LICENSES` (0644)
  under `marathon-0.99.0/`. The modes survive `unzip`, and the metadata's manifest digests match.
- `LICENSE` is identical to the repository's. `THIRD-PARTY-LICENSES` is about 2.5 MB, has no
  absolute paths, includes `vendor/russh` as vendored, and has 5 crates that state they
  have no licence file, with their SPDX expressions.
- The notes contain the matching changelog section with absolute links, the asset table,
  source commit, install, checksum and attestation commands, and the macOS note.
- The musl binary is `ELF 64-bit LSB executable, x86-64, statically linked, stripped`; its
  metadata records zig 0.17.0 and `cargo-zigbuild 0.24.0`.
- The darwin binary, run with `env -i`, prints `0.99.0` for `version` (rc 0) and prints usage
  for `--help`. It links Security, CoreFoundation, libiconv.2 and libSystem.B, and its
  LC_BUILD_VERSION is minos 11.0, SDK 14.5.

**macOS signature and quarantine (macOS 27.2, arm64).** The binary reports `Signature=adhoc`,
flags `adhoc,linker-signed`, no team identifier, and `codesign -v` passes.

| Check | Result |
|---|---|
| `curl -fLO` and `gh release download` of a real release asset | no `com.apple.quarantine` |
| `unzip` or `ditto -xk` of a zip carrying the mark | the binary gets the mark |
| Binary with no mark | runs |
| Binary with the mark | no output and no exit; killed after 45 s |
| Fresh copy, marked, cleared with `xattr -d` before its first run | runs |
| Mark cleared from the zip before `unzip` | runs |
| The copy that was launched while marked, after `xattr -d` | still blocked; killed after 20 s |

So the docs say the binary "hangs" rather than "is refused", and that the mark must be cleared
before the first run. `spctl --assess` rejects the binary with or without the mark, so it says
nothing about this. The mark was set by hand in Safari's format; no browser was driven and
Archive Utility was not tried. A cause for the last row was not established: it may be a
pending Gatekeeper prompt rather than a lasting state, so the docs say "can stay blocked".

**Publish command lines, from the offline test's recorded calls.**

```
gh api repos/{owner}/{repo}/releases --paginate --slurp
gh release create 0.99.0 dist/aarch64-apple-darwin/marathon-0.99.0-aarch64-apple-darwin.zip dist/x86_64-unknown-linux-musl/marathon-0.99.0-x86_64-unknown-linux-musl.zip dist/SHA256SUMS --verify-tag --draft --title 0.99.0 --notes-file dist/release-notes.md
gh release edit 0.99.0 --draft=false --latest
```

For `0.99.0-rc.1`, `create` adds `--prerelease`, and `edit` is
`--draft=false --prerelease --latest=false`. The test's `dist/` stands in for the
downloaded artifact directory, whose real paths have a `release-` prefix on the target.

**Offline tests.** `python3 -I scripts/release/test_release.py`: 60 tests, up from 18.

**Final CI.** The two `ci.yml` runs of the final head are in the implementation report,
because recording them here would change the head they ran on.

## Tool and action versions

Every action SHA was checked against its tag through `gh api`.

| Tool | Version | Pin |
|---|---|---|
| `actions/checkout` | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| `actions/upload-artifact` | v7.0.2 | `cf430e030ddbb5b0abf93d22962f4752f3646cd9` |
| `actions/download-artifact` | v8.0.2 | `9000827ccba6bdab643e8b6fd33ac0654aef8333` |
| `actions/cache` | v6.1.0 | `55cc8345863c7cc4c66a329aec7e433d2d1c52a9` |
| `actions/attest-build-provenance` | v4.2.2 | `4d101475d8b20a2381f78447822ac1eab6504dd8` |
| Alpine image | 3.24.3 | `sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a` |
| zig | 0.17.0 | tarball SHA-256 `1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026` |
| cargo-zigbuild | 0.24.0 | `cargo install --locked` |
| actionlint | 1.7.12, darwin arm64 release binary, checksum verified, run locally | clean on all workflows; shellcheck was not available to it |

`differential.yml` keeps its unpinned actions: it is not called by `release.yml`, and this
WP removes only its tag trigger.

## Limitations

- **The tag path has never run.** `no-existing-release`, the ancestry branch of `preflight`,
  attestation, `publish` and `verify` are covered only by offline tests and by reading the
  workflow, because exercising them would publish. The first real tag is their first run.
- **`workflow_dispatch` has not run.** GitHub accepts a manual run only for a workflow already
  on the default branch, so the verification used a branch push. `gh workflow run release.yml`
  first works after merge, which is why the runbook's dry run comes before the first tag.
- **Draft visibility** comes from GitHub's API documentation. It was not tried, since that
  would have needed a draft.
- **Environment.** The skipped `publish` job did not create the `release` environment: the
  repository still has none after the dry run.
- **Evidence for the musl binary** is `version` and `--help` only, as in WP 7.3b.
- **`THIRD-PARTY-LICENSES` lists crates only,** as specified, and its header says so. The HTML
  and timeline report assets and the `libxctest-parser` dylibs, copied from the Kotlin source and
  compiled in, are not listed.
- **Paths in binaries.** Both release binaries contain build paths: the runner's cargo registry
  paths in panic locations, and an upstream developer's path inside the embedded
  `libxctest-parser` dylibs copied unchanged from the Kotlin source. Changing the release profile
  is out of scope here.
- **PLAN §11 decision 5** still names the gnu target. It is a dated decision that WP 7.3b's user
  decision supersedes, so it was left alone.

## Incidental bugs

Both are in [found-bugs.md](../found-bugs.md):

- U148: `differential.yml` ran on every tag on self-hosted runners this repository does not
  have. The tag trigger is removed.
- U149: `release.py` dropped the stderr of failed tool calls, hiding why `gh` refused something.
  The error handler now prints it.

Also seen, not fixed: `docs/fork-notes.md` points to the README's "dated status", which the
README does not have.
