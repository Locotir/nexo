# nexo: architecture

`nexo` is a terminal coding harness: a full-screen TUI (and a headless mode)
that drives a model with tools over any provider, with a built-in context curator as its default context policy.
Written in Rust, like Grok Build and Codex; the TUI is ratatui + crossterm.

## Crates and the one rule

```
nexo-core        contracts only: Message/Part, StreamEvent, Protocol, Tool, AgentEvent/Command, Config, paths
nexo-providers   protocol adapters (openai-chat, openai-responses, anthropic, gemini, mock), presets, retry, SSE
nexo-tools       built-in tools (read, write, edit, bash, glob, grep, ls, todo, note, web_search, web_fetch) and the MCP stdio client
nexo-agent       the loop: turns, tool execution, permissions, sessions (JSONL), cost, context policies, the curator policy
nexo-curator     the context curator: the goal engine (ledger, rules/notes/pin blocks, rent-or-buy cuts), no I/O
nexo-tui         the interface: consumes AgentEvent, produces AgentCommand; never touches providers or tools
nexo-cli         the `nexo` binary: wires config → providers → tools → agent → tui / headless
nexo-bench       `nexo bench`: agent tasks run against a real model under several context policies, scored and summed up
```

Dependency direction: `core ← providers, tools, agent, tui ← cli`. The TUI
depends on **core only** (it is driven through the channels of
`nexo_core::agent`); the agent depends on core, providers, tools and
the curator (which depends on no other nexo crate); the benchmark drives the
agent the way the binary does, and the binary depends on everything. Nothing depends upward or sideways. A crate that needs
a type another crate owns asks for it to move into core.

Conventions, all crates:

* English, short doc comments on every public item saying what it is for.
* `cargo test -p <crate>` green and `cargo clippy -p <crate> --all-targets`
  clean before the crate is handed over; no `unwrap()` on data from the
  network, a file or the user.
* Workspace dependencies only (`name = { workspace = true }`); a crate that
  needs a crate the workspace lacks adds it to the **workspace** table with a
  version and uses it from there.
* No secrets anywhere: keys come from the environment variable or command the
  provider names, are held in memory and never logged, printed or written.
* Nothing in the tree says who or what wrote it.
* Tests do not reach the network: adapters are tested on recorded fixtures
  (`tests/fixtures/*.sse`) and a local mock server (`httpmock`).

## Public API each crate commits to

### nexo-providers

```rust
pub fn registry() -> Registry;                       // the protocols by id
impl Registry { pub fn get(&self, id: &str) -> Option<Arc<dyn Protocol>>; pub fn ids(&self) -> Vec<&'static str>; pub fn register(&mut self, p: Arc<dyn Protocol>); }
pub fn presets() -> BTreeMap<String, ProviderConfig>; // openai, anthropic, gemini, deepseek, xai, groq, mistral, openrouter, together, fireworks, cerebras, moonshot, qwen, zhipu, minimax, ollama, lmstudio, vllm, llamacpp (with models, pricing, context windows where known)
pub struct Client;                                     // retry/backoff around a Protocol
pub mod catalogue;                                     // the models.dev dataset: `lookup(preset, model) -> (pricing, context, output)`, `cached()`, `refresh()`, `refresh_if_stale(done)` (7-day cache at `~/.cache/nexo/models.json`, never blocks a request)
pub mod hints;                                         // what an error message says about the model: `context_window(msg)`, `max_output(msg)`, `unknown_model(err)`, `closest(wanted, names, n)`, `wait_hint(err)`
impl Client { pub fn new(registry: Registry) -> Client; pub async fn stream(&self, cfg: &ProviderConfig, req: &ModelRequest, retries: u32) -> Result<EventStream, ProviderError>; pub async fn list_models(&self, cfg: &ProviderConfig) -> Result<Vec<ModelInfo>, ProviderError>; pub fn registry(&self) -> &Registry; }
pub mod sse;                                           // `parse(bytes stream) -> Stream<SseEvent{event, data}>`
pub mod mock;                                          // protocol "mock": scripted replies for tests and demos
```

