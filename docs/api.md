# nexo api: the API behind a website

`nexo api` turns captured web traffic into a map of a site's API. The map says which endpoints exist, what they take and return, how they page and how they authenticate. You or an agent can then describe an endpoint, call it, watch it for changes, and check that it still answers as recorded. No browser is needed after the capture.

Use it to:

- watch data a site shows you (prices, stock, a list of items) without scraping pages;
- let an agent act on a site through its API instead of clicking through it;
- notice when a site changes its API, from a script or CI.

All of it is worked out in Rust from the traffic. No model reads the raw capture.

```
nexo api analyze cap.har --save map.json     read a capture, save the map
nexo api describe map.json [ENDPOINT]        list endpoints, or show one
nexo api call map.json ENDPOINT -p k=v       call one endpoint
nexo api watch map.json ENDPOINT --every 5m  call it on a schedule, print changes
nexo api check map.json                      call the safe endpoints, report drift
```

`nexo re api <op>` runs the same ops (see [re.md](re.md)). Only call sites you may use, and follow their terms.

## Capture

`analyze` reads a HAR 1.2 file. There are two ways to get one.

**From nexo's browser tool.** You, or an agent running in nexo, ask the `browser` tool:

1. `capture_start` begins keeping what the page sends.
2. Browse the flows you want mapped: log in, open the lists, use the search, page through results.
3. `capture_stop` says how many requests were kept and what was left out.
4. `capture_save` with a `path` (for example `shop.har`, relative to the session folder) writes the HAR.

Then run `nexo api analyze shop.har --save shop.json`. Saving a file is asked about like any other write.

Nexo's own headless browser keeps headers, request bodies and JSON or text response bodies of XHR and fetch calls. Every token, cookie and password value is turned into a marker before it is kept, so the saved file holds no live secret, and the same token has the same marker wherever it shows up.

Your own Chrome (`select_browser`) keeps only the URL, method, type, status and mime type of each request, no headers and no bodies. `analyze` then sees endpoints and statuses but cannot learn request or response shapes, and `capture_save` says so in its answer. For shapes, use `select_browser` with the id `headless` and browse again.

**From devtools.**

- Chrome or Edge: open DevTools, Network tab, reload or use the site, then "Save all as HAR".
- Firefox, Playwright `recordHar` and mitmproxy exports also read.

Tick "Preserve log" first if the flow spans several pages. Response bodies must be in the file: that is where the shapes come from. A devtools HAR holds your real tokens and cookies; `analyze` never prints or saves them, but keep the file private.

### What is dropped

Not everything in a capture is the site's API. These calls are dropped, and the summary counts each reason:

| Reason | What it is |
| --- | --- |
| `not http` | `data:`, `wss:` and other schemes |
| `other host` | only when you pass `--host`: anything not on those hosts or under them |
| `preflight` | `OPTIONS` requests |
| `tracker` | analytics, ads and logging hosts, and `ping` or `beacon` requests |
| `event stream` | `text/event-stream` answers: they are read as channels, not as endpoints (see Live channels) |
| `static file` | images, scripts, styles, fonts, media, by resource type, extension or mime type |
| `page` | HTML documents |
| `no response` | requests that got no answer |
| `redirect` | 3xx answers without a `set-cookie` (one that sets a cookie is kept: many logins end that way) |

Each drop is counted, so nothing vanishes without a trace. A header says `N calls, K kept, E endpoints; dropped: ...`.

### Secrets

Secrets are removed while the file is read, before anything else sees them. A header, cookie, query parameter or body field whose name says it carries a secret (`authorization`, `token`, `session`, `api_key`, `csrf`, `password`, a bare `key` in a query, and similar) becomes `<secret>`. Only a short fingerprint is kept in memory, so nexo can tell that the token a request sends is the one an earlier login response gave. Long opaque strings (tokens, hashes) are never shown as examples, and opaque path segments print as `<opaque>`.

