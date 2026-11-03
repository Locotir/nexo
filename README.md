<div align="center">

# nexo

### The coding agent that doesn't run out of context.

A terminal coding agent for any LLM API, written in Rust.<br>
Its built-in **context curator** sends the model only what the next step needs.<br>
Long sessions stay fast and cheap, the prompt cache stays warm, and nothing is summarised away.

[![Rust 1.80+](https://img.shields.io/badge/Rust-1.80%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-2a78d6)](LICENSE)
[![Linux | Windows](https://img.shields.io/badge/Linux%20%7C%20Windows-supported-52514e)](#install)
[![Providers](https://img.shields.io/badge/providers-20%20built--in%20%2B%20200%2B-1baf7a)](#any-provider)
[![PayPal](https://img.shields.io/badge/PayPal-support%20nexo-00457C?logo=paypal&logoColor=white)](https://www.paypal.me/locotir)

</div>

![One 100-step session: whole history against the nexo curator](docs/img/hero.svg)

## The problem

Every coding agent re-sends its **whole history** on every call. Step 80 pays again for the logs of
step 3. When the window fills, the agent **compacts**: a summary replaces the work, the exact error
line or path the model needed is gone, and the prompt cache starts from zero.

## What nexo does instead

nexo keeps the whole history on disk and gives the model a **curated view** of it on every request.

| | Whole history | Compaction | **nexo curator** |
|---|---|---|---|
| Tokens per call | grow every step | reset, then grow again | **stay small** |
| Prompt cache | hits, but you pay for everything | broken by each summary | **kept**: it cuts only when the saving pays for the rewrite |
| Old details | all there, buried | lost in a summary | **on disk**; the view says what left and how to read it back |
| Your rules ("never touch prod") | drift out of attention | can be summarised away | **pinned** for the whole session |
| A resumed session | same | starts from the summary | **same view as before**, cache still warm |

## The numbers

| Test | Result |
|---|---|
| **One 100-step session in nexo** (deepseek-chat) | **−36% cost**, **−59% wall time**, **−48% tokens**, same score |
| **Live runs**, 10 per arm over 5 tasks (DeepSeek, reference implementation of the same engine) | **−56% tokens**, **−38% cost**; solved 9/10 vs 10/10 |
| Replay of 28 recorded nexo sessions | **−42%** tokens sent |
| Simulation on unseen recorded sessions | **61.5%** of the bill saved (5-minute cache) |
| Proved ceiling for any curator that sends literal text | **89.7%** |

What the model is sent, request by request, in that 100-step session:

![Tokens per request: the whole history grows to 97k, the curated view is 19k at request 100](docs/img/tokens-per-step.svg)

Every figure, its data and its limits (mostly one model, one author's tasks):
[`docs/results.md`](docs/results.md). Measure it on your own tasks with [`nexo bench`](docs/usage.md).

## How the curator works

![Where the curator sits](docs/img/flow.svg)

- **Squeeze at birth.** Progress bars, repeated lines and noise never enter the history.
- **One thread per goal.** A finished goal becomes a frozen card, so each goal keeps its cached prefix.
- **Cut in batches, only when it pays.** An economic-order-quantity rule: old material leaves when the
  tokens saved over the next requests outweigh one cache rewrite.
- **An anchor that never drops.** Your rules, the agent's notes, pinned files, a ledger of what left
  and a map of the work.
- **Stale reads go first.** A file read before the agent edited it leaves before anything else, with a
  note to read it again.
- **Resume = same view.** Reopen or rewind a session and the curator replays its decisions, so the
  provider's cache still hits.

The engine, every setting and its default: [`docs/how-it-works.md`](docs/how-it-works.md).

## Install

Rust 1.80 or newer. Linux or Windows.

```sh
cargo install --locked --git https://github.com/Locotir/nexo nexo-cli
```

Or from source:

```sh
git clone https://github.com/Locotir/nexo && cd nexo/nexo
cargo build --release            # target/release/nexo (nexo.exe on Windows)
```

## Quick start

```sh
export DEEPSEEK_API_KEY=...      # or ANTHROPIC_API_KEY, OPENAI_API_KEY, GEMINI_API_KEY, ...
cd your/project
nexo                             # full-screen interface
nexo -p "run the tests and fix what fails"   # headless: prints the reply
nexo --continue                  # resume the latest session here
nexo doctor                      # checks files, provider, model and key with one live call
```

The status bar shows what the curator is doing (`curator −62%`: sending 62% less than the whole history).
Pick a policy in `~/.nexo/config.toml`:

```toml
[context]
policy = "curator"               # auto | plain | compact | curator
```

## Everything else a coding agent needs

- **Full-screen terminal UI**: mouse, folding transcript, search, command palette, diffs before every
  write, 16/256-colour, mono and `NO_COLOR` themes.
- **Headless mode** for scripts and CI: `nexo -p "..." --json`.
- **Tools**: read, write, edit, bash, glob, grep, ls, web search and fetch, todo list, language-server
  diagnostics after an edit.
- **Sub-agents** (read-only explorers or full workers, custom ones from `.nexo/agents/*.md`) and
  **background jobs**.
- **MCP servers**, **hooks** (`pre_tool` can block), **plan mode**, permission modes and remembered rules.
- **Undo and rewind**: go back to any message, and the files the agent wrote go back too.
- **Images**: attach them or paste a screenshot with `Ctrl+V`.

86 features, each with the test that checks it: [`nexo/FEATURES.md`](nexo/FEATURES.md).
Full guide: [`nexo/README.md`](nexo/README.md).

## Any provider

Bring your own API key. Built in: **Anthropic**, **OpenAI**, **Google Gemini**, **DeepSeek**, **xAI**,
**Groq**, **Mistral**, **OpenRouter**, Together, Fireworks, Cerebras, Moonshot, Qwen, Zhipu, MiniMax,
and local servers (**Ollama**, LM Studio, vLLM, llama.cpp). Over 200 more come from the
[models.dev](https://models.dev) directory, and any OpenAI- or Anthropic-compatible endpoint works.

## FAQ

**Can I use it with Claude?** Yes, with an Anthropic API key (`ANTHROPIC_API_KEY`). nexo never signs in
with a Claude.ai subscription: Anthropic's terms do not allow third-party tools to use subscription
logins, and nexo follows them.

**Does it still compact?** Only if you choose the `compact` policy, or as a last resort when a provider
rejects a request as too long. With the curator, the view stays far below the window.

**Where does my data go?** Only to the provider you pick, and to the web search backend or MCP servers when the agent uses them. Sessions live on your machine in
`~/.nexo/sessions/` and hold whatever passed through the conversation, so treat them like credentials.

**Is the saving real on my model?** It depends on the price of a cached token and on how long your
sessions are. `nexo bench` runs the same tasks with and without the curator and prints the difference.

## Support

nexo is free and MIT-licensed. If it saves you money, you can share a little of it:

[![Support nexo on PayPal](https://img.shields.io/badge/PayPal-support%20nexo-00457C?style=for-the-badge&logo=paypal&logoColor=white)](https://www.paypal.me/locotir)

A star helps others find it. Issues and pull requests are welcome.

## License

[MIT](LICENSE). nexo is an independent project, not affiliated with or endorsed by Anthropic, OpenAI,
Google, DeepSeek or any other provider. Product names are trademarks of their owners.

<sub>Keywords: AI coding agent, LLM agent harness, terminal coding assistant, Claude API, Anthropic API,
OpenAI API, DeepSeek API, Gemini API, context engineering, context window management, context
compression, prompt caching, token optimization, long-running agent sessions, Rust TUI, open-source
alternative to closed coding agents.</sub>
