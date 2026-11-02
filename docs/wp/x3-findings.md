# X3 — publication cleanup

Prepares the tree for export as a fresh public repository with a single squashed commit. This private repository
keeps its history; nothing here rewrites it.

## What was removed

- Internal tooling steps and references specific to the private development setup (CI steps, one-off helper scripts,
  comments and notes that mentioned them).
- `scripts/check.sh` runs an optional, untracked local hook when one exists
  (`${MARATHON_LOCAL_CHECK:-$HOME/.config/marathon-rs/local-check.sh}`).

## What was scrubbed

- Absolute home-directory paths, the Kotlin checkout path (now `$MARATHON_KT`; tool defaults use `~/src/marathon`),
  worktree paths, host aliases and machine names, internal project and ticket identifiers, and descriptions of the
  author's machines. Environments are described generically ("a remote Mac", "a Linux KVM box").
- The `ssh` alias in the spike scripts is now `remote-mac` (`spikes/targets/remote-mac-key.sh`); add a matching `Host`
  entry to run them.
- Generated evidence under `fixtures/` was rewritten with same-length replacements of the machine-specific home
  directories (`/Users/<name>` and `/home/<name>` became neutral placeholders of the same length), so recorded sizes are
  unchanged. This covers plain text, base64-encoded log payloads, gzip blobs, zip archive members (member order, names,
  timestamps and modes kept, CRCs recomputed, archive size kept) and the debug strings of the compiled sample binaries.
  Every recorded SHA-256, blob name and field-level digest was recomputed from the new bytes. Copied reference fixtures
  were not touched (`scripts/sync_fixtures.py` reports the same result as before).
- The sample binaries are fixture payloads that no test executes or code-sign-verifies.

## Exceptions

- Commit SHAs of the Kotlin source (`e47352f2`, `c95b2128`) are kept on purpose.

## Publishing

1. On a clean machine, `git clone` this repository, check out the release commit and run `scripts/check.sh`
   (set `MARATHON_KT` to a checkout of the pinned Kotlin source to include the source-dependent checks).
2. Export the tree without history: `git archive HEAD | tar -x -C ../marathon-rs-public`.
3. In the new directory: `git init -b main`, `git add -A`, one commit, add the new remote and push.
4. Tag the release as described in `docs/releasing.md`; CI builds and publishes from the tag.

Do not reuse this repository's remotes, branches or notes for the new one.