### Live channels

A HAR that holds WebSocket frames (`_webSocketMessages`, as Chrome and Edge devtools write them) or `text/event-stream` answers also gives channels. `analyze` groups the messages of a channel by the field that tells their kind (`type`, `event`, `op`, `channel` or `action`), else by their top-level shape; a stream by its event name. Per message type it keeps the direction, the count, the rate per minute and the JSON shape. Heartbeats (ping and pong frames, stream comments, tiny repeated frames, `{"type":"ping"}`) are counted and named, not listed as types. The first client messages after the open are kept as the subscribe pattern. Tokens inside frames are hidden like any other secret, and a map holds none.

The summary prints one line per channel and one short line per type (capped):

```
[ws_socket] WS app.test/socket, 14 messages, by "type"
  subscribe: {"type":"subscribe","token":"<secret>"}
  <- ticker x6 [7/min] {price:number, symbol:string}
  heartbeat: ping/pong x12
```

The OpenAPI export has them under `x-nexo-channels`, and the map file holds them, so `describe` shows them.

Nexo's own headless browser captures live channels too (your own Chrome does not). A WebSocket becomes one HAR entry: URL, handshake headers, status 101 and `_webSocketMessages`. An EventSource keeps its messages as the `text/event-stream` body of its response. Frame and message text goes through the same masking as bodies (a `token` field or a `Bearer ...` in a frame becomes a marker). Caps: 100 WebSockets per capture, 5000 frames per socket, 64 KiB per text frame (longer ones are cut), 5000 messages per stream, and the capture's total size cap. Binary frames keep only the opcode and their length (`_length`), never the payload. `capture_stop` and the HAR (`log._nexoCapture`, `_nexoWebSocket` on each socket) count what was left out.

## analyze

```
nexo api analyze cap.har
nexo api analyze cap.har --host shop.example.com --save map.json
nexo api analyze cap.har --openapi openapi.json
nexo api analyze cap.har --json
```

| Flag | Meaning |
| --- | --- |
| `--host HOST` | Keep only this host and the hosts under it. Repeat for more. |
| `--save FILE` | Write the map, for `call`, `describe`, `watch` and `check`. |
| `--openapi FILE` | Also write an OpenAPI 3.1 file. |
| `--json` | Print the OpenAPI JSON instead of the summary. |
| `--budget N` | About how many characters the summary may take. Default 6000. `describe` takes it too. |

Without `--json` it prints a summary: the header, the hosts, the auth schemes, then one block per endpoint, busiest first, until the budget is used. The last line counts the endpoints left out.

```
[get_api_items] GET /api/items x12 a1
  query: page int, q? string
  -> 200 json {items: [{id, name}], total}
  paging: page via ?page, items at $.items
```

Read it as: the endpoint id, the method and path, how many calls were seen (`x12`), and which auth schemes it used (`a1`). A `?` after a query name means it was not in every call. `{user_id}` in a path is a parameter: segments that look like ids are folded together, and a plain word becomes a parameter only when many different words stand in the same place with the same answer shape.

Exit code: 0, or 2 if the HAR cannot be read or a file cannot be written.

### The map file

`--save` writes a JSON file (`"nexo_api_map": 1`; a newer format is refused). It holds:

- `hosts`, `total` (calls in the capture) and `dropped` (counts by reason);
- `auth`: each scheme with its kind, how many calls use it and, when an earlier response handed the secret out, a `source` such as `set by POST /api/login ($.access_token)`;
- `endpoints`, each with its `id` (also the OpenAPI operationId, like `get_users_user_id`), method, origin, path, call count, path and query parameters, non-standard headers, body shape, response shapes by status, paging, auth references and one example URL.

Shapes are types, string formats, a few enum values, array sizes and how often each object field appeared. They are not data.

