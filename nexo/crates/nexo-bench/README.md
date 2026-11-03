# nexo-bench

`nexo bench` measures nexo itself on fixed tasks: each task is done by nexo's own agent loop, with
nexo's own tools, under a context policy (an *arm*), then graded, priced and recorded. Running the
same tasks with several arms shows what a context policy changes in score, steps, tokens and money.

## Running it

```sh
nexo bench --upstream https://api.deepseek.com/anthropic --model deepseek-chat --key "$DEEPSEEK_API_KEY" \
    --tasks realm --arms curator --repeats 1 --max-steps 100 --budget 5 --out ../bench-out
```

| flag | meaning | default |
|---|---|---|
| `--upstream URL` | the provider's address | required |
| `--model NAME` | the model | required |
| `--key KEY` | the API key (leave out for a local server) | none |
| `--protocol P` | `anthropic`, `openai-chat`, `openai-responses`, `gemini` | guessed from the address |
| `--tasks LIST` | task ids or groups (`all`, `core`, `long`, `more`, `hard`), comma separated or repeated | `all` |
| `--arms LIST` | arms, comma separated or repeated (see below) | `plain,compact,curator` |
| `--repeats N` | times each task runs with each arm | 1 |
| `--max-steps N` | most steps (model requests that offer tools) per task | 100 |
| `--budget USD` | stop the whole benchmark once this much is spent | none |
| `--max-tokens N` | largest reply, in tokens | nexo's default |
| `--out DIR` | where the results go | `./bench-out` |
| `--skip-preflight` | do not send the preflight request | off |

Before any task, the preflight sends one tiny request asking the model to call the `ls` tool. It stops
the benchmark when the key, the address or the model fail, or when the model writes its tool calls as
text (a server whose tool-call handling does not suit the model), since every task would then end after
one step.

The agents keep their settings, sessions and caches under `<out>/.nexo`, never in the user's own nexo
folders, and every permission is granted (nobody is there to ask). Sub-agents, hooks and MCP servers are
off. The exit code is 0, or 2 when the budget stopped the benchmark.

## Arms

An arm is `[NAME@]POLICY[:k=v;k=v...]`:

- `POLICY` is one of nexo's context policies: `plain` (send everything), `compact` (summarise when a
  request reaches `at` input tokens) or `curator` (the built-in curator);
- the `k=v` pairs after `:` are checked before anything runs:

| policy | parameters | default |
|---|---|---|
| `plain` | none | |
| `compact` | `at=N`: input tokens (system, tools and messages, as the provider counts them) that trigger a summary | **100000** |
| `curator` | the curator spec, exactly what `[context] curator_spec` takes | the engine's |

- `NAME` names the arm in file names and tables (default: the policy). Two arms need different names:
  `--arms curator,tight@curator:eoq=4000`.

Every other repeat runs the arms in reverse order, so no arm always goes first.

### What each arm does

| | plain | compact | curator |
|---|---|---|---|
| history sent | all of it | the summary, then the turns since | the curator's view |
| extra requests | none | **one summary per compaction**, counted in tokens and cost, not in steps | none |
| when | never | every time a request reaches `at`, also several times in one turn | every step |
| what a summary keeps | | **the task statement and the files touched**, written mechanically, plus the model's summary and the last exchange whole | |

**Shared by all arms (harness behaviour, not a policy):** a `read` of a file the model already has,
unchanged and still in what it sees, is answered with a short note instead of the same lines. It
applies equally to plain, compact and curator.

## Output

- `results.jsonl`: one JSON object per task, arm and repeat, appended and flushed after each run (a
  killed benchmark keeps what it finished): `task`, `arm`, `policy`, `spec`, `rep`, `score`, `ok`,
  `detail` (what the checker found), `finished`, `stop` (`max_steps` or `budget` when the bench stopped
  the agent), `steps`, `requests`, `input`, `cache_read`, `cache_write`, `output`, `cost`, `estimated`,
  `tool_calls`, `tool_errors`, `repeated_calls`, `wall_s`, `errors`, `transcript`; and, to reproduce a run,
  `model`, `commit` (the nexo build), `max_steps`, `max_tokens`, `compact_at`; and the context measures
  `compactions`, `rewrites`, `view_tokens` (per step), `final_view_tokens`, `max_view_tokens`.
- `<task>-<arm>-<rep>.jsonl`: the transcript, one line per request: the messages exactly as sent to the
  model (the system prompt when it changes), the reply, its usage and cost. Each line also has
  `step` (null for a summary), `compaction`, `view_tokens` (provider count) and `view_est_tokens`
  (characters / 4). **It never holds the address, the headers or the key**; the key is also scrubbed
  from every line as a safeguard.
- `summary.md` (also printed at the end):