The mock protocol: `ProviderConfig { protocol: "mock", extra.script: path }`;
without a script it echoes the last user text as `"echo: …"`; a user text
that starts with `tool:` makes it call the named tool once
(`tool:read {"path":"a.rs"}`) and then answer with the result's first line.
A script file is a JSON list of replies, each a list of StreamEvents in a
simple JSON form (`{"text": "…"}`, `{"reasoning": "…"}`, `{"tool": {"id","name","args"}}`,
`{"usage": {...}}`, `{"finish": "stop"}`), consumed one reply per request.

### nexo-tools

```rust
pub fn builtin_with(web: &WebConfig) -> ToolSet;       // read, write, edit, bash, glob, grep, ls, todo, note, web_search, web_fetch (one set per session)
pub mod mcp { pub struct McpClient; impl McpClient { pub async fn connect(name: &str, cfg: &McpServerConfig) -> anyhow::Result<McpClient>; pub fn tools(&self) -> Vec<Arc<dyn Tool>>; pub async fn close(self); } }
```

`todo` keeps the model's task list for multi-step work: each call replaces
the whole list (`items: [{text, status: pending|in_progress|done}]`, at most
50 items of 200 characters), a call without `items` shows it, the output is a
`ToolDisplay::Todo` the interface shows in its panel, and the list is also
written to `<sessions>/<id>.todo.json`. It never asks for permission
(`permission::ALWAYS_ALLOWED`).

`note` keeps what the model learnt (`text`: one line per item, beginning with
`Finding:`, `Plan:` or `Rule:`, 4,000 characters at most): the tool changes
nothing and answers how many lines it counted; the context policy that keeps
notes (the curator) reads the call itself, so the lines outlive their message.
It never asks for permission either.

Tool names are lowercase; their argument keys are the common ones
(`path`, `command`, `pattern`, `old_string`/`new_string`), which the curator's
classifier recognises. `read` returns numbered lines (`   12│ text`) with
`offset`/`limit`; `edit` replaces one exact occurrence (or all with
`replace_all`) and returns a `ToolDisplay::Diff` (the file before and after, `start_line` 1; a
permission preview shows only the replaced text and carries the line where it sits); `write` creates parents;
`bash` runs `sh -c` in `cwd` with a timeout (default 120 s, max 600 s), merges
stdout/stderr in order, caps output at 30k characters keeping head and tail,
and streams lines as `ToolProgress`; `glob` and `grep` use the `ignore`
walker (gitignore-aware) and `regex`; `ls` lists one directory. Paths are
resolved against `cwd`; a path outside `cwd` is allowed but the summary says
so, for the permission dialog. MCP tools are named `mcp__<server>__<tool>`
and are `ToolKind::Other` unless the server annotates them read-only.

### nexo-agent

```rust
pub struct AgentSetup { pub config: Config, pub cwd: PathBuf, pub provider: String, pub model: String, pub tools: ToolSet, pub client: Arc<Client>, pub resume: Option<String>, pub gate: Option<Arc<dyn PermissionGate>> }
pub async fn run(ends: AgentEnds, setup: AgentSetup) -> anyhow::Result<()>;   // until Shutdown
pub mod session { pub struct SessionMeta { id, cwd, created, updated, provider, model, title, messages: usize }; pub struct Session; pub fn list() -> Vec<SessionMeta>; pub fn latest_for(cwd: &Path) -> Option<SessionMeta>; impl Session { pub fn create(cwd, provider, model) -> Session; pub fn open(id) -> anyhow::Result<Session>; pub fn messages(&self) -> &[Message]; pub fn outputs(&self) -> &BTreeMap<String, ToolOutput>; pub fn append(&mut self, m: &Message) -> anyhow::Result<()>; ... } }
pub mod context { #[async_trait] pub trait ContextPolicy: Send { fn name(&self) -> &str; async fn view(&mut self, system_tokens: u64, history: &[Message]) -> anyhow::Result<View>; fn reset(&mut self); } pub struct View { pub messages: Vec<Message>, pub tokens: u64, pub note: Option<String> } pub struct Plain; pub struct Compact; pub struct Curator; pub fn from_config(cfg: &ContextConfig, ...) -> Box<dyn ContextPolicy> }
pub mod permission { pub fn rule_matches(rule: &str, tool: &str, kind: ToolKind, args: &Value) -> bool; pub struct Policy { ... } }
pub mod prompt { pub fn system_prompt(cwd: &Path, cfg: &AgentConfig, tools: &ToolSet) -> String; pub fn source_file(cfg) -> Option<PathBuf>; pub fn built_in() -> &str; pub const RAW_MARK }
pub mod limits { pub struct Limits { pricing, context_window, max_output, source }; pub fn resolve(provider, cfg, model, vendor: Option<&[ModelInfo]>) -> Limits }
pub mod config_edit { pub fn set(file, path, key, value); pub fn push(file, path, key, s); pub fn set_pricing(file, provider, model, p); pub fn set_limit(file, provider, model, which, n) }   // edits that keep comments and order (toml_edit)
pub mod gate { pub struct ChannelGate; }   // turns a PermissionRequest into AgentEvent::PermissionAsk and waits for AgentCommand::Permission
```

