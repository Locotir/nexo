# Sharing sessions and the audit log

Both stay on your machine. nexo sends no telemetry.

## Export a session

```
nexo export [SESSION-ID | --last] [--html | --markdown | --json] [-o FILE] [--no-redact]
```

- With no id, the latest session of the current directory is used (else the newest one).
- HTML is the default. It is one self-contained file: inline CSS, no script, no font, nothing loaded
  from the network, light and dark by the reader's `prefers-color-scheme`.
- Messages are markdown rendered to safe HTML. Everything is escaped and raw HTML is shown as text;
  images show their alt text, and only http, https and mailto links stay links.
- Tool calls and results are collapsible `<details>`. An edit shows as a diff. The header gives
  model, date, turns, tokens and cost.
- `-o -` prints to stdout. The default file is `<session id>.html` (`.md`, `.json`) in the current directory.
- In the TUI: `/export html`, `/export json`, `/export notes.html`. The format follows the word or the
  file extension; with no path the file goes next to the session file.

### Redaction

Before anything is written, these become `[redacted]`:

- keys and tokens (`sk-`, `ghp_`, `github_pat_`, `xox`, `AKIA`/`ASIA`, JWTs, `Bearer ...`), PEM key
  blocks, `password=`, `token:` and `API_KEY=` style values, `NAME=value` env lines;
- runs of 40 or more base64 characters that mix digits, upper and lower case;
- the arguments and results of any tool call that names a `.env*` file (as a path or inside a shell command).

`--no-redact` turns it off and prints a warning. In the markdown export the output of such a call is replaced by
`[redacted: .env file]`, and the rest is scrubbed as text. Pattern redaction is greedy but not perfect: read a
file before you share it.

## The audit log

Off by default. In `config.toml`:

```toml
[audit]
enabled = true
path = "~/.nexo/audit/nexo.jsonl"   # optional; default ~/.nexo/audit/YYYY-MM.jsonl, one file a month
```

A project's own `config.toml` cannot change `[audit]` unless the project is trusted.

The log is append-only. A background thread writes it, fed by an unbounded queue, so a slow disk never
blocks a turn. One JSON object per line.

Tool call:

```json
{"ts":"2026-11-07T10:00:00Z","kind":"tool","session":"20261107-100000-ab12","cwd":"/work/app",
 "user":"ana","provider":"deepseek","model":"deepseek-chat","tool":"bash","args":"bash cargo test",
 "decision":"allowed","by":"rule","status":"ok","exit_code":0,"duration_ms":1840}
```

- `args` is the tool's one-line summary, redacted, at most 200 characters; `[redacted]` for a call on a `.env*` file.
- `decision` is `allowed`, `denied`, or `asked` (the person was asked and said yes).
  `by` is `rule` (the permission mode and rules), `hook`, or `user`.
- `status` is `ok`, `error` or `denied`. `exit_code` is set for shell commands.

Model request:

```json
{"ts":"...","kind":"request","session":"...","cwd":"...","user":"ana","provider":"deepseek",
 "model":"deepseek-chat","call":"main","tokens_in":1200,"tokens_out":340,"tokens_cached":9000,"cost_usd":0.0021}
```

- `call` is `main`, `sub` (a sub-agent), `reader`, `compact`, `suggest` or `review`.
- `tokens_in` counts input that was not served from the cache (cache writes included); `tokens_cached` is the cache read.
- `cost_usd` uses the prices nexo knows (0 when there are none).

A sub-agent's tool calls are logged like the main agent's (same session, its own `decision` and `by`); `exit_code` is not set for them. Its model requests are `sub`.

## Report

```
nexo audit report [--since 7d] [--by user|model|day] [--json] [--file FILE]
```

Sums tool calls, requests, tokens and cost per user (default), per `provider/model`, or per day, with a
total row. `--since` takes `7d`, `12h`, `30m`, `2w` or a date such as `2026-11-01`. With no `--file` it reads
the configured file, or every month file in `~/.nexo/audit/`. Tool calls count under the model in use.
