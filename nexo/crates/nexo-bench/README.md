# nexo-bench

`nexo bench` measures nexo itself on fixed tasks: each task is done by nexo's own agent loop, with
nexo's own tools, under a context policy (an *arm*), then graded, priced and recorded. Running the
same tasks with several arms shows what a context policy changes in score, steps, tokens and money.

## Running it

```sh
printf %s "$DEEPSEEK_API_KEY" | nexo bench --upstream https://api.deepseek.com/anthropic --model deepseek-chat --key - \
    --tasks realm --arms curator --repeats 1 --max-steps 100 --budget 5 --out ../bench-out
```

| flag | meaning | default |
|---|---|---|
| `--upstream URL` | the provider's address | required |
| `--model NAME` | the model | required |
| `--key KEY` | the API key (leave out for a local server; `-` reads it from stdin, out of the process list) | none |
| `--protocol P` | `anthropic`, `openai-chat`, `openai-responses`, `gemini` | guessed from the address |
| `--tasks LIST` | task ids or groups (`all`, `core`, `long`, `more`, `hard`, `padded`), or the external benchmark's `repoqa` and `canonstep` (with `--llmb`), comma separated or repeated | `all` |
| `--arms LIST` | arms, comma separated or repeated (see below) | `plain,compact,curator` |
| `--repeats N` | times each task runs with each arm | 1 |
| `--max-steps N` | most steps (model requests that offer tools) per task | 100 |
| `--budget USD` | stop the whole benchmark once this much is spent; with no prices for the model it refuses to start | none |
| `--price MISS,HIT,OUT` | the model's prices in dollars per million tokens: input on a cache miss, on a cache hit, output | nexo's own (DeepSeek chat and reasoner, and the models its provider presets price) |
| `--max-tokens N` | largest reply, in tokens | nexo's default |
| `--temperature T` | sampling temperature of every request | the provider's own |
| `--out DIR` | where the results go (refused when it holds an earlier `results.jsonl`) | `./bench-out` |
| `--skip-preflight` | do not send the preflight request | off |
| `--llmb DIR` | the external benchmark's distribution folder (for `--tasks repoqa,canonstep`) | none |
| `--instances IDS` | instance ids of `repoqa` and `canonstep`, comma separated | all of them |
| `--limit N` | at most this many instances per task, in id order | none |
| `--cache DIR` | where `repoqa`'s repositories are cloned once | `<out>/.cache/repos` |

