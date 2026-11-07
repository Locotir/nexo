# Setting up nexo (for a person, or an assistant doing it for them)

This guide is complete on its own: following it top to bottom gets nexo
working with any provider. Every step is one command; nothing needs a file
edited by hand. Print it any time with `nexo guide`.

## 0. The rules an assistant must keep

- **Never write a key into a file, a commit or a chat log.** Keys go in an
  environment variable, in the credential store (`nexo auth login`), or come
  from a command (`api_key_cmd`). The configuration file only names them.
- **Ask the person** which provider they pay for (a key, or a subscription
  such as GitHub Copilot or ChatGPT Plus/Pro) before choosing one.
- **Check with `nexo doctor`** after every change. It exits 0 when the setup
  works, and every line marked `FIX` says what to run next.

## 1. Find out what is there

```sh
nexo doctor                     # what a run would use, and what is missing
nexo providers                  # the usable providers and where each key comes from
nexo providers --all --json     # every provider nexo knows (200+), as JSON
nexo auth list                  # what the credential store holds (never the secrets)
nexo update --check             # whether GitHub has a newer nexo (`nexo update` installs it)
```

`nexo providers --all --json` gives, for each provider: `name`, `protocol`,
`base_url`, `key_env` (the variable its key is read from), `login` (the
subscription login it takes, if any), `key` (where the key is found now, or
`missing`), `ready`, `default_model` and `models`.

## 2. Give the provider its key — one of four ways

| Way | Command | When |
|---|---|---|
| Subscription login | `nexo auth login copilot` | the person pays for a subscription (table below) |
| Stored key | `nexo auth login <provider> --key` | a key, kept by nexo (asked for, or read from stdin) |
| Environment variable | `export DEEPSEEK_API_KEY=...` (Windows: `setx DEEPSEEK_API_KEY "..."`, then a new terminal) | a key already in the shell or a secrets manager |
| Command | `api_key_cmd = "pass show deepseek"` in the config | a password manager |

A key in the environment, a file or a command always wins over the store.

### Subscription logins

| Provider name | Subscription | Login |
|---|---|---|
| `copilot` | GitHub Copilot (Pro, Pro+, Business, Enterprise) | `nexo auth login copilot` |
| `codex` | ChatGPT Plus, Pro or Team | `nexo auth login codex` |
| `xai` | SuperGrok / X Premium (or `XAI_API_KEY`) | `nexo auth login xai` |

Each login prints a web address and a code: the person opens the address,
types the code, and nexo waits until they have approved. The tokens go to the
credential store (`nexo auth list` shows where) and are renewed by nexo when
they expire. An assistant running the command must show the address and the
code to the person and wait; it cannot approve for them.

`nexo usage` shows how much of a ChatGPT or Copilot plan is left and when it
starts over ([PLAN-USAGE.md](PLAN-USAGE.md)).

`codex` has three ways in, chosen with `--flow` (or in `/model`, which lists
them). With no flow, nexo reuses the login of the Codex CLI when its file is on
the machine (`$CODEX_HOME/auth.json` or `~/.codex/auth.json`): nothing is typed
and no token is shown; when nexo renews it, the new tokens are written back to
that file so the two never spend the same refresh token. Without that file it
opens the browser (`--flow codex-browser`, answered on `localhost:1455`, or the
next free port of 1455 and 1457). On a machine without a browser use
`--flow codex-device`: it prints an address and a code instead.

### Stored keys

```sh
nexo auth login deepseek --key            # asks for it, without showing it
printf '%s' "$KEY" | nexo auth login deepseek --key -    # from stdin, for scripts
```

## 3. Choose the default provider and model

```sh
nexo use deepseek                         # its default model
nexo use openrouter qwen/qwen3-coder      # a given model
nexo models deepseek                      # the models, their windows and prices
nexo models deepseek --json               # the same as JSON
```

`nexo use` writes `provider` and `model` into the user configuration
(`nexo config path` shows where). For one run only: `nexo --provider X --model Y`
or `NEXO_PROVIDER` / `NEXO_MODEL`.

A project's own `.nexo/config.toml` cannot set a provider's address or key source, MCP servers, hooks,
allow rules or commands until the project is trusted (`nexo config trust`); the session says what it left out.

Any provider name from `nexo providers --all` works without a configuration
block: nexo knows its address, protocol, key variable and models. Presets
with hand-tuned prices and limits: openai, gemini, deepseek, xai, groq,
mistral, openrouter, together, fireworks, cerebras, moonshot, qwen, zhipu,
minimax, copilot, codex, and the local servers ollama, lmstudio, vllm and
llamacpp (these need no key).

## 4. Check

```sh
nexo doctor            # with a live call to the provider's model list
nexo doctor --offline  # without it
nexo -p "say ok"       # one real request, printed
```

## 5. A provider nexo does not know

Add a block to the user configuration (`nexo config init` writes a commented
starter file):

```toml
[providers.myserver]
protocol = "openai-chat"              # openai-chat | openai-responses | gemini
base_url = "http://localhost:8000/v1"
api_key_env = "MYSERVER_API_KEY"      # or api_key_cmd = "...", or `nexo auth login myserver --key`
models = ["my-model"]
default_model = "my-model"

[providers.myserver.pricing."my-model"]   # dollars per million tokens, optional
input = 0.5
output = 1.5
cache_read = 0.05
```

Most servers speak `openai-chat` (an OpenAI-compatible `/v1/chat/completions`).
A block with the name of a known provider overrides only the fields it sets.
Inside the interface, `/model` walks through the same steps and probes the
server.

## 6. When something fails

| Message | Do |
|---|---|
| `needs a key: set $VAR, or run nexo auth login X` | one of the four ways in step 2 |
| `no provider can be used` | step 2, then `nexo use <provider>` |
| `unknown provider` | `nexo providers --all` for the right name, or step 5 |
| `401` / `auth` error | the key is wrong or the login expired: `nexo auth login <provider>` again |
| `no Copilot chat access` | the GitHub account has no Copilot plan with chat |
| `model ... is not in its list` | `nexo models <provider>` and `nexo use <provider> <model>` |
