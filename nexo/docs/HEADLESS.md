# Headless contract

`nexo -p` runs without the interface, for scripts and CI. Everything here is stable: scripts may rely on it.

```
nexo -p "fix the failing test" --output-format json --max-turns 20
git diff | nexo -p "review this diff"            # plain stdin is added to the prompt
nexo -p --input-format stream-json --output-format stream-json < messages.jsonl
```

Headless runs are saved as sessions but marked, so they stay out of `/resume`.

## Flags

| Flag | Meaning |
| --- | --- |
| `-p [PROMPT]` | Run without the interface. The prompt may be empty with `--input-format stream-json`. |
| `--output-format text\|json\|stream-json` | What goes to stdout (below). Default `text`. |
| `--json` | Older flag, kept as it was: every internal agent event as a JSON line (`{"event":"ready",...}`). New scripts should use `--output-format stream-json`. Cannot be combined with `--output-format`. |
| `--input-format text\|stream-json` | `stream-json`: user messages as JSON lines on stdin. |
| `--max-turns N` | Stop after N tool rounds and exit 4 (the model is not asked to go on). |
| `--allowed-tools a,b` | Offer only these tools. An entry with a pattern, such as `bash(git *)`, also allows those calls without asking. Alias `--allowedTools`. |
| `--disallowed-tools a,b` | Take these tools away; `bash(rm *)` adds a deny rule. Alias `--disallowedTools`. |
| `--append-system-prompt TEXT` | Added to the end of the system prompt. |
| `--add-dir PATH` | One more folder the file tools may read and write without asking. Repeatable; it also works without `-p`, for the interactive screen; `/add-dir PATH` adds one mid-session. |
| `--json-schema FILE\|JSON` | The final answer must be JSON that validates against the schema. |
| `--permission-mode`, `--yolo`, `--model`, `--provider`, `--cwd`, `--continue`, `--resume` | As in the interface. |

Nobody can be asked in a headless run: a tool call that needs permission is refused (the model sees "Permission denied") unless the mode is `yolo` or an allow rule covers it.

The flags above except `-p` are refused (exit 2) without `-p`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done. |
| 1 | The run failed or the model hit an error (provider error, a session that cannot be resumed). |
| 2 | Bad usage: unknown flag or value, unreadable schema or `--add-dir`, no prompt, a line of the input stream that is not a user message. |
| 3 | The final answer does not match `--json-schema` after one retry. |
| 4 | The turn limit (`--max-turns`, or `agent.max_turns` in the configuration) was reached. |
| 5 | A tool call was refused by the permission policy, a deny rule or `--allowed-tools`, and the run otherwise finished. |
| 130 | Interrupted (Ctrl-C). A second Ctrl-C leaves at once. |

Before this contract a turn that did not finish exited 2; it now exits with the specific code above. With several messages on the input stream, the exit code is the first non-zero one.

## text (default)

The reply streams to stdout. Tool calls and notices go to stderr.

```
$ nexo -p "say hi"
Hi.
```

With `--json-schema` the validated answer is printed once, as compact JSON.

## json

One object at the end, on one line:

```json
{"type":"result","subtype":"success","is_error":false,"exit_code":0,"result":"Hi.","session_id":"20261107-101500-ab12","model":"claude-x","num_turns":1,"usage":{"input_tokens":812,"output_tokens":4,"cache_read_tokens":0,"cache_write_tokens":0},"cost_usd":0.0031,"duration_ms":1840,"stop_reason":"end_turn"}
```

- `subtype`: `success`, `error` (exit 1), `error_schema` (3), `error_max_turns` (4), `error_permission_denied` (5), `interrupted` (130).
- `stop_reason`: `end_turn`, `max_turns`, `interrupted` or `error`.
- `num_turns`: model calls made. `usage` and `cost_usd` are the session totals.
- `structured_output`: the parsed answer, when `--json-schema` is given and it validated.
- `error`: the reason, when `is_error` is true and a reason is known.
- `plan_usage`: when the request ran on a subscription login and the backend reported the plan, an array of `{"plan": "codex", "tier": "plus", "meters": [{"label": "wk", "used_percent": 1.0, "resets_in_seconds": 569444}]}` (`used` and `limit` too for a plan that counts requests). Absent for a provider on a key. See [PLAN-USAGE.md](PLAN-USAGE.md).
- A run that cannot start (no provider, an unknown provider or a configuration that does not parse) prints the same object with `subtype` `error`, `exit_code` 1, empty `result`, `error` set and no session, on stdout with `json` and `stream-json` (with `--json`: one `{"event":"error","message":...}` line). The one-line message goes to stderr too, and the exit code is 1.

## stream-json

One JSON object per line, as it happens. Every line has a `type`:

```
{"type":"init","session_id":"...","provider":"mock","model":"mock","tools":["read","bash"],"cwd":"/work","permission_mode":"ask"}
{"type":"assistant_delta","text":"Reading"}
{"type":"assistant","text":"Reading the file."}
{"type":"tool_use","id":"c1","name":"read","input":{"path":"a.txt"},"summary":"read a.txt"}
{"type":"tool_result","id":"c1","is_error":false,"content":"hello file","elapsed_ms":3}
{"type":"notice","level":"warn","text":"..."}
{"type":"result","subtype":"success","is_error":false,"exit_code":0,"result":"Done.", ...}
```

`init` comes first. `assistant_delta` carries streamed text, `assistant` the whole message once it is complete. `result` is the last line and has the fields of the `json` format. With an input stream there is one `result` per user message.

## Input stream

With `--input-format stream-json`, each stdin line is a user message. Accepted shapes:

```
{"type":"user","message":{"role":"user","content":"first question"}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"second"}]}}
{"content":"also fine"}
```

Blank lines are skipped, and lines whose `type` is not `user` are ignored. Messages run one after another in one session, each waiting for the previous answer. The run ends when stdin closes. If `-p` has text, it is the first message. `--json-schema` cannot be combined with an input stream (exit 2).

## Schema

`--json-schema` takes a file path or inline JSON (anything starting with `{`). The model is asked to answer with only JSON matching the schema. If the answer does not validate, it is sent back once, in the same session, with the errors. A second miss exits 3. The checker covers `type`, `enum`, `const`, `properties`, `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `anyOf`, `oneOf`, `allOf`, `not` and local `$ref`; other keywords (such as `pattern` and `format`) are ignored.

## TLS roots

nexo trusts the system CA store first (`SSL_CERT_FILE` and `SSL_CERT_DIR` are honoured); when it is empty or unreadable, as in a container image without `ca-certificates`, it falls back to the bundled Mozilla roots.