| section | contents |
|---|---|
| header | model, commit, max steps, max tokens, repeats, budget, spent |
| Arms | one row per arm: settings, runs, **score** (mean), steps, requests, input, cache read, output, cost $, wall s, tool errors (sums), final view and max view (means), compactions, rewrites (sums) |
| Paired with the first arm | per other arm, over the tasks both ran: Δ score (points), Δ cost, Δ steps, each with a **95% paired bootstrap interval** (10000 resamples, fixed seed); left out under 2 tasks |
| Runs | one row per run, same columns |

What the columns mean:

- **steps**: requests that offered tools, one per step of the loop (a compaction summary is a request,
  not a step). `--max-steps` caps them; a run that reaches the cap is not `finished`.
- **requests**: every request, steps and compaction summaries alike.
- **input**: input tokens that missed the cache; **cache read** those served from it.
- **view**: the input tokens of a step's request (system, tools and messages), as the provider counts
  them (characters / 4 when the provider gives no usage); **final view** is the last step's. All token counts are the provider's own usage figures.
- **compactions**: summary requests. **rewrites**: steps whose messages do not start with the previous
  step's messages (the history was changed, not just extended). Plain has none of either.
- **cost**: the provider's token counts priced per million. DeepSeek (`deepseek-chat`) is 0.28 input,
  0.028 cache hit, 0.42 output; other models use the prices nexo's provider presets know, and a model
  with no known prices is priced as DeepSeek and marked estimated.
- **tool errors**: tool results flagged as errors.
- **repeated calls**: a tool called again with the same arguments (in any key order) that got the same
  output back: the model asked for something it already had.

## Scores

Every task has a checker that grades the working directory once the agent stops; the score is 0..1.
Most tasks are small Rust projects checked with `cargo test --offline` (no dependencies), often with
hidden tests the checker adds to a copy of the project, plus checks that the trap was respected (a rule,
a value, a file left alone). A task scores the share of its checks that pass, or 1/0 when the parts
only make sense together.

`realm` scores the share of 400 unseen cases its `coins.rs` gets right.

## Tasks

| id | group | what it tests |
|---|---|---|
| `rules-first` | core | rules stated once at the start must hold until the end |
| `noisy-log` | core | the one useful line is buried among about 600 log lines |
| `early-detail` | core | values from the first message are needed only after unrelated work |
| `plan-change` | core | the goal changes mid-run; the old one must be dropped |
| `misleading-error` | core | the error points at one file, the cause is in another |
| `cross-session` | core | a second session needs values given only in the first |
| `long-haul` | long | four requests: bugs, a noisy audit, and a release note that needs facts from the start |
| `marathon` | long | nine requests: bugs, an audit, five specifications, and facts from the start |
| `three-sessions` | more | three sessions; later ones need a number and a naming rule from the first |
| `selective-revert` | more | the first of three edits is cancelled; the old code exists only in the history |
| `stale-doc` | more | a correction to a file is given once, early; later the file is to be followed |
| `realm` | hard | reverse engineering a game binary whose rule is spread over data, binary and save |

Multi-session tasks restart the agent between sessions and resume the saved session, as `nexo --resume`
does.

### realm

The working directory holds a compiled game (`./realm SAVE ENEMY ZONE` prints the coins a fight pays),
its data files, a save, an out-of-date manual and `harness.rs`. The model must write `coins.rs` with
`pub fn loot(save: &Save, enemy: &str, zone: &str) -> i64` that pays exactly what the game pays. The
checker copies `coins.rs` and the harness into a fresh folder, away from the game, compiles them with
`rustc`, and runs the 400 unseen cases through stdin and stdout as JSON lines. A `coins.rs` that runs
programs, reads files, opens sockets, embeds files (`include_str!`, `include_bytes!`) or uses `unsafe`
scores 0.

The game is built from C with the system C compiler (`cc`, `gcc` or `clang`), so realm needs one, plus
`rustc`. It is also *sealed*: on Linux, every shell command of the agent and the checker's run go
through `unshare -r -m -n -p --mount-proc --fork` with no network, the system read-only and the home and
temporary folders hidden, except the working directory and the Rust toolchain; the folder the benchmark
runs from and `--out` are hidden too, file tools refuse paths outside the working directory, and the
programs a task names in `mask` (realm: the script interpreters) answer "not found" inside the box. Where
that is not possible (another system, no user namespaces), realm is skipped and the summary says why.

## Adding a task

1. Write a function returning a `Task` (in `src/tasks/`): its `id`, `trap`, `group`, the files of the
   working directory, the `turns` (`(session, message)`; a new session number restarts the agent on the
   saved session), a `check` that grades the directory into an `Outcome`, a reference `solution` that
   edits a fresh directory into a passing one, and `ready` (whether this machine can run it). `setup`
   builds what files cannot carry; `sealed` asks for sealed commands.
2. Add it to `tasks::all()`.
3. Run `cargo test -p nexo-bench -- --ignored`: every checker must give its own solution full marks and
   reject the untouched start.
