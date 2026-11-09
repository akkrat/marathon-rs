# marathon-rs

A Rust rewrite of marathon, a test runner for Android, iOS and macOS. The source is a fork of marathon, frozen at commit `e47352f2`, checked out at `$MARATHON_KT` (read freely, never modify). `PLAN.md` is the plan; work happens in the work packages (WPs) of §7, each with a brief in `docs/wp/<id>.md`.

## Ground rules

- **Behaviour parity.** Phases 0–7 followed JVM parity: port the bugs and record them in `docs/known-kotlin-bugs.md`. From Phase 8, the contract in PLAN.md §3.12 applies; contract changes need a D-row and golden updates are reviewed.
- **Name the source.** Each ported module starts with `//! Port of <kotlin path relative to the marathon repo>`.
- **Java semantics live in `jcompat`.** Phase 8 WPs may replace jcompat types with standard or crate equivalents as their brief specifies; until then, the old rule holds for new code. Use `marathon_core::strategy::queue::OrderedQueue` for test queues (D60); use the jcompat regex wrapper over the Rust `regex` crate for user-supplied regexes (D6). Keep existing formatters for result-file data and pinned progress/summary text. Diagnostic logging uses `tracing` with Rust format arguments and module targets; follow [the logging design](docs/design/logging.md).
- **Concurrency** follows `docs/design/lifecycle.md` once it exists (WP 0.6).
  - No blocking calls on the core executor thread.
  - No `unwrap()`/`expect()` on I/O, process or network results; they return errors.
- **Tests.** A Phase 8 WP may delete or rewrite a test pinning a removed JVM quirk; its parity-matrix row becomes `n/a` with reason `8.x: <D-row>`. Port every Kotlin test listed for your WP in `docs/parity-matrix.csv`, under the Kotlin test's name in snake_case, and set its row to `ported` with the Rust test path. A row may be set to `n/a` only with a reason, and a reviewer must agree. Real-time tests follow the timing rules in `docs/testing.md`.
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