The loop, per `Submit`: append the user message (with `@file` attachments
expanded as fenced blocks); then up to `max_turns` times: build the view
through the context policy; call `client.stream`; forward deltas as events
while collecting; append the assistant message; if no tool calls, end the
turn; else run the calls **in order** (parallel when all are `Read`/`Search`),
each through the permission policy (`deny` rules → denied without asking;
mode `Auto` or an `allow` rule → allowed; else the gate), emitting
`ToolCall/ToolStart/ToolProgress/ToolDone`; append one user message with the
results; loop. `Interrupt` cancels the stream and the running tools; the
partial assistant message is kept and the turn ends `Interrupted`. A
`Submit` while a turn runs is queued and starts when the turn ends; the
queue's length is reported as `Queued { pending }` whenever it changes. A
`ContextLength` error compacts once and retries. Usage is accumulated per
turn and session and priced with the provider's pricing (`cost_usd`, 0 when
unknown). The `Context` event is emitted after every view. Every message and
tool output is appended to the session file as it happens, so a crash loses
nothing and `resume` replays `History` events.

Between turns the conversation can be replaced or cut: `Resume { id }` opens
another recorded session in place; `Rewind { index }` keeps the first
`index` messages (never cutting between an assistant's calls and their
results) and rewrites the session file atomically (temporary file, then
rename), keeping its usage; `Clear` starts a new session. Each of them
emits `HistoryCleared`, then `SessionChanged { session_id, title }`, then the
replay (`History`, `ToolCall`/`ToolDone`, `Usage`); sent during a turn they
answer with a warning notice. `Export { path }` writes the conversation as
markdown (`path` relative to the working directory, or next to the session
file as `<id>.md` when none is given) and answers with a notice naming the
file.

Goals: `SetGoal { Some(text) }` pins the goal at the end of the system prompt
(`Goal: …` followed by the convention: end the reply with the single line
`GOAL DONE` when it is met, or when nothing is left to do), records it in the
session file (`{"type":"goal"}`, restored on resume), answers with `Goal {
text, round: 0, max }` and **starts the work**: when no turn runs, the goal
text is submitted as the request (`Goal: <text>`); while a turn runs it is
only pinned (a notice says so) and checked when that turn ends; with requests
queued it runs after them. When a turn ends on its own (`Done`) and the reply
does not declare the goal done (`GOAL DONE` as its last non-empty line, or
anywhere in its last 200 characters, case-insensitive), the agent submits
`The goal is not marked done yet. Keep working on it…` by itself, once per
round, up to `max` rounds (5; `NEXO_GOAL_ROUNDS`), with a `Goal` event and a
notice per round. The first continuation always runs; a continuation round
that called no tool did nothing new, so the loop stops there with a notice
instead of asking again; after the last round a warning says the goal is
still open. Whenever the loop stops (done, nothing to do, cap) the rounds go
back to 0 in the `Goal` event and the goal stays set (`/goal off` clears it);
a new request from the person resets the rounds too; an interrupt stops the
continuation. Queued requests run before a continuation.

