# Contributing

Issues and pull requests are welcome. For a large change, open an issue first so we can agree on
the shape before you write it.

## Build and check

Rust 1.89 or newer. From `nexo/`, the checks the CI runs: the format check on Linux, the other
three on Linux and Windows (the macOS job runs the same checks but is not gating yet):

```sh
cargo fmt --check
cargo build --workspace
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets --keep-going -- -D warnings
```

Before pushing, `nexo/tools/pre-push-check.sh` runs those four checks for the local stable
host, with true colour and a Linux 32-bit C toolchain probe (the debugger tests otherwise skip).
For example, from `nexo/`:

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$HOME/build-cache/nexo-ci" nice tools/pre-push-check.sh
```

This is not an automatic Git hook and does not check other operating systems, Rust 1.89,
`cargo deny`, or the Intel Mac target. Run CI and inspect each job, including macOS and
advisories, which currently do not gate the overall result. A local pass cannot establish a
cross-platform pass. Do not set `NEXO_UPDATE_GOLDEN` during validation.

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
  `nexo bench` or the `replay` example ([`docs/usage.md`](../docs/usage.md)).
- **Secrets never enter the repository.** Keys come from the environment; benchmark output stays
  outside the tree.

## Commits

One topic per commit, a short imperative subject (`tui: keep the draft when a modal opens`), and
the four checks green before you push.

By contributing you agree that your work is released under the [Apache License 2.0](../LICENSE).

See also the [code of conduct](CODE_OF_CONDUCT.md) and the [changelog](../docs/CHANGELOG.md).