Auth kinds are `bearer`, `basic`, `api_key_header`, `api_key_query`, `cookie` and `csrf`. Paging styles are `cursor`, `next url`, `page`, `offset` and `link header`; a paging entry names the request parameter, the size parameter, the JSON path of the next-page value and the JSON path of the list. GraphQL operations are endpoints too, with ids like `gql_GetUser`: the shape is the operation's variables, batches are split into one endpoint per operation, and persisted queries are named `persisted:` plus the start of the hash. The map keeps no query text.

**The map never holds a secret value.** It holds names only: the header, cookie or parameter that carries a secret, and where it came from. The values come from environment variables when you call (below). Keep the map out of version control anyway if the site is private.

## describe

```
nexo api describe map.json
nexo api describe map.json --budget 2000
nexo api describe map.json get_api_items
nexo api describe map.json "GET /api/items/{item_id}"
```

With no endpoint it prints the same summary as `analyze` (6000 characters, or about N with `--budget N`) and, if the map has auth, the environment variable each scheme reads. With an endpoint it prints its path, query and body parameters (`*` marks a required one, with an example when there is a safe one), the first three response shapes, paging, and per auth scheme the variable it reads and the login it can run.

An endpoint is named by its id, or by `"METHOD /path"` with the templated path or a real one (`GET /api/items/99`). If a name matches more than one endpoint, the error lists the ids.

## call

```
nexo api call map.json get_api_items -p page=2 --dry-run
nexo api call map.json get_api_items -p page=2
nexo api call map.json "POST /api/items" --body '{"name":"demo"}'
```

| Flag | Meaning |
| --- | --- |
| `-p`, `--param NAME=VALUE` | A parameter. Repeat for more. `NAME=@env:VAR` reads the value from an environment variable. |
| `--body JSON\|@FILE` | The whole body, as JSON text or a file. |
| `--auth NAME=ENVVAR` | Read an auth scheme's secret from this variable. Repeat for more. |
| `--login` | Run the login call the map records first, and use the token it returns. |
| `--login-param`, `--login-body` | Parameters and body for that login call. |
| `--dry-run` | Print the request with `<secret>` for each auth value. Send nothing. |
| `--json` | Print `status`, `content_type`, `location`, `body` and `bytes_not_read` as JSON. |
| `--reveal` | Print the answer unscrubbed. |

**Parameters.** A `-p` name is matched to a path parameter, then a query parameter, then a body field. Prefix with `query.` or `body.` to say which, and use dots for nested body fields (`body.user.name`). Body values are typed from the map: a field seen only as numbers is sent as a number. GET and HEAD take no body. A missing required parameter is an error that lists all of them, with examples.

**Auth.** Secrets come only from environment variables, never from the command line or the map. `describe` prints the default variable for each scheme, built as `NEXO_API_` plus the scheme label in capitals: `NEXO_API_BEARER`, `NEXO_API_HEADER_X_API_KEY`, `NEXO_API_COOKIE`. `--auth bearer=MY_TOKEN` names another variable. The scheme is put where the traffic had it: the header, the query parameter, the `cookie` header. For basic auth a value like `user:pass` is encoded for you.

With `--login`, nexo runs the endpoint that handed out the token in the capture, reads the token from the JSON path the map records, and uses it for the call. The token stays in memory. After a 401 or 403 it logs in again once. The login call reads its own secrets (password and so on) the same way: `--login-param password=@env:SITE_PW`.

**Methods.** `nexo api call` sends any method, and has no `--unsafe` flag: you named the endpoint, so it runs. The `--unsafe` flag belongs to `check` (below). Redirects are not followed. A call times out after 30 seconds, and 8 MiB of the answer are read; the rest is counted.

**Output.** Unless you pass `--reveal`, token-looking values and secret fields in the answer print as `<secret>`, and so does the `code` of a redirect URL. Notes go to stderr as `nexo api: ...`.

Exit code: 0; 1 if the answer status is 400 or more, or the call failed; 2 for bad arguments, an unknown endpoint, a missing parameter or an unset variable.

