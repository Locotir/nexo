# How the curator builds the view of a call

Checked against `next` at `cdeb98a` (2026-11-09). The code is `nexo/crates/nexo-curator` (the engine in
`engine/`), driven by `BuiltIn` in `nexo/crates/nexo-agent/src/context.rs` and the turn loop in
`nexo/crates/nexo-agent/src/turn/start.rs`. Constants below are the defaults of `D_SPEC` in `lib.rs`.
Diagrams show the mechanism; their numbers are illustrative, not measured.

## Principles

These are the design. A change that breaks one of them is a bug, whatever it saves.

1. **The curator is never switched off.** `auto`, the default, is the curator. A setting it cannot run is
   left out and the rest still applies (`BuiltIn::lenient`), with one line in the transcript. `plain` and
   `compact` exist as choices for comparison; nothing picks them for you.
2. **No sawtooth, no lossy summary.** The history is never edited and nothing is rewritten by a model.
   The view is chosen by rules over the history, and what leaves stays on disk under a name
   (`m00042`). The view does not fall at each new message: it grows at the end and is rewritten only
   at an epoch, when that pays (below).
3. **Every fact the call needs goes into each call.** What a request names is looked up in an index of
   the whole history and put in the call (the tail, below). Facts the session settled, such as a
   non-default path or port, come back whenever an action supposes the default.
4. **The cache prefix is stable.** Between epochs nothing before the tail changes. What is rebuilt for
   each call goes at the end of the last message.
5. **The view may inflate when a request needs it.** A request that names more than the normal tail
   holds gets its lines up to 99 % of the model's window for that one call. The hard ceiling of any view
   is 95 % of the window.
6. **Not forgetting ranks above cost.** When a rule would save money and lose a fact the agent used,
   the fact wins. On 2026-11-09 the ceiling and a cost rule for rewrites were kept because they cut
   forgotten words by 37.5 % (200 to 125) although replay cost rose 5.64 % (lab log, commits `c8e39dd`
   and `00c9329`).

The `compact` policy and a compaction after the provider rejects a request (`Lesson::Context` in the turn
loop) still exist in the agent. The curator does not use them: it keeps the view under the window so the
fallback should not fire. Whether it never fires in long real sessions is not measured here (Unknown).

## One model call

```mermaid
sequenceDiagram
    participant Host as agent loop
    participant Cur as Curator
    participant LLM as Provider

    Host->>Cur: window, prices, tools, then view(whole history)
    Note over Cur: index only the new messages (lines by key, facts, steps)
    Cur->>Cur: epoch due? (cost rule, ceiling)
    alt epoch
        Cur->>Cur: close answered requests, drop dead outputs, cut to target
    else between epochs
        Cur->>Cur: append only; bring back steps the request names
    end
    Cur->>Cur: tail: standing facts, lines the request names
    Cur-->>Host: view, tokens, what moved
    Host->>LLM: request
    LLM-->>Host: reply
    Host->>Cur: next call (the history grew at the end)
```

The host gives the curator the model's window less the reply (`Curator::window`), scaled by the ratio of
provider tokens to estimated tokens it has learned, with 5 % to spare (`view_window` in `start.rs`), the
cache prices (`Curator::prices`) and the cache usage the provider reported (`Curator::usage`). The
curator's own ceiling is 95 % of what the host gives (`CEILING` in `engine/window.rs`).
`Engine::decide` makes every decision once per call; messages are indexed once, when they arrive.

## The view over a session

```text
 view tokens
 start  |            .'|              .'|
 of cut |         .'   |           .'   |
 target |  ____.'      |____    .'      |____
        |  grows at   epoch   grows at  epoch
        +--------------------------------------> model calls
```

Between two epochs the view only grows at its end, so the provider's cache keeps hitting. At an epoch
the view is rewritten down to `target=10000` tokens in one batch. The epoch comes when keeping the extra
tokens costs more than sending the new prefix once, or when the view reaches the ceiling. It does not come
because a new message arrived.

### When an epoch is due (`epoch_due`, `fire_point`)

Keeping a token in the view costs `ratio` of an uncached token on every later call: the provider's cache
price over its input price when the host gave it, else `ratio=0.1`. A rewrite lands on `L` tokens and sends
that prefix without the cache once, at a price of `margin x L x (1 - ratio)` with `margin=2.25`. The calls
still to come are `ahead=0.25` times the calls so far, at least 4. The rewrite starts once the extra
tokens, times `ratio`, times those calls, exceed that price. Past a view of 100k tokens a quality term
is added: each token past it costs a further 0.05 of an uncached token per call (a heuristic, not a
measured price). With a free cache the rewrite never pays. With `ahead=0` it starts at twice the target.
A fixed `cap` (off by default) replaces the cost rule with a trigger.

An epoch also happens at once when the view passes the ceiling or the model's window, and for the first
view of an engine. After an epoch the view's size is stored, and the next epoch waits for growth past it
by the room the cost rule gives.

### What happens at an epoch

All the work that waited happens together, so the cache is broken once:

- Answered requests close (`close=2`): a request keeps its message and the agent's final reply; its tool
  steps are weighed one by one by the rent rule, against the chance a later reply draws on them (the need
  predictor in `data/model.json`).
- Outputs the agent did not draw on go idle and leave when that pays (`live=1`). Exact copies, repeated
  runs of lines, and reads made out of date by a later edit (`stale=1`) leave first.