Permission rules: `tool` (any call of it), `tool(glob)` where the glob is
matched against the call's target: the command for `bash` (prefix globs such
as `bash(git *)`, `bash(cargo test*)`), the path for file tools
(`edit(src/**)`, `read(/etc/*)`), the tool name for MCP tools
(`mcp__server__*`). `deny` beats `allow` beats the mode.

Session file: `~/.local/share/nexo/sessions/<id>.jsonl`, one JSON object per
line: `{"type":"meta",...}` first, then `{"type":"message","index":n,"ts":...,"message":{...}}`,
`{"type":"tool_output","call_id":...,"output":{...}}`, `{"type":"usage",...}`,
`{"type":"title","title":...}`. The id is `YYYYMMDD-HHMMSS-<4 hex>`.

Model limits: what is known of the model in use (prices per million tokens,
context window, largest reply) is resolved in this order, each source
filling what the previous left empty: the configuration (`pricing`,
`context_window`, `max_output` under `[providers.<name>]`), the preset, the
models.dev catalogue, the vendor's own model list (asked for in the
background when something is still missing). The agent emits `ModelLimits`
after `Ready`, after `SetModel`, on `ShowModel`, after `SetPricing` and after
a correction. `SetPricing` writes `[providers.<name>.pricing."<model>"]` to
the **user** file through `config_edit` (comments and order kept). Every
request sends `max_tokens = min(agent.max_tokens, max_output)`.

Corrections from errors: when a request fails, `hints` reads the message.
A context-length message that names the window (`maximum context length is
131072 tokens`, `prompt is too long: 213000 tokens > 200000 maximum`, Gemini's
`input token count (300000) exceeds the maximum number of tokens allowed
(262144)`, `256k`, `1,048,576`...) stores `context_window` for the model in
the user file and the compaction path runs; a `max_tokens` message
(`max_tokens: 65536 > 8192, which is the maximum allowed`, `must be at most
32768`, `Expected a value <= 16384`) stores `max_output`, caps `max_tokens`
and retries the request once; an unknown model (`does not exist`, 404) gives
a notice with the closest names from the configuration and the catalogue;
a rate limit's `retry-after` is shown in the notice. Each correction emits
`ModelLimits` with `source: "error"`.

The person's prompt: `prompt::system_prompt` takes its base text from
`agent.system_prompt_file`, else from `~/.config/nexo/prompt.md` when it
exists, else the built-in text; the tool guidance, directory, date, project
instructions and `system_prompt_extra` are appended unless the file starts
with the line `<!-- nexo: raw -->` (then it is sent as is). The prompt is
rebuilt at the start of every turn, so an edit applies to the next request.

Built-in curator (`context::BuiltIn`, crate `nexo-curator`): the goal engine,
the default for `auto` and `curator` and the only curator nexo has. It keeps
the history it was last given and the JSON of each message, so a call converts
only what changed; the engine itself caches each message's cut and the visible
list, so a call does little beyond the new messages. `tests/golden.rs` replays
the golden fixtures of `tests/fixtures/*.jsonl` (`NEXO_UPDATE_GOLDEN=1`
records them again) and `examples/bench.rs`
measures the time of a call. Its default spec is `D_SPEC`, which holds the
default of every value (`eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;
protect=12`, birth `squeeze`, a pure view with `directives=0;nudge=0`, and
the closing, idle and copy mechanics `close=2;live=1;dedup=3;named=1` among
others; [`docs/how-it-works.md`](../docs/how-it-works.md) shows it whole and a test keeps that copy
equal). `[context] curator_spec` is laid over it, pairs separated by `;`.
`params` refuses an unknown key, a negative or non-finite number and a key
the engine does not support (`spill`, `recall`, `ttl` …); the agent leaves
such a pair out, runs the rest and says so in a `Notice`. Besides the ledger, the
anchor carries three `[curator]` blocks harvested from the messages that leave
the view, each within its token budget: `rules`, `notes` (the model's
`Finding:` / `Plan:` lines) and `pin` (the last full read of the files the
request names and the last full write of the files the model wrote); the ledger gets what they leave of 80% of the target. Nothing
is written to disk. Each view also carries the curator's memory (a cell per
message, `state` one of `whole`/`cut`/`recalled`/`out`, zone 0 the anchor and
one per topic), which `/context-live` draws.