## watch

```
nexo api watch map.json get_api_items --every 5m --state items.json
nexo api watch map.json get_api_items --times 1 --state items.json
```

`watch` takes everything `call` takes, plus:

| Flag | Meaning |
| --- | --- |
| `--every DURATION` | Time between calls: `30s`, `5m`, `2h`, `1d`, or bare seconds. At least 10s. Needed unless `--times 1`. |
| `--times N` | Stop after N calls. Without it, it runs until you stop it. |
| `--state FILE` | Keep the last answer between runs. |
| `--for DURATION` | Follow a live channel of the map instead (the endpoint argument is its id): see below. |

The first tick prints `first answer: status 200`. After that it prints nothing when the answer is the same, and otherwise `# N changes` followed by lines for each added, removed or changed path, such as `~ $.body.count: 1 -> 2`. Lists whose items share an `id`-like field (`id`, `_id`, `uuid`, `guid`, `key`, `slug`, `code`) are matched by it, so a reordered list shows only what really changed. Changes of secret fields are not changes. A tick prints at most 100 changes.

`--state` is written with mode 0600 and holds the status and the scrubbed body. A body that is not JSON is saved as its size and a fingerprint only. With `--times 1` and `--state`, `watch` suits cron: one call per run, a diff against the last run.

### Following a channel

```
nexo api watch map.json sse_events --for 60s
```

With `--for`, `watch` opens the channel's event stream with a plain streaming GET (auth read from environment variables as `call` does), reads for that long (1s to 1h) and prints the messages seen, one line per type with its count and rate, and what differs from the map: a message type that did not show is `drift:` (a type not seen in a short window is not proof it is gone), a new type is `new:`, and a changed field type or a missing field are reported like `check` does. At most 30 difference lines. Exit code 1 if there is drift.