- The window cut (`window_cut`) takes the oldest results the sticky set does not hold, down to
  `target`; past the ceiling the oldest sticky results leave too, as pointers, down to 90 % of it.

What the agent is working from is the sticky set, decided by rules with no model: the request, the
agent's own text and calls, the latest `file now` of each path, the last `check:`, the latest whole read
of each path the agent changed or the person named, a `recall` result until the step after it, and what
the agent has read and not yet written about (the latest `still=32000` tokens of it). Failing runs are
kept up to 2,000 tokens, since the agent reports them.

When the head and the protected tail alone do not fit the target, the economic-order-quantity cap
(`eoq=16000`, `Engine::eoq_cap`) prices the cut instead of the target.

## What the call carries beyond the view

Nothing below changes the history or the cached prefix. It is built again for each call and put in the last
message (`engine/tail.rs`).

- **Standing facts** (`standing.rs`). A line of the session's own words that overrides a default (a shell
  rc that is not `~/.bashrc`, a database on another port, a folder not to touch) is kept by target: the stems
  of its words, the base name of its paths and its ports. When the request, the agent's last words or the
  inputs of its last calls hold a target, the fact goes into the tail as a `Standing:` line even though
  nobody asked for it, because the action supposes the default. The table holds 200 facts and drops none for
  budget; a later fact on the same subject replaces it and shows the old one as `antes:`.
- **The index** (`index.rs`, `facts.rs`). Every line of every tool output and message is indexed by its
  keys: words, numbers of any length, paths, versions and identifiers whole. A key weighs by how rare it is
  in the session, so a number or a name few lines hold decides and a common word decides nothing. For the
  person's latest request, the best lines that the view does not hold whole are put in the tail as a
  `Recall` block, grouped by message name, at most 8 per key, 40 lines and 3,000 tokens. Only the text of
  a line is read from the history; the index stores hashes and positions.
- **Inflation** (`INFLATE_TO=0.99`). When the request names more than that holds (40 lines, or lines from
  8 messages or more), it gets 4 times the lines per key and 10 times the total, up to 99 % of the
  model's window, for that call only. The next call builds its tail from the normal limits again. This is
  the only inflate rule in the code today.
- **Steps the request names** (`named=16000`, `engine/named.rs`). A step that read a file the latest
  request names stays, and one already out comes back as text at the end of the view. A file is told from
  its neighbours by the parts of its name they do not share (`assembly-C.txt` next to `assembly-A.txt`).
  A read that an epoch cut after it came back returns by a shared file name only when the request names
  something of its own: a name many reads share (`service.conf` of every folder) would otherwise bring all
  of them back on every request. A step brought back in a message that leaves with an epoch leaves with it.
- **The `recall` tool** is offered unless `recall=0`. It brings a message back as it was by its name:
  `m` and the 1-based index in five digits, so `m00042`.

## What stays in the anchor

The first message carries `[curator]` blocks, each within a budget: the person's rules (`rules=2000`,
never dropped), the agent's notes (`notes=3000`), pinned files (`pin=4000`), the ledger (`ledger=6000`) and
the map of requests that moved out. They are rewritten only at an epoch. [`how-it-works.md`](how-it-works.md)
describes each.

| Piece | Between epochs | At an epoch |
|---|---|---|
| System prompt and tools | Same every call | Same |
| Anchor | Same | Rewritten if the blocks changed |
| Person's messages and the agent's final replies | Stay | Stay |
| Tool calls and outputs of answered requests | Stay | Leave by the rent rule, one line names what left |
| Sticky set | Stays | Stays below the ceiling |
| A step the request names | Comes back at the end, as text | Leaves again if no longer named |
| Standing facts, indexed lines | In the tail, per call | Same |
| Last 12 messages (`protect=12`) | Never cut | Never cut |

The line that replaces a closed request names the calls that left, and the output names (`m00042, m00043;
recall brings any of them back as it was`) when `recall` is offered.

## Topic cards

When the person changes topic, the old topic can be frozen as one card (`cards=8`) so the view holds its
account in a few lines. The split is priced (`split_now`): the cache reads the other topics would cost,
less the rewrite, less the chance they are needed again. The split happens only without epochs. With the
default `epoch=1` it is not made, so cards do not appear in default sessions. They remain for `epoch=0`.
If the agent asks twice for something a split left out, the view is whole again until the next change of
topic.

## Nudge

`nudge=0` is the default. With `nudge=N` the curator appends one request for `Finding:` and `Plan:` notes
to the last tool result after N steps with no note, at most once per N steps, never inside the cached
prefix (`NUDGE` and `NUDGE_SAID` in `engine.rs`). It is off because asking for notes early fixed wrong
readings in measured runs (earlier implementation).

## Where rules are saved

The `note` tool takes a `scope`. With `session` (the default) a `Rule:` line lives in the anchor for the
session. With `global` it is also saved to `~/.nexo/prompt.md`, and with `project` to the project's
instructions file, or `AGENTS.md` at the project root when there is none, dated, under
`## Remembered by nexo` (`nexo-tools/src/note.rs`, `nexo_core::compat::rules_file`). The system prompt
tells the model when to do it (`nexo-agent/src/prompt.rs`). These files are read into the prompt of later
sessions; the curator's `rules` block is what holds a rule within one.

## What is not confirmed here

- Whether a long real session ever reaches the provider-rejects fallback: not measured.
- The topic-card mechanism has not been re-measured since epochs became the default.
- Figures are in [`results.md`](results.md), with dates and commits.