`nexo bench report [RESULTS...] [--out DIR] [--summary FILE]` writes `report.html` in `--out` (default
`./bench-out`) from one or more `results.jsonl` (files or folders holding one; default: `--out`).
`--summary FILE` also writes the Markdown summary there, with the counters read again from the
transcripts. When every input is a `results.jsonl` with its `bench.json` (a copy or a filtered file has none: the folder's figures describe the whole run), both files show the run's own spent figure (skipped
runs included), and the summary its budget (one input), "Stopped early" line and skipped tasks; else
they only know the recorded runs.

Before any task, the preflight sends one tiny request asking the model to call the `ls` tool. It stops
the benchmark when the key, the address or the model fail, or when the model writes its tool calls as
text (a server whose tool-call handling does not suit the model), since every task would then end after
one step.

The agents keep their settings, sessions and caches under `<out>/.nexo`, never in the user's own nexo
folders, and every permission is granted (nobody is there to ask). Sub-agents, hooks and MCP servers are
off. Every program started for a run (the agent's shell commands, the language servers and checkers of
`diagnostics`, the graders' commands) gets a cleared environment: only `PATH`, `LANG`, `LC_*`, `TERM`,
the Rust toolchain's `CARGO_HOME`, `RUSTUP_HOME` and `RUSTUP_TOOLCHAIN`, and a `HOME` and `TMPDIR` of
the run's own, so no key, token or session socket reaches them (on Windows' PowerShell only the bench
key is removed). On Windows names match without regard to case (`Path`), what programs need to start
is kept too (`SystemRoot`, `windir`, `ComSpec`, `PATHEXT`, `SystemDrive`, `OS`, `NUMBER_OF_PROCESSORS`,
`PROCESSOR_ARCHITECTURE`, `ProgramData`, the `ProgramFiles` and `CommonProgramFiles` folders), and
`USERPROFILE`, `APPDATA` and `LOCALAPPDATA` point into the run's home.
The exit code is 0, or 2 when the budget cut a run or left runs undone.

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

The arms run in a seeded shuffle for every task and repeat, so no arm always goes first and the provider
cache is shared evenly.

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
- `bench.json`: what the records do not hold (budget, spent with the skipped runs, why it stopped
  early, the skipped tasks), for `nexo bench report`. It names the results file it describes, and a new run removes the old one.
- `summary.md` (also printed at the end):

| section | contents |
|---|---|
| header | model, commit, max steps, max tokens, repeats, budget, spent (every priced reply, runs skipped after an error included, with their share) |
| Arms | one row per arm: settings, runs, **score** (mean), steps, requests, input, cache read, output, cost $, wall s, tool errors (sums), final view and max view (means), compactions, rewrites (sums) |
| Paired with the first arm | per other arm, over the tasks both ran: Δ score (points), Δ cost, Δ steps, Δ recall calls, Δ re-reads, Δ dup lines (points), Δ wall s, each with a **95% paired bootstrap interval** (10000 resamples, fixed seed) once there are at least 5 tasks and 3 runs per task and arm (else "not enough runs"); left out when the arms share no task |
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
  0.028 cache hit, 0.42 output; other models use the prices nexo's provider presets know; `--price`
  overrides both, and a model with neither has its cost shown as unknown, never guessed from another vendor.
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
| `marathon-heavy` | heavy | the marathon with twenty times the examples in each specification: the history passes 200k tokens, so a summary at 100k or 200k falls in the middle of the work |
| `marathon-noisy` | heavy | the heavy marathon plus junk: a build script that prints 150 useless notes after every edit and twelve long CI logs (400 KB) holding one fact, asked for at the very end; the history passes 200k tokens early and most of it is noise |
| `marathon-pasted` | heavy | the noisy marathon with its junk pasted into the requests (twelve CI logs, five long specifications): it stays in every later request whatever the agent reads, so the history passes 200k tokens in every run |
| `three-sessions` | more | three sessions; later ones need a number and a naming rule from the first |
| `selective-revert` | more | the first of three edits is cancelled; the old code exists only in the history |
| `stale-doc` | more | a correction to a file is given once, early; later the file is to be followed |
| `padded-revert-code` | padded | `selective-revert` across three files, with over 8,000 tokens of pasted material in between |
| `padded-revert-data` | padded | the same in a data project: undo the first of three edits to a SQL transform and a CSV |
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
temporary folders, `/run` (but for the PATH folders under it, put back read-only, as on NixOS) and `/dev/shm`
hidden, except the working directory and the Rust toolchain;
the folder the benchmark runs from and `--out` are hidden too, file tools refuse paths outside the
working directory, `diagnostics` is not offered (its language servers would run outside the box), and
the programs a task names in `mask` (realm: the script interpreters) answer "not found" inside the box,
by name and by full path. Where that is not possible (another system, no user namespaces), realm is
skipped and the summary says why.

## Adding a task

1. Write a function returning a `Task` (in `src/tasks/`): its `id`, `trap`, `group`, the files of the
   working directory, the `turns` (`(session, message)`; a new session number restarts the agent on the
   saved session), a `check` that grades the directory into an `Outcome`, a reference `solution` that
   edits a fresh directory into a passing one, and `ready` (whether this machine can run it). `setup`
   builds what files cannot carry; `sealed` asks for sealed commands.
2. Add it to `tasks::all()`.
3. Run `cargo test -p nexo-bench -- --ignored`: every checker must give its own solution full marks and
   reject the untouched start.