A WebSocket channel is followed the same way, over `ws://` or `wss://` (TLS with the system's root certificates): the handshake carries the map's auth headers and cookies and the page's `Origin`, the subscribe messages the map recorded are sent in order, and pings are answered (protocol pings by the library, a recorded application-level ping with the recorded pong). A `<secret>` in a subscribe message is filled from the same auth values as the headers; if one cannot be filled, `watch` names the field and connects to nothing. The window is enforced even when the server sends nothing, a binary frame counts by length only, and the connection is closed at the deadline.

Exit code: 0; 1 if the last call failed; 2 for bad arguments (including `--every` under 10s, `--times 0`, or no `--every` without `--times 1`).

## check

```
nexo api check map.json
nexo api check map.json get_api_items -p item_id=3 --auth bearer=MY_TOKEN
nexo api check map.json --json
```

`check` calls the endpoints of a map once and says whether each live answer still matches what was recorded.

- With no endpoint names it takes every endpoint. With names (id or `"METHOD /path"`) only those.
- Only GET and HEAD are called. Other methods are listed as `skipped` unless you pass `--unsafe`, which calls them too (they may change data; no body is sent).
- An endpoint that needs a parameter you did not give with `-p`, or an unset auth variable, is `skipped` with the reason. A `-p` is only given to the endpoints that have that name.
- `check` never logs in. Auth comes from environment variables only (`--auth NAME=ENVVAR` or the defaults).

Each endpoint ends as one of:

| State | Meaning |
| --- | --- |
| `ok` | Status, content type and shape match. A new field is information only. |
| `drift` | A field that was in every recorded answer is gone, a type changed, a value can be null now, the content type changed, or the answer is no longer JSON. |
| `broken` | The status is one the map never saw (a 404, a redirect), a 401 or 403 where the map saw success (`auth failed`), or there was no answer. |
| `skipped` | Not called, with the reason. |

Output is one line per endpoint, then detail lines (at most 12 per endpoint and 60 in all), then `N endpoints: A ok, B drift, C broken, D skipped`. `--json` prints a `summary` (`endpoints`, `ok`, `drift`, `broken`, `skipped`, `exit`) and an `endpoints` list with `id`, `method`, `path`, `state`, `status`, `recorded_status`, `content_type`, `note`, `drift`, `info`, `broken`, `changes` and `findings`. Only field names, types and statuses are printed, never a value.

| Exit code | Meaning |
| --- | --- |
| 0 | all ok or skipped |
| 1 | some drift, none broken |
| 2 | some endpoint broken |
| 3 | the map or the arguments cannot be used |

In CI:

```sh
export NEXO_API_BEARER="$SITE_TOKEN"
nexo api check map.json -p item_id=3 --json > check.json || echo "API changed: exit $?"
```

## OpenAPI export

`--openapi FILE` on `analyze` writes OpenAPI 3.1; `--json` prints it. It has paths with parameters, request bodies and responses as JSON Schema, and the auth schemes as `components.securitySchemes` (`a1`, `a2`, ...). The title is the first host plus "API". Extensions carry what OpenAPI has no field for: `x-nexo-paging`, `x-nexo-example`, `x-nexo-source`, `x-nexo-cookies`, and `x-nexo-channels` for WebSocket and event-stream channels (id, kind, url, subscribe pattern, heartbeat, and `messages_by_type` with direction, count, rate and schema), `x-nexo-graphql` for GraphQL operations (they share one URL, so they are not paths). A second operation on the same method and path goes under `x-nexo-more-operations`. No secret value is in it.

## client

`nexo api client MAP --lang ts|py|sh [--out FILE] [--only EP]... [--force] [--date YYYY-MM-DD]` writes a small client for the mapped API, so a watcher or an automation can run from cron or CI in your own stack without nexo.

| Language | Needs | What you get |
| --- | --- | --- |
| `ts` | Node 18+ (global `fetch`), no imports | one async function per endpoint, interfaces for request and response bodies, and an async generator `NAME_all` for a paged list |
| `py` | Python 3.9+, `urllib.request` and `json` | the same functions, and a generator `NAME_all` for a paged list |
| `sh` | POSIX `sh` and `curl` | one function per endpoint; paging is left out, with a comment |

The function names are the endpoint ids, so `nexo api call MAP get_api_items` and `get_api_items(...)` are the same call. A name that is a reserved word, starts with a digit or has other characters is fixed (`class_`, `_1st`). `--only` keeps some endpoints (an id, or `"METHOD /path"`). The paging follows what the map says: `page`, `offset`, `cursor`, a next URL, or a Link header, up to a `limit` you give (1000 items by default). A next URL on another origin is never followed.

Secrets come from environment variables when a call runs, with the names `call` suggests (`NEXO_API_BEARER`, `NEXO_API_HEADER_X_API_KEY`, ...). A missing one stops the call with a message that names it. A session cookie is sent only when `NEXO_API_COOKIE` is set. A CSRF header that echoes a cookie reads it from that cookie string unless its own variable is set. No secret, marker or token-like example from the map is written; plain example values appear only in comments. The shell client passes headers to `curl` on standard input, so no token shows in a process list.

Every call times out after 30 seconds, sends a User-Agent that names nexo, does not follow redirects, and retries once on a 429 after `Retry-After` (at most 30 seconds). The header comment holds the origin, the date (`--date`, or today), the operations, and one line per secret that says where it comes from.

GraphQL endpoints become one function per operation name. The map keeps neither the query text nor a persisted-query hash, so the function takes the query text (or `extensions` with the hash) as an argument. WebSocket and event-stream channels are skipped, with a comment that points to `nexo api watch`.

Without `--out` the client is printed. With `--out` it is written, and an existing file is not replaced unless `--force` is given.

```sh
nexo api client map.json --lang py --out items.py
NEXO_API_BEARER="$SITE_TOKEN" python3 -c 'import items; print(items.get_api_items())'
```

## The agent tool

nexo gives the model a tool named `api`. It takes one `command` string, the same line `nexo api` reads (a leading `nexo api` is optional):

```
analyze cap.har --save map.json
describe map.json get_api_items
call map.json "GET /api/items" -p page=2 --dry-run
```

The model can run `analyze`, `describe`, `call` and `client`. It cannot run `watch` or `check` (they run long or call many endpoints, so you run them), and it cannot use `--reveal` or `--body @file`. The answer is the text the CLI prints, cut at 12,000 characters.

Secrets stay out of the model's hands. The tool refuses a command when:

- a `-p` or `--login-param` value looks like a secret, or its name says it is one, and it is not `@env:VAR`;
- a `--body` or `--login-body` holds a secret-looking value or a field with a secret name;
- `--auth NAME=VALUE` carries a token instead of the name of a variable.

The model writes the variable's name instead (`--auth bearer=MY_TOKEN`, `-p password=@env:PW`). It should start with `--dry-run`.

### Permission checks

Each call is classified from the parsed command, and your permission rules decide as they do for the tool that does the same thing by hand:

| Command | Kind | Asked like |
| --- | --- | --- |
| `analyze`, `describe`, `call --dry-run`, `client` without `--out` | read | a file read |
| `analyze` with `--save` or `--openapi`, `client --out` | write | `edit` and `write` |
| `call` of a GET or HEAD endpoint | network | `web_fetch` |
| `call` of any other method, or with `--login` | exec | a shell command that changes things |

More checks run per call:

- The map and the HAR must pass the read rules, and files it writes must not be denied by a write rule.
- A real call is also checked as a `web_fetch` of the endpoint's origin: a rule that denies the host stops it.
- If the map cannot be read, the call counts as exec.
- A relative map path is resolved from the session's folder. If that leads to a different file than the permission check saw, the call is refused: give an absolute path.
- A `--dry-run` shows `@env:VAR` as `<VAR>` and does not read the variable.

The tool is listed as exec, so a sub-agent limited to reading does not get it.

## The MCP route

`nexo re --mcp` serves the kit to other agents over MCP on stdio, and one of its tools is `api`, with the same `command` line and the same refusals as above (no `watch`, `check` or `--reveal`, no secret values).

Reading a map, `analyze` without `--save`, and `--dry-run` always work. A call that sends a request, `--login`, and anything that writes a file are refused unless the server was started with `--allow-changes`:

```
nexo re --mcp --allow-changes
```

`nexo re api <op>` is the same as `nexo api <op>` on the command line, and `nexo re` lists the api ops next to the `bin` and `dbg` ones.

## Example: from a capture to a call

Save a HAR from DevTools while you use `https://shop.example.com`, then:

```sh
nexo api analyze shop.har --host shop.example.com --save shop.json
```

```
42 calls, 6 kept, 3 endpoints; dropped: 30 static file, 4 tracker, 2 page
hosts: https://shop.example.com
auth:
  [a1] bearer in authorization, set by POST /api/login ($.access_token)
[get_api_items] GET /api/items x3 a1
  query: page int
  -> 200 json {items: [{id, name}], total}
  paging: page via ?page, items at $.items
...
```

Ask for one endpoint, and see which variable it reads:

```sh
nexo api describe shop.json get_api_items
```

Call it with a fake token from the environment. The dry run prints the request and sends nothing:

```sh
export NEXO_API_BEARER="example-token-not-real"
nexo api call shop.json get_api_items -p page=2 --dry-run
```

```
GET https://shop.example.com/api/items?page=2
accept: application/json
authorization: Bearer <secret>
```

Drop `--dry-run` to send it. Then keep an eye on it, and check the whole map from cron:

```sh
nexo api watch shop.json get_api_items -p page=1 --every 15m --state items.json
nexo api check shop.json -p page=1
```

The outputs above show the shape of what nexo prints; the numbers and names depend on your capture.