The rules block: the person's lines that read as a standing instruction
(English and Spanish cues such as never, always, don't, from now on, nunca,
siempre, prohibido, recuerda, a partir de ahora) and the lines the model
restates as `Rule: …` are kept whole for the whole session. They are never
dropped to make room: when the anchor is short, the ledger shrinks instead.
So a correction typed once in the chat holds natively, without a system
prompt or a file.

The nudge: the counter `since_note` resets on an assistant message with a
`Finding:`, `Plan:` or `Rule:` line, in its text or in the input of a `note`
call (`NOTE_TOOL`), and goes up otherwise; once it reaches `nudge` (and the
view has been rewritten, chunks were dropped or the history is past `protect`)
the engine appends `NUDGE`, a request for a `note` call, to the end of the last
tool result of the last message, never inside the cached prefix, and at most
once per `nudge` steps (`since_nudge`). The host tells the engine the tools it
offers (`Engine::tools`, the `tools` list of a `view` request, `ContextPolicy::tools`
before each view): without a `note` tool the request is `NUDGE_SAID`, the same
lines at the start of the reply, and the directives change the same way
(`CURATOR_DIRECTIVES_SAID`). The request waits while the agent is between two
parts of one file (`mid_read`: its last call was a read with a range,
`tools::partial_read`), bounded by `since_note < 2·nudge`. Only the latest
request stays (`nudged` holds one message; the earlier one is rebuilt plain),
and it leaves once a note answers it, so the view carries one request, not a
pile. The stat is `nudges`. Each of these came from a measured run: a request
for a tool that was not offered stayed unanswered for nineteen requests, and a
request between the first and the second part of a file turned the read into a
search. Birth (`at_birth`) squeezes only the output of a process (`Kind::Exec`
or `Kind::Other` by `tools::classify` of the call the result answers): what a
read or a search tool returns is material and stays as it came. The `note` call itself is harvested
like a text unit: its `Finding:`/`Plan:` lines go to the notes, its `Rule:`
lines to the rules.

