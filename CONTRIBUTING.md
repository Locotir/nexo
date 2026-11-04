# Contributing

Issues and pull requests are welcome. For a large change, open an issue first so we can agree on
the shape before you write it.

## Build and check

Rust 1.80 or newer. From `nexo/`, the same four checks the CI runs on Linux and Windows:

```sh
cargo fmt --check
cargo build --workspace
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
```

The interface tests draw in 24-bit colour: run them from a colour terminal, or with
`COLORTERM=truecolor` set.

## Golden captures

Some interface and curator tests compare their output with captures kept in the repository.
If you change that output on purpose:

```sh
NEXO_UPDATE_GOLDEN=1 cargo test --workspace   # rewrites the captures
git diff                                      # read every change
cargo test --workspace                        # must pass without the variable
```

## Ground rules

- **Tests are deterministic.** No network, no clock, no randomness without a fixed seed.
- **No new dependencies** without a reason in the pull request.
- **The interface** keeps working in 16 and 256 colours, in mono and with `NO_COLOR`; text stays
  readable (no italics, AA contrast); `animations = false` stops all motion.
- **Context policies are general.** A change to the curator has to help any kind of session (a web
  repository, a data project, a chat without tools), not one benchmark task. Show it with
  `nexo bench` or the `replay` example ([`docs/usage.md`](docs/usage.md)).
- **Secrets never enter the repository.** Keys come from the environment; benchmark output stays
  outside the tree.

## Commits

One topic per commit, a short imperative subject (`tui: keep the draft when a modal opens`), and
the four checks green before you push.

By contributing you agree that your work is released under the [MIT license](LICENSE).
