# Usage

## Build and run

Rust 1.89 or newer, on Linux or Windows.

```sh
cd nexo
cargo build --release                        # target/release/nexo
export DEEPSEEK_API_KEY=...                  # or another provider's key
cd your/project
nexo                                         # the full-screen interface (TUI)
nexo -p "run the tests and fix what fails"   # headless: prints the reply
```

The context policy is set in `~/.nexo/config.toml` (`%USERPROFILE%\.nexo\config.toml` on Windows):

```toml
[context]
policy = "curator"                           # auto | plain | compact | curator
# curator_spec = "eoq=15000;ledger=10000"    # override the engine's settings
# working_set = "10000/40000/80000"          # target/cap/hard tokens of the view (the default)
```

`auto`, the default, is the curator. `plain` sends the whole history; `compact` summarises once a request
reaches `compact_at` input tokens (**100000** by default, counted as the provider counts them), as often
as needed, keeping the task statement and the files touched. The status bar shows what the policy is doing
(`curator −62%`, `curator idle` while the whole history is still cheaper to send than to rewrite,
`curator off`, `plain`), how many messages each call put in and took out (`+3 in  -12 out`), and `/context` says why. The view never passes the model's window, so a long session does not end in an error from the provider.

## The benchmark: `nexo bench`

`nexo bench` runs agent tasks against a real model under several context policies and scores each run
with the task's own checker. The tasks run commands the model writes, so run it in a container or a VM.

```sh
printf %s "$DEEPSEEK_API_KEY" | nexo bench --upstream https://api.deepseek.com/anthropic --model deepseek-chat \
  --key - --tasks realm --arms plain,compact,curator \
  --repeats 1 --max-steps 1000 --budget 5 --out bench-out
```

- `--upstream`, `--model`, `--key`: the endpoint, the model and its key (take it from the environment,
  as above, rather than writing it out).
- `--tasks`: which tasks to run. `realm` gives the agent a stripped game binary whose coin rule it must
  rebuild from experiments into a `coins.rs`; the small tasks are Rust mini-projects, each with a trap
  long runs tend to fall into (a rule stated once, a detail buried in noise, a goal that changes).
- `--arms`: one arm per context policy, `[NAME@]plain|compact|curator[:k=v;k=v]`;
  `lean@curator:eoq=15000` is the curator with that spec, reported as `lean`.
- `--repeats`, `--max-steps`: runs per task and arm, and the step limit of each run.
- `--budget`: stops starting runs once that many dollars are spent. It needs prices: nexo knows
  DeepSeek's and those its provider presets list; for another model give them with
  `--price MISS,HIT,OUT` (dollars per million tokens), or the bench refuses to start.
- `--out DIR`: `results.jsonl` (one line per run), each run's transcript, and `summary.md` with, per
  arm, the score, steps, requests, tokens, cost, tool errors and repeated calls.

How runs are judged (a score per run, non-inferiority to the whole history within ten points as the
primary test, dollars as the secondary one) is fixed in advance in [`scoring.md`](scoring.md), where `full` is
the `plain` arm.

## Repository layout

```
README.md                   the presentation and a short guide
CONTRIBUTING.md, SECURITY.md  how to send a change, how to report a security problem
docs/
  usage.md                  this file: build, config, the benchmark, the replay example
  results.md                every measured figure
  how-it-works.md           the engine and its settings
  scoring.md                how benchmark runs are scored, fixed before they are made
  related-work.md           what this work builds on
  providers-base.md         the provider protocols nexo speaks
  providers-policy.md       what each provider's terms allow
nexo/
  README.md                 using nexo
  ARCHITECTURE.md           the crates and their contracts
  FEATURES.md, IDEAS.md     what exists, with its tests, and what may come
  crates/
    nexo-core               types shared by every crate
    nexo-providers          the protocol adapters
    nexo-tools              the agent's tools
    nexo-agent              the agent loop, sessions and context policies
    nexo-curator            the context curator
    nexo-bench              the benchmark behind `nexo bench`
    nexo-tui                the full-screen interface
    nexo-cli                the `nexo` binary
.github/workflows/ci.yml    format, build, test and clippy on Linux and Windows
```

## Replaying the curator on a recorded run

Every `nexo bench` run writes the requests it sent as a `.jsonl` transcript. The `replay` example runs
the curator over one offline, request by request, with no model and no key, and prints what the model
would have been sent:

```sh
cd nexo
cargo run --release -p nexo-curator --features sim --example replay -- bench-out/realm-plain-0.jsonl
cargo run --release -p nexo-curator --features sim --example replay -- FILE --spec "eoq=20000" --every 10
```

Columns: request, messages in view, tokens of the whole history, tokens of the view, share kept,
nudges, the engine's stats when they change. `--quiet` prints only the summary. This is how a change to
the engine is judged before paying for a live run: replay the recorded runs, read what the view keeps
and what the ledger holds, then run the benchmark again.

## Further reading

- [`../nexo/README.md`](../nexo/README.md): installing and using nexo, providers, sessions, the curator's status.
- [`nexo/ARCHITECTURE.md`](../nexo/ARCHITECTURE.md): the crates, their dependencies and their public API.
- [`scoring.md`](scoring.md): the benchmark's scoring rules.
- [`related-work.md`](related-work.md): prompt caches, context compression, Belady's
  algorithm, ski rental, the economic order quantity, budgeted coverage.
- [`providers-base.md`](providers-base.md) and
  [`providers-policy.md`](providers-policy.md): the providers, their protocols and their terms.

## Data and privacy

nexo's sessions stay on the machine (`~/.nexo/sessions/`) and hold whatever passed through the
conversation, credentials included: treat them like the credentials themselves. Whether each provider's
terms allow this use is read provider by provider in [`providers-policy.md`](providers-policy.md). The recordings behind
the simulations are one person's sessions and are not published; every table names the data it came from.
