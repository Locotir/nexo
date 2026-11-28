# `nexo --wire`: what goes to the model, as it goes

`--wire` records every request nexo sends to a model, byte for byte, with the answer that came back and what
nexo decided before sending it: what the context policy (RACE) kept, how the request was fitted under
the model's window, and every warning. A report beside the files follows the session live.

```bash
nexo --yolo --wire              # runs go in ~/.nexo/wire
nexo --wire ./trace             # or in a folder of your own
NEXO_WIRE=~/traces nexo         # the same through the environment (sub-agents and team members inherit it)
nexo wire --open                # open the newest report in the browser
```

It is off unless asked for, and costs nothing then.

## What a run holds

One folder per nexo process, `<dir>/<date>-<time>-<pid>/` (`<dir>/latest` names the newest):

| File | What it is |
|---|---|
| `NNNN-request.json` | the URL, method, headers and body exactly as sent (pretty printed) |
| `NNNN-response.sse` | the answer's bytes as they came, or the error text |
| `NNNN-reply.txt` | the text the model wrote, taken from the stream |
| `events.jsonl` | the timeline: one line per request, answer and harness event |
| `report.html` | the run at a glance; reloads itself every 2 s while live |

`events.jsonl` has these lines (`type`):

- `request`: `n`, the URL, model, protocol, bytes, estimated tokens (characters / 4), reply room asked, system and
  tool sizes, images.
- `response`: status, time to the end, bytes, the provider's own `usage`, the stop reason, any error.
- `note` with a `kind`:
  - `view`: what the context policy sent, `view_tokens` out of `history_tokens`, the messages it kept, its
    `ceiling`, the learned token `ratio`, and `failed` with the reason when the policy failed (nexo then sends
    everything).
  - `fit`: the model's `window`, what the request takes (`used`) and the `reply` room, how many times the
    RACE cut it again (`tries`, `shrink`), and `over` when it still did not fit.
  - `refused`: the request was not sent; the message is the one the person saw.
  - `warn` / `error`: every warning and error shown in the session.

```bash
# every failed view, with why
jq -c 'select(.kind=="view" and .data.failed)' events.jsonl
# requests next to what the provider counted
jq -c 'select(.type=="response") | {n, usage}' events.jsonl
```

## The report

The left side is the timeline: requests with their status, estimated / counted input tokens and time, the
RACE's views and the window fit between them, warnings in amber, failures in red. Pick a request to see it
split into its parts: system, tools and each message with its role, block kinds (`text`, `tool_use`, `image`,
...), size and first words; the largest is marked. Sort by size to find what fills the window. Links open the raw
request, answer and reply. The filter searches everything; `live` pauses the reload.

The report lists the newest 80 requests part by part; older ones keep their numbers and their raw files.

## What is kept out, and what is not

The files hold the whole conversation: the prompt, your files and tool output as the model saw them. They are
written for you only (files 0600, folders 0700 on Unix). Header values whose name carries a key, token, secret,
auth, cookie or session, user info in a URL (`user:password@`) and `key`/`token` query parameters are written as
`‹hidden›`. The body is written as sent: whatever nexo sent the model (a project file holding a key, say) is in
it, as it was on the wire. Share a run as you would share the session itself.
