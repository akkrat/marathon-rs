# marathon-rs

A Rust rewrite of marathon, a test runner for Android, iOS and macOS. The source is a fork of marathon, frozen at commit `e47352f2`, checked out at `$MARATHON_KT` (read freely, never modify). `PLAN.md` is the plan; work happens in the work packages (WPs) of §7, each with a brief in `docs/wp/<id>.md`.

## Ground rules

- **Behaviour parity.** The external interface stays exactly as it is: Marathonfile and Marathondevices formats, CLI, exit codes, output files, logs, and the commands sent to workers (PLAN.md §3).
  - Port what the Kotlin code *does*, including its bugs.
  - When you reproduce a Kotlin bug, add or update its row in `docs/known-kotlin-bugs.md`.
  - The only intentional differences are D1–D28 (PLAN.md §3.11).
- **Name the source.** Each ported module starts with `//! Port of <kotlin path relative to the marathon repo>`.
- **Java semantics live in `jcompat`.** Use `JavaPriorityQueue`, never `BinaryHeap`, for test queues; use the jcompat regex wrapper over the Rust `regex` crate for user-supplied regexes (D6); use the jcompat formatters for anything that reaches an output file or log.
- **Concurrency** follows `docs/design/lifecycle.md` once it exists (WP 0.6).
  - No blocking calls on the core executor thread.
  - No `unwrap()`/`expect()` on I/O, process or network results; they return errors.
- **Tests.** Port every Kotlin test listed for your WP in `docs/parity-matrix.csv`, under the Kotlin test's name in snake_case, and set its row to `ported` with the Rust test path. A row may be set to `n/a` only with a reason, and a reviewer must agree. Real-time tests follow the timing rules in `docs/testing.md`.
- **Fixtures.** Test resources live under `fixtures/`, copied unchanged from the Kotlin repo. `fixtures/PROVENANCE.md` records where each came from. Never edit a copied fixture; add a new file instead.
- **Comments.** Match the surrounding density. Explain *why* (a Kotlin quirk being kept, a protocol detail), not *what*.

## Toolchain and commands

Rust comes from Homebrew's `rustup` and is not on the default `PATH`:

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
```

- `scripts/check.sh`: the gate. It runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`, and checks the plan graph, the source inventory and the parity matrix.
- `scripts/remote-check.sh`: runs `check.sh` for the current HEAD on the Linux build box over SSH. The host comes from `~/.config/marathon-rs/remote.env`. The orchestrator runs it before every merge.
- `scripts/source_inventory.py <kotlin repo>`: regenerates `docs/source-inventory.csv` (every Kotlin main file → WP).
- `scripts/check_plan_dag.py`: validates the WP graph in PLAN.md §7.

A reference JDK for oracle work is Android Studio's: `/Applications/Android Studio.app/Contents/jbr/Contents/Home`.

## Git

- Work on a branch named `wp/<id>-<slug>`, usually in your own worktree. Never commit to `main`; the orchestrator merges after review.
- Keep commits focused. End every commit message with:

  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  ```

- Before saying you are done, run `scripts/check.sh` and report its result as it is.