Written files: `tools::full_write` recognises a whole-file write (a content
key, not an edit/patch/replace tool unless `command=create`); the engine keeps
them in `wrote` by basename and shows them, once their write left the view,
under "[curator] Files you wrote, as last written (whole):", newest first,
each capped at `WROTE_TOKENS` (1,500; head and tail kept around "… N lines
cut …"). `harvest` trims read pins and write pins together against `pin`: a
scratch write (`_` or `tmp` prefix) goes first, then the older of the oldest
write and the oldest read, and at least one pin stays. A later edit marks the
entry "edited later in mNNNNN: re-read before trusting"; an edit of at least
half the pinned text drops it; a new write replaces it; a write whose result is
an error removes it. Write pins are not carried across a sync reset.

### nexo-tui

```rust
pub struct TuiOptions { pub cwd: PathBuf, pub provider: String, pub model: String, pub models: Vec<(String, String)>, pub theme: String, pub mouse: bool, pub show_reasoning: bool, pub fold_after: usize, pub session_id: Option<String>, pub sessions: Vec<(String, String)> /* id, title */ }
pub async fn run(handle: AgentHandle, opts: TuiOptions) -> anyhow::Result<()>;   // takes the terminal until quit
```

## The interface, specified

Like Grok Build (full screen, mouse, modals, palette), and then better where
it counts: the transcript is a pager you can read, fold, search and copy from;
nothing flickers; nothing is lost on resize.

The status bar's pieces, in drop order when the terminal is narrow: the directory goes first, then the branch, then the cache share and call count (`81% cached · 12 calls`), then the memory (`mem <history> → <sent>` from `Context.history_bytes`/`view_bytes`), then the gauge's counts, then the model is shortened, then the goal tag, the gauge and the mode; a piece is shown whole or not at all. The gauge is the last call's input over the model's window (`ModelLimits`), coloured by share (<50% calm, 50–75% yellow, 75–90% orange, ≥90% red); without a known window it shows the tokens sent, uncoloured. The hint bar keeps the keys and bounds the session name to 24 columns.

Layout, top to bottom: **title bar** (provider/model, directory with the git
branch, permission mode, `◎ goal` while a goal is set, the context gauge `▰▰▱▱▱ 42% 54k/128k policy`: the
last call's input over the model's window, session tokens and cost at the
right; pieces are dropped from the end when the bar is narrow, never cut);
**transcript**; **input**; **key hints**.

An empty session shows a bordered banner (version, model, directory, branch,
permission mode, three key hints, "type / for commands"); it leaves at the
first message (notices may sit under it).

Transcript blocks, each rendered once and cached until it changes:

* person: accent bar at the left, the text as typed, `HH:MM` dim at the right
  when the message was sent from this interface;
* assistant: markdown (headings bold in the accent with a dim `#` level cue,
  `•`/`◦` bullets, numbered lists aligned, quotes with a rail, tables, links
  underlined with the URL dim, inline code tinted, fenced code with a thin
  left rail, syntax highlighting and the language label at the right);
* reasoning: dim, folded by default to one line (`▸ thinking · 14 lines · 3.1s`);
* tool call: a card with status (spinner, ✓, ✗, ⊘), the summary line
  (`read path · n lines`, `edit path · +n −m`, `bash command · exit c`),
  elapsed time, and the body under a rail in the status colour, folded after
  `fold_after` lines (`… 212 more lines`); `edit` shows a unified diff with
  tinted ± lines; `bash` the command and its output; `read` the file with
  line numbers on the code background; `todo` the task list (`○ pending`,
  `◐ in progress`, `● done`, `done/total` in the header);
* notices (retries, compaction, errors) in one line each;
* the model card (`◈ model provider/name · 128k context · 8k max reply ·
  $0.28 in · $0.42 out · $0.028 cached · from catalogue`) after a model
  change, a correction or `/pricing`;
* a usage line at the end of each turn (tokens in/out/cached, the turn's
  cost when known, time).

Navigation: wheel and PageUp/PageDown scroll; Home/End; the view follows the
stream while at the bottom, and shows a `↓ n new` pill when scrolled up;
a click selects a block and a click on the selected block's head toggles its
fold; `Shift+↑` (or Tab) on an empty draft selects the last block, then ↑/↓
walk the blocks, →/← open and fold, Enter toggles, Esc goes back; `Ctrl+O` folds
or unfolds all tool bodies; `Ctrl+T` shows or hides reasoning; `Ctrl+F`
searches the transcript (n/N to jump, matches highlighted); `Ctrl+Y` copies
the last assistant message (clipboard, OSC 52 fallback); `Ctrl+L` redraws;
`Ctrl+B` toggles the side panel (the goal in force with its round, the todo
list with `○`/`◐`/`●` and `done/total`, then the changed files with `+n −m`);
a drag selects text and copies it
when the button comes up (`Shift`+drag leaves the selection to the terminal;
`/mouse` turns capture off altogether).

Input: multi-line editor; `Enter` sends, `Ctrl+J` (and `Alt+Enter`) inserts a
newline; `↑`/`↓` at the first/last line browse the prompt history; `Esc`
interrupts a running turn, a second `Esc` clears the draft (idle and empty:
the rewind picker); `Ctrl+C` once interrupts, twice within two seconds quits;
`/` at the start opens the command palette (fuzzy list, `Tab`/`Enter` to
complete, `Esc` to close); `@` opens the file picker (fuzzy over the
project's files, gitignore-aware; chosen files are attached); a line
starting with `!` runs in the shell and shows the output without a model
call; bracketed paste keeps newlines.

Slash commands: `/help`, `/model [name]` (picker without a name),
`/provider`, `/pricing [in=… out=… cache=… write=…]` (alone: `ShowModel`;
with values: `SetPricing`, prices per million tokens), `/permissions [mode]`,
`/yolo` (auto mode), `/prompt` (the system prompt as sent, shown as a card),
`/prompt edit` (the TUI steps aside, `$VISUAL`/`$EDITOR`/`nano`/`vi` opens
`~/.config/nexo/prompt.md`, created from the current prompt when missing,
and the TUI returns), `/goal <text>` (`SetGoal`; alone it shows the goal,
`off` clears it), `/compact`, `/clear`, `/resume` (picker; resumes in
place), `/rewind`, `/export [path]`, `/rename <name>` (the title the pickers show), `/diff`, `/cost`, `/context` (what the
next call will see), `/theme [name]` (picker with live preview; the choice is
written to `[ui] theme` in the user configuration), `/mouse` (capture on or
off), `/keys`, `/copy`, `/quit`. Unknown commands say so in a notice. The
command table (`commands::COMMANDS`) carries each command's aliases
(`/sessions` for `/resume`, `/back` for `/rewind`, `/mode` for
`/permissions`, `/system` for `/prompt`, `/q` for `/quit`, ...); the palette
ranks an exact prefix of a primary name first, then of an alias (shown dim
beside the name: `/resume  (sessions)`), then fuzzy matches, ties in the
table's order; Tab completes to the primary name.

Modals (centered, `Esc` closes): permission (summary, the detail scrollable,
buttons `[y] allow  [a] always  [n] deny`, the suggested rules listed with
`1..9` to adopt one); model picker; session picker; rewind picker; theme
picker (previews as you move, restores on Esc); help. A modal takes the keys
and the mouse until it closes.

Themes: forced colours. Every theme is a complete set of explicit RGB values,
background included, and the interface paints its own ground on every cell
(the transcript, the editor, the bars, the panel, the palette, the modals),
so nexo looks the same whatever the terminal's palette; on a terminal
without 24-bit colour (`COLORTERM` not `truecolor`/`24bit` and `TERM`
without `256color`, or `NEXO_COLORS=256`/`16`) each value is mapped to the
nearest of the 256-colour cube, or to the standard colour of the same hue on
16 colours, and `/theme` says so. `dark` (default: vivid pastels, every text
colour at WCAG AA 4.5:1 on every ground), `nexo`, `light`,
`catppuccin-mocha`, `gruvbox`, `nord`, `dracula`, `solarized-dark`,
`tokyo-night`; `mono` is the one theme that leaves the terminal's colours
alone (also under `NO_COLOR`). Code highlighting through syntect sets
foregrounds only, on the theme's code background.

A `demo` example (`cargo run -p nexo-tui --example demo`) drives the TUI
with a scripted agent (text, reasoning, tool calls with a diff and a long
shell output, a permission ask, a notice, usage, model limits, the prompt)
for visual checks without a model. Tests: markdown to lines, folding, search, command parsing,
palette ranking, selection geometry, the gauge, the theme preview, the
pricing parser, and `TestBackend` snapshots of the layout.

### nexo-cli

`nexo` (TUI in the current directory), `nexo -p "prompt"` (headless: prints
the reply; `--json` prints every AgentEvent as a JSON line), `--provider`,
`--model`, `--cwd`, `--resume <id>` / `--continue`, `--permission-mode`,
`--context plain|curator|compact`, `--yolo`; subcommands `bench` (run the
benchmark: `--upstream`, `--model`, `--key`, `--tasks`, `--arms`, `--repeats`,
`--max-steps`, `--budget`, `--out`; see the README), `providers` (list
presets and configured ones, which have a key), `models [provider]
[--refresh]` (every model known with its context window, largest reply,
prices and where they come from; `--refresh` fetches the catalogue again),
`pricing [provider/model] [--set in=.. out=.. cache=.. write=..]` (show or
set the prices of a model, per million tokens, in the user file), `prompt
[--edit]` (print the system prompt as it would be sent, or open the prompt
file in `$VISUAL`/`$EDITOR`, creating it from the built-in text), `sessions`,
`config path|init` (writes a commented starter file).
