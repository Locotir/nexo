# nexo

A terminal coding harness for any model provider: a full-screen interface
(mouse, folding transcript, search, palette) and a headless mode, with tools
(read, edit, write, shell, search), permissions, sessions, MCP servers, and
a built-in context curator as its default context policy.
Written in Rust (ratatui), for Linux and Windows (both built and tested on every push; macOS is not tested).

![nexo with DeepSeek: a to-do list, the cause of a failing test, the fix and the test run](docs/demo.gif)

Everything it does, and the test that checks each thing: [FEATURES.md](FEATURES.md).

## Install

**Linux** (terminal):

```sh
curl -fsSL https://raw.githubusercontent.com/Locotir/nexo/main/install.sh | sh
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1 | iex
```

Both download the ready-built nexo from the [latest release](https://github.com/Locotir/nexo/releases/latest)
(Linux and Windows, x86_64 and ARM; checked against its SHA-256 list) and put it in `~/.local/bin` on Linux
or `%LOCALAPPDATA%\Programs\nexo` on Windows, adding that folder to PATH. No Rust, no compiler, a few seconds.
Where no ready-built file fits, or with `NEXO_BUILD=1`, they install what the build needs without asking
(Rust, a C compiler or the Visual Studio C++ build tools; on Windows only the Windows prompt to allow) and
build from source. `NEXO_INSTALL_DIR` picks another folder. A copy left in cargo's own folder by older
installs is removed, unless `NEXO_INSTALL_DIR` is set.

Each time the interface starts, nexo asks GitHub in the background whether the latest release is newer than
the commit it was built from and says so; `nexo update` downloads it and replaces nexo where it is (it builds
from source when no release fits), and `nexo update --check` only says. `[ui] update_check = false` or
`NEXO_NO_UPDATE_CHECK=1` turns the check off; a build from changes never pushed is not checked.

From a clone:

```sh
cd nexo
cargo build --release          # target/release/nexo
install -m 755 target/release/nexo ~/.local/bin/
```

**Windows.** The same build (`cargo build --release` in PowerShell gives `target\release\nexo.exe`), or from
Linux `cargo build --release --target x86_64-pc-windows-gnu` with mingw-w64 installed. Settings and sessions
live in `%USERPROFILE%\.nexo` (`config.toml`, `sessions\`). Commands run in Git Bash when Git for Windows is
installed (the model writes POSIX shell, and `C:/…` paths work there), else in PowerShell, and the `bash`
tool tells the model which; `/status` shows the one in use. Keys typed in `/model` go to
`%USERPROFILE%\.nexo\keys\<name>` (`api_key_file`). Windows Terminal shows the colours, the mouse and the
notification at the end of a long turn. Every push is built and tested on Linux and Windows.

## Start

```sh
export DEEPSEEK_API_KEY=...     # or OPENAI_API_KEY, GEMINI_API_KEY, XAI_API_KEY, GROQ_API_KEY, ...
cd your/project
nexo                            # picks the first provider whose key is present
nexo --provider openai --model gpt-5
nexo -p "what does this repository do?"            # headless: prints the reply
nexo -p "run the tests and fix what fails" --permission-mode auto --json
nexo --continue                 # resume the latest session of this directory
nexo --yolo                     # never ask for permission (also /yolo inside)
nexo prompt                     # print the system prompt as it would be sent from here
printf %s "$KEY" | nexo bench --upstream URL --model M --key - --out bench-out   # measure the context policies
```

Local servers need no key: `nexo --provider ollama --model qwen3:8b`.
With a subscription instead of a key: `nexo auth login copilot` (or `codex`
for ChatGPT Plus/Pro: it reuses the Codex CLI login if there is one, else
opens the browser; `/model` offers the same), then `nexo use copilot`. `nexo doctor` checks the
setup, and `nexo guide` prints the step-by-step guide
([`docs/SETUP-FOR-AGENTS.md`](docs/SETUP-FOR-AGENTS.md)), which an assistant
can follow to set nexo up for you.

## In the interface

The keys sit on the top line; the **status bar** is the bottom line, right under the draft. It shows the model, the permission mode (coloured: `auto` in red), the git branch, a thin line gauge (`━━━╸━━━━ 42%`) for context that turns yellow, orange and red as the last call approaches the model's window, and the **memory** in tokens (`mem 1.2M`: the whole history kept, in k, M or B). The gauge is what each call sends, so the gap between the two is the saving of the context policy. On the right, `Σ 6.5k billed` adds up the tokens of every call so far: each call re-sends its context, so it grows much faster than the memory, and it is what the cost next to it is paid on. `81% cached` is the share of input the cache served (green from 50%), `12 calls` the model calls so far, and `⏱ 14s` how long the running turn has taken. `/context` prints all of them.


| Keys | What |
|---|---|
| `Enter` / `Ctrl+J` | send / newline |
| `Enter` on an empty box while a request waits in the queue | interrupt the turn and run the queued request now |
| `↑` on an empty box while requests wait in the queue | take the last queued request back into the box to edit it (click a queued row for any other) |
| `Esc` | interrupt the turn; again: clear the draft |
| `Ctrl+C` ×2, `Ctrl+D` (empty draft) | quit |
| wheel, `PgUp`/`PgDn`, `Home`/`End` | scroll the transcript (it glides there) |
| scrollbar | drag the thumb, or click the track to jump; it lights up under the pointer, in every window and picker too. While you read further up, `↓ N new` (click it) and the thumb pulse when more arrives |
| click on a block | select it; a click on the first line of the selected block folds or opens it (a drag still selects text) |
| `Shift+↑` or `Tab` (empty draft) | select the last block: a tool call with its result, thinking, a reply or message of several lines |
| `↑`/`↓` (a block selected) | the previous / next block, the view following it; `↓` past the last goes back to the draft |
| `→` / `←` / `Enter` (a block selected) | open / fold / toggle it; `Esc` or typing lets it go |
| `Ctrl+O` | fold or open every tool group, tool card and thinking block |
| `Ctrl+T` | show or hide the model's reasoning |
| `Ctrl+P` | interpreted view: a markdown, JSON, CSV or TSV block (and a `.md`/`.json`/`.csv` file a tool read or wrote) shows rendered instead of as source; again: the source |
| `Shift+Tab` (no block selected) | permission mode: ask → accept-edits → plan → ask |
| `Ctrl+F`, `n`/`N` | search the transcript |
| `Ctrl+Y` | copy the last reply |
| `Ctrl+V` | paste: an image in the clipboard (a screenshot) is kept as a PNG and attached to the next request; text is pasted as text |
| `Ctrl+G` | write the draft in `$EDITOR`; it comes back to the box |
| `↑`/`↓` (no block selected), `Ctrl+R` | the requests sent before (kept between runs in `~/.nexo/prompt-history.jsonl`); `Ctrl+R` searches them |
| drag with the mouse | select text; copied when the button comes up (`Shift`+drag: the terminal's own selection; `/mouse` turns capture off) |
| `Esc` `Esc` (empty draft, idle) | rewind to before an earlier message and edit it; the files the agent wrote since are put back |
| `Ctrl+B` | the side panel: goal, todo list, changed files |
| `/`, `Ctrl+Space`, `Alt+P` | command palette (`/model`, `/effort`, `/undo`, `/init`, `/status`, `/tools`, `/pricing`, `/permissions`, `/plan`, `/yolo`, `/prompt`, `/prompt edit`, `/goal`, `/compact`, `/clear`, `/resume`, `/rewind`, `/shared-memory`, `/todo`, `/queue`, `/undelete`, `/export`, `/rename`, `/diff`, `/cost`, `/context`, `/theme`, `/mouse`, `/timestamps`, `/help`); every command answers to its aliases too (`/sessions` is `/resume`, `/back` is `/rewind`, `/q` is `/quit`), and typing either puts the command first in the list |
| `@` | attach a file (fuzzy picker over the project); an image (`png`, `jpg`, `gif`, `webp`, up to 5 MB) goes as an image for models that see |
| `!cmd` | run a shell command without a model call |

While the agent works, the line above the editor says what it does (`⠴ Editing… 12s · ↓ 1.3k tok · Esc interrupts`),
with a light sweeping across the word; the reply grows behind a blinking caret. A turn that took 15 s or more
rings the terminal's bell when it ends, with a desktop notification where the terminal takes one (OSC 9/777):
`[ui] notify_after = 0` turns it off.

The status bar answers clicks: the model opens the model picker, the gauge `/context`, the cost `/usage`. On the keys row, `todo d/t` opens the todo list (`/todo`), `⚙ N jobs` the jobs window with their output (`/jobs`), `⊕ mem` the shared memory that waits (`/shared-memory list`), `◷ N queued` the queued messages (`/queue`; pick one to edit it) and the session name the sessions (`/resume`); each is underlined under the pointer, and `Esc` closes the list.

In the `/usage` window the **Usage** tab ends with "What's contributing to your usage?". nexo reads the sessions saved on this machine in the background and ranks what cost the most as a percentage, each row with one change under it: calls that carried over 150k tokens of context, cache misses, the tools whose output was re-sent most, spend by model and by project, and (for sessions recorded by this version) the history the curator left out. A table lists the biggest sessions by cost and cache hit, each named by its folder and the day it was last used. `d`, `w` and `m` pick today, the last 7 days or the last 30; switching never reads again. Shares overlap, so they do not add up to 100%, and the figures cover only the sessions on this machine.

Scrolling eases out, new blocks fade in, dialogs and the palette pop open and a glow runs along the line
under a working agent; `[ui] animations = false` turns all motion off (everything lands at once).
Messages and replies carry a dim `HH:MM` at the right of their first line, and a reply its duration once it ends;
`[ui] timestamps = false` turns it off, `/timestamps` toggles it for the session.

A run of two or more tool calls (thinking in between counts) is one line while it is folded: `Made 1 edit +29 −3, read 2 files, ran 3 shell commands · 1 failed  4.2s ▸`. It reads in the present tense with a live clock while a call runs (the running call stays in view) and folds by itself once all have finished. Select it with the arrows or click it, then Enter or → opens it; `Ctrl+O` opens or folds all of them. `[ui] group_tools = false` shows every call on its own line again.

While you type, the rest of a word seen anywhere in the session (your messages, replies, tool arguments and output) shows in grey after the cursor: words in any language, names, paths (and each part of them), flags, URLs, hashes. `→` at the end of the word takes it; with a block picked or the command list open the arrows keep their meaning. Secrets (values after key/token/password, `sk-` style keys) are never offered. `[ui] complete_words = false` turns it off.

**Spend limits.** `[agent] max_cost = 1.0` stops a session at a dollar; `max_cost_day = 5.0` stops every
session together for the day (what was spent each day is in `~/.nexo/spend.json`). Past 80% nexo
warns once; at the limit the request stops before the model is called, so a limit is passed by one call at
most. The status bar shows the spending against the limit (`$0.4200/1.00`, orange near it, red at it).
`/budget` says what this session and today cost; `/budget 2` sets this session's limit, `/budget off` lifts it.

```toml
[agent]
max_cost = 1.0       # dollars per session
max_cost_day = 5.0   # dollars per day, all sessions
```

**Commands of your own.** A markdown file in `~/.nexo/commands/` or in the project's `.nexo/commands/`
(once the project is trusted: `nexo config trust`) is a command named after it: `.nexo/commands/review.md` is `/review`. Its text is sent as the request, with
`$ARGUMENTS` replaced by what follows the name (appended when there is no `$ARGUMENTS`). The palette lists them
after the built-in ones, with the `description:` of a front matter block or the file's first line:

```markdown
---
description: review the change against our rules
---
Review $ARGUMENTS against CONTRIBUTING.md and list what breaks a rule.

**Agent Skills.** A folder with a `SKILL.md` (front matter `name`, `description`, optional `allowed-tools`)
and any files it mentions is a skill, the same format Claude Code and Codex use. nexo looks in
`.nexo/skills`, `.agents/skills` and `.claude/skills` of the project (the working directory up to the
repository root), then in `~/.nexo/skills`, `~/.agents/skills` and `~/.claude/skills`; on a name clash the
first wins, nexo's own folders first. The model only sees each skill's name and a short description (up to
100, in the `skill` tool's description, fixed for the session so the prompt cache holds) and loads the body
with the `skill` tool when a task matches; the files a skill mentions are read with `read`, relative to its
folder. You can run one yourself with `/skill-name [args]`; `/skills` lists what was found and where from.
`allowed-tools` is shown but not enforced: the permission rules still decide. Project skills are read like
`AGENTS.md`, without the trust step that commands and agents need.
```

**Sub-agents.** The model has a `task` tool: it hands a self-contained job to a sub-agent that works on
its own and answers with a short report. Only the report enters the conversation, so a wide search costs
one message. Two kinds come built in:

| Kind | Can do | For |
|---|---|---|
| `explore` (default) | read, grep, glob, ls, web search; nothing outside the project | "where is X handled?", "what calls Y?" |
| `general` | every tool but `task`; writes and commands ask the same permission as the main agent | a self-contained change or check |

Several `task` calls in one message run at once, also `general` ones that change files: a file one of
them is changing is closed to the others until it ends, and the one turned away says so in its report
(`[agent] parallel_tasks = false` runs those that can change files one after another). A sub-agent's spending is added to the session's. Your own kinds go in
`~/.nexo/agents/<name>.md` or `.nexo/agents/<name>.md` (the project's wins over a built-in of the
same name, and is read once the project is trusted):

```markdown
---
description: reviews a diff for bugs
tools: [read, grep, glob]
model: deepseek-chat
rounds: 20
---
Read the change and list what is wrong with it, worst first.
```

`[agent] subagents = false` removes the tool.

**The briefing.** A sub-agent does not start blind. Before it runs, nexo (not the model) picks from the
conversation that sends it what bears on its task and puts it ahead of the task, at most
`[agent] subagent_brief` tokens (3000; `0` turns it off):

- what nexo knows for a fact: the person's latest request, rules they gave earlier ("never touch
  Cargo.lock"), the files read, written and edited (those the task names first; a read marked `current`
  when its lines still match the file on disk), the commands still failing with their error lines, and
  the todo plan;
- excerpts: the passages of tool outputs and of the conversation that hold the task's words, a long
  output cut to the lines around them, reads that a later write made stale left out. With
  `subagent_brief_engine = "auto"` (the default, also `"curator"`) the curator's selection picks them,
  run inside nexo: it weighs each turn by how likely the task draws on it (its
  thread model) and each paragraph by the task's rare words and names it adds; `"lexical"` uses a
  plainer selection of the same kind, by word overlap alone. The same conversation and task give the
  same briefing. On the conversations and tasks of `crates/nexo-agent/tests/fixtures/brief/` nexo
  picks the same spans, with the same labels and texts, as an earlier, unpublished implementation of the
  selection, whose output is recorded in those fixtures (`the_selection_matches_the_reference`).

The sub-agent is told not to read again what is marked current, and its report comes back without the
lines it copied from the briefing and cut at about 2000 tokens. Where the sub-agent works, a line says
what it was handed: `briefed: 2.1k tok, 6 spans, 5 facts · curator`. `nexo brief SESSION|FILE TASK…`
prints the briefing a task would get from a saved session (or a JSON file of messages), with
`--tokens`, `--engine`, `--json`.

What it buys, measured. A replay of a recorded session of 26 sub-agents (764 tool
calls, 0.74M tokens of tool output) and builds each briefing from the conversation that started it:

| | the whole parent conversation | briefing, curator | briefing, lexical | last 3000 tokens |
|---|---|---|---|---|
| files the sub-agent read that it names | 53% | 25% | 24% | 15% |
| same, handed over half-way through the work | 63% | 40% | 30% | 14% |
| tool calls whose output it already holds (80% of lines) | 2.0% | 0 | 0 | 0 |

Size 3.1k tokens on average; about 0.1 s for a 300k-token parent (median, curator or lexical). In that session
the main agent wrote long, self-contained task prompts and its sub-agents read new files, so the briefing
mostly tells them where to look; the excerpts pay off when the main agent read or ran something itself and
then hands over a short task (`a_sub_agent_starts_with_a_briefing_of_what_the_session_read`). The cost is
the briefing's tokens once, then read from the cache on every later call of the sub-agent (about
3.1k × (1.25 + 0.1 × calls) input tokens): it pays for itself when it spares two or three calls.

**Background work.** `bash` with `background: true` starts a command (a server, a watcher, a long build) and
returns at once with its job id; `task` with `background: true` does the same for a sub-agent that changes
nothing (one that can change files runs in the foreground, so it never edits beside another). The model goes
on with its work and uses the `jobs` tool to list jobs, read new output (optionally waiting for it) or kill
one. When a job ends, the model is told once, and if it was idle it wakes up to look. `/jobs` opens a window with them and the end of their output,
`/jobs kill [id]` stops one (or all), and the status line shows `⚙ N jobs` while any runs. In a `-p` run,
jobs still running when the model finishes are stopped.

**Language servers.** After an edit the agent can ask the file's language server what is wrong with it:
the `diagnostics` tool starts rust-analyzer, pyright (or pylsp), typescript-language-server, gopls or clangd,
whichever is installed, in the project's root, keeps it for the session and returns the errors and warnings
with their lines. rust-analyzer is started with build scripts, proc macros and the check on save off, so a
diagnostics call does not run the project's code; read-only sub-agents do not get the tool.

**Hooks.** Shell commands at points of the loop, each given the event as JSON on stdin and `NEXO_EVENT`,
`NEXO_TOOL`, `NEXO_CWD` in its environment. A `pre_tool` hook that exits with 2 blocks the call, and what
it printed is the reason the model reads; `post_tool` and `turn_end` hooks are told what happened.

```toml
[[hooks]]
event = "pre_tool"
matcher = "bash"                       # a tool, `edit|write`, `web_*`; none: every tool
command = "grep -q 'git push' && { echo 'no pushes from here' >&2; exit 2; } || exit 0"

[[hooks]]
event = "post_tool"
matcher = "edit|write"
command = "cargo fmt >/dev/null 2>&1 || true"
```

**Vim keys.** `/vim` (or `[ui] vim = true`) gives the editor vim's modes: `Esc` to normal mode
(`h j k l w b e 0 $ gg G x X D C dd cc u p`, `i a A I o O` back to typing, `Enter` sends), the mode shown
by the mark (`›` typing, `▪` normal) and in the hint bar.

**Undo.** Before the agent writes a file, nexo keeps what it looked like. `/undo` takes back the last request:
its messages go, the files it wrote are put back (a file it created is removed), and the request returns to the
editor to be changed and sent again. A rewind to an earlier message (`/rewind`, `Esc Esc`) does the same for
everything after it. Changes made by shell commands are not known, so they stay. `/init` has the agent study the
project and write `AGENTS.md`; `/status` says what nexo runs with (model, endpoint, where the key comes from,
context policy, session file, configuration files) and `/tools` lists the tools by kind.

**Git commands.** nexo gathers the git facts first (status, diff, stat, log), so the model starts with them:
`/review [base|commit|--staged]` reviews the uncommitted changes plus the branch since its merge base with
main or master, and lists findings ranked by severity with `file:line` (it edits nothing); `/commit [hint]`
commits what is staged, or everything after saying so, with a message in the style of the last ten subjects and
never a trailer or co-author line; `/pr [base]` pushes the branch if needed and runs `gh pr create` with the
title and body taken from the commits (without `gh`, or without `gh auth login`, it says so and stops). Commit,
push and PR go through the normal permission prompts. A diff over 48 000 characters is cut by file, keeping the
biggest hunks, and the cut is marked. For CI, `nexo review [--base main] [--commit SHA] [--staged] [--json]`
prints the findings (read-only mode) and exits 1 on a high-severity finding, 3 when the reply is not parseable,
2 when there is nothing to review.

**Sessions.** In the `/resume` list, `Del` marks a session and a second `Del` moves it to the trash (a folder
inside the sessions folder; nothing is erased): `/undelete` brings back the last one, `/undelete <id>` any of
them, and `nexo sessions delete|restore <id>...` does the same from the shell. `/shared-memory` shares what another
session knows with this one: pick it (or `/shared-memory <id> [focus]`) and its notes (`Finding:`, `Plan:`,
`Decision:` lines), its constraints, files and failing commands, and the excerpts that bear on the focus (its
goal or title by default), about 4k tokens at most, go with your next message, once. Nothing earlier is
rewritten, so the cached prefix holds. `/shared-memory off` drops what waits.

Before a write or a command the permission dialog shows the diff or the
command: `y` once, `a` always (a rule is remembered), `n` deny. Rules look
like `bash(git *)` or `edit(src/**)`. `~` and links are resolved before a path rule is checked: a deny rule
sees both the path as written and where its links lead, and an allow rule must match both, so
`edit(src/**)` does not reach a link in `src/` that points out of the project. `accept-edits` writes
without asking only inside the project, never in `.git`, `.nexo` or shell start-up files.
An allow rule for `bash` covers a command only when it is plainly that command: a redirection to a
file, `$'..'`, `${..@..}`, a substitution or an option that runs code (`git -c`, `find -exec`,
`xargs`) is asked about. Deny rules for `bash` are a guard against mistakes, not a sandbox.

Reading and searching are not asked about, except for files that hold keys or credentials: `.env` and
`.env.*`, anything under `.ssh`, `.gnupg`, `.aws`, `.kube` or `.docker`, private keys and certificates
(`id_rsa*`, `*.pem`, `*.key`, ...), `.netrc` and its kind, and nexo's own credential store. Those are
asked about in every mode but `auto`; only an allow rule that names their path (`read(.env)`, not `read`)
skips the question. Sub-agents keep the same rules, deny rules included. A `grep` over a folder
passes over those files and says how many it left out; naming one as its `path` searches it (and asks).
`diagnostics` reads by starting the project's language server or compiler, which run its build
scripts and macros: it is asked about like a command (an allow rule `diagnostics` or `auto` skips the
question) and never runs in `plan` or `read-only`.

An empty session opens with a banner: the nexo mark beside the version, the
model and permission mode, the directory and branch, and one line of hints. The status bar carries the git branch and a
context gauge (`━━━╸━━━━ 42% 54k/128k`: what the last call sent over the model's
window); `/pricing` shows what is known of the model (prices, context window,
largest reply) and `/pricing in=0.28 out=0.42 cache=0.028` sets its prices per
million tokens and remembers them. `/prompt edit` opens your preferences in
`$EDITOR` (`~/.nexo/prompt.md`, added after the built-in prompt); they apply
from the next request.

### Todo and goals

For work with several steps the model keeps a task list with the `todo`
tool (`[ ]`, `[>]`, `[x]`); the interface shows it in the panel (`Ctrl+B`)
and it comes back on resume. With the `note` tool it keeps what it learnt
(`Finding:`, `Plan:`, `Rule:` lines): the call changes nothing, the context
policy keeps the lines with the first message once their message leaves. `/goal <text>` pins a goal and starts working on
it; while a turn runs it is pinned and checked when the turn ends. The model first
restates it as you mean it (what, why, how, when it is done) and works from that,
so a goal typed in a hurry still gets a clear finish line. When a reply ends
without the line `GOAL DONE` the agent continues by itself, up to five rounds
(`NEXO_GOAL_ROUNDS`). It stops and tells you why when it needs you (a question
waits for your answer, and your answer resumes it), when you press Esc, when a
turn fails or when a round does nothing new; `/goal resume` goes on, `/goal
pause` holds it, `/goal off` clears it. A question you send while it works is
answered before the work goes on. The curator takes care of what the model *remembers*; the
list and the goal take care of what it is *aiming at*.

### Plan mode and questions

In plan mode (`Shift+Tab`, `/plan [text]`, `--permission-mode plan`) the
model reads, searches and thinks but changes nothing: writes and commands are
refused. When it knows what to do it shows the plan, rendered as markdown,
with three choices: **yes, auto-accept edits**, **yes, approve each edit**, or
**no, keep planning**; the last row takes words of one's own ("tell the model
what to change") that go back to it. `/plan off` leaves plan mode. In a run
nobody watches (`-p`) the model takes the open choices itself and the plan
goes ahead with its edits allowed.

When a choice is the person's, the model asks with `ask_user`: a list of
options, picked with the arrows and `Enter`, a digit, or a click; with
several allowed, `Space` ticks them; the last row takes an answer of one's
own; `Esc` dismisses. In a run nobody watches the tool is not offered.

### Code

Code blocks, files and diffs are coloured for every language `bat` knows
(Rust, C, C++, C#, Go, Java, Kotlin, Swift, Ruby, PHP, JavaScript,
TypeScript, JSX, Bash, PowerShell, SQL, HTML, CSS, YAML, TOML, Dockerfile,
Makefile, Nix, Zig, Haskell, Elixir, Lua and many more), by name, alias, file
extension or first line; a shell command in a `bash` card is coloured too.

### Themes

The colours are forced: every theme is a complete set of RGB values,
background included, and nexo paints its own ground, so it looks the same in
any terminal whatever palette the terminal has configured (a terminal
without 24-bit colour gets the nearest of the 256-colour cube, and `/theme`
says so). `/theme` opens a picker that previews each theme as you move and
remembers the choice (`[ui] theme` in the user configuration). `dark` is the
default (neutral, the classic terminal colours fixed). The others: `nexo` (a
deep blue-grey ground, a teal accent, a warm marker for the assistant),
`light`, `mono` (no colour at all; also when `NO_COLOR` is set),
`catppuccin-mocha`, `gruvbox`, `nord`, `dracula`, `solarized-dark`,
`tokyo-night`. Code highlighting follows the theme.

## Configuration

`~/.nexo/config.toml` (user) and `.nexo/config.toml` (project; wins key by key:
a project's `[context] reader_at` leaves your `curator_spec` and `policy` alone, a price it sets for one
model leaves your others, and a key it sets to its default value still wins).
A key nexo does not read (a typo such as `curator_specs`, a section that does not exist, a setting since
retired) does not stop it: the session opens with a notice naming the key and the closest one it reads,
and `nexo doctor` lists it. Your own `~/.nexo` is never taken for a project, even when your home is kept in git.

**Untrusted projects.** A repository you clone may carry its own `.nexo/config.toml`. Until you trust the
project, the settings that could run a command, send a key, a file or the conversation elsewhere, or lift
a spend limit are ignored, and the session opens with a notice naming them: a provider's `base_url`,
`api_key_*`, `auth`, `login`, `headers`, `protocol`, `extra` and `pricing` (your own for that provider
stay), a `provider` your file does not define, `[mcp.*]`, `[[hooks]]`, `permissions.allow`,
`permissions.mode` set to `auto` or `accept-edits`, `ui.status_command`, `agent.system_prompt_file`,
`web.url` and `web.api_key_env`, and an `agent.max_cost` or `max_cost_day` above yours or not a positive
number (0, a negative number or `nan` would mean no limit). Everything else in it applies, and its `deny`
rules always do.
The project's `.nexo/commands/` and `.nexo/agents/` are not loaded either until you trust it.
`nexo config trust [DIR]` trusts the project (its resolved folder is added to `~/.nexo/trusted-projects`,
one per line; remove the line to take it back) and `nexo config path` says whether it is trusted.
An `a` (always) answer saved to the project file applies in later sessions once the project is trusted.

Everything of your own lives in that one folder, `~/.nexo` (`%USERPROFILE%\.nexo`
on Windows): `config.toml`, `prompt.md`, `commands/`, `agents/`, `sessions/`,
`keys/`, `spend.json` and `cache/`. Settings from the old places
(`~/.config/nexo`, `~/.local/share/nexo`, `~/.cache/nexo`) move in by
themselves the first time, without overwriting anything already there.
`nexo config init` writes a commented starter. Keys never go in these files:
a provider names the environment variable (or a command) that holds its key.

**As root** (`sudo nexo`, `su -`, a root shell) nexo uses root's own home:
`/root/.nexo`, and `~` means `/root`, even when `sudo` kept the caller's `$HOME`. So root never reads a
user's settings nor leaves root-owned files in their home. `NEXO_CONFIG_DIR`,
`NEXO_DATA_DIR` and `NEXO_CACHE_DIR` still win when set. The status bar shows
`# root` in red for as long as nexo runs that way.

```toml
provider = "deepseek"
model = "deepseek-chat"

[providers.myserver]                 # any OpenAI-compatible server
protocol = "openai-chat"
base_url = "http://localhost:8000/v1"
api_key_env = "MYSERVER_API_KEY"
models = ["my-model"]

[permissions]
mode = "ask"                         # ask | accept-edits | plan | auto | read-only
allow = ["read", "grep", "glob", "bash(git status*)"]

[context]
policy = "auto"                      # auto | plain | curator | compact

[mcp.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
# trust_read_only = true             # believe the server's readOnlyHint (off: every tool is asked about)
```

An MCP server gets only these variables of your environment, plus its own `env`: `PATH`, `HOME`, `USER`,
`LANG`, `LC_*`, `TERM`, `TMPDIR`, `TMP`, `TEMP`, `SHELL`, `XDG_*`, and on Windows `SystemRoot`,
`SystemDrive`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `PATHEXT`, `COMSPEC`, `WINDIR`, `ProgramData`
and `ProgramFiles`; a key it needs goes in its `env`.

### Providers

Four wire protocols cover every provider ([`docs/providers-base.md`](../docs/providers-base.md) is the
specification): `openai-chat`, `openai-responses`, `anthropic`, `gemini`.
Presets exist for openai, anthropic, gemini, deepseek, xai, groq, mistral, openrouter,
together, fireworks, cerebras, moonshot, qwen, zhipu, minimax, copilot, codex,
and the local servers ollama, lmstudio, vllm and llamacpp. Beyond them, a
built-in directory of 200+ providers works with only the key set.

| Command | What it does |
|---|---|
| `nexo auth login copilot` | log in with a subscription (also `codex` for ChatGPT Plus/Pro, `xai`) |
| `nexo auth login deepseek --key` | keep a key in nexo's credential store: asked for without showing it (`--key -` reads stdin) |
| `nexo use deepseek [model]` | make it the default |
| `nexo doctor` | check the setup; every `FIX` line says what to run |
| `nexo providers [--all] [--json]` | the providers and where each key comes from |
| `nexo models deepseek [--json]` | models with window, largest reply and prices |
| `nexo guide` | the full setup guide, written so an assistant can follow it |

The guide is [`docs/SETUP-FOR-AGENTS.md`](docs/SETUP-FOR-AGENTS.md): hand it to
any assistant and it can set nexo up for you without writing a key to a file.
A new vendor is a `[providers.name]` block; a new protocol is one adapter in
`crates/nexo-providers`.

### Setting up a model: `/model`

`/model` lists the models already configured (Enter switches to one) and every
provider to set up, with the local servers found running on this machine
(Ollama on 11434, LM Studio on 1234, vLLM on 8000, llama.cpp on 8080) marked
`● running` and moved up. Setting one up is five steps, each checked by asking
the server itself rather than trusting what was typed:

| Step | What nexo does |
|---|---|
| 1 provider | a preset, a configured provider, or any other address |
| 2 address | asks it what it is: Ollama `/api/version`, llama.cpp `/props`, LM Studio `/api/v0/models`, vLLM and any OpenAI-compatible API `/v1/models`, Anthropic and Gemini their model lists. Says what answered (`✓ Ollama 0.12.6 · 3 models`) or why nothing did |
| 3 key | only when the server answers 401/403. Paste the key (kept in `~/.nexo/keys/<name>`, readable only by you; the config points at it with `api_key_cmd`), or `$VARIABLE`, or `!command`. A refused key is said so |
| 4 model | the server's own list, with window, prices and whether it reasons when the server says; a name not listed can be typed |
| 5 check and save | the facts, each with where it came from, and **one real request** that asks the model to call a test tool: it says how long it took, whether tools work and whether it reasons |

The context window is the one the server actually serves with when it says so:
Ollama's `/api/ps` (the window the model is loaded with, often far below what
it was trained for, which `/api/show` gives), llama.cpp's `n_ctx`, vLLM's
`max_model_len`; then the models.dev catalogue and the presets fill the rest.
Local servers cost 0 per token. Every value can be changed before `Enter`
saves it (to `~/.nexo/config.toml`, the rest of the file untouched) and
switches to the model.

**Effort.** The levels a model takes are worked out from the protocol and the
server and confirmed by the test request: OpenAI's `reasoning_effort`
(`minimal` on GPT-5), Anthropic's and Gemini's thinking budgets (Gemini Pro
cannot turn it off), Ollama's `off`/`on` (`low`/`medium`/`high` on gpt-oss),
none for models whose reasoning is fixed (DeepSeek's reasoner) or when the
server refuses the parameter. `/effort` picks among them (`/effort high`,
`/effort default`); the choice is kept per model and shown next to it in the
status bar (`◆ high`).

### Prices and limits

The cost in the status bar is right when the prices are. For each model nexo
knows the price per million tokens of input, output and cached input (and
cache writes where the vendor charges them), the context window and the
largest reply. They come, in this order, from your configuration, the
preset, the open [models.dev](https://models.dev) catalogue (fetched once,
cached for a week in `~/.nexo/cache/models.json`, refreshed in the
background; `nexo models --refresh` forces it) and the vendor's own model
list. To state them yourself:

```toml
[providers.deepseek.pricing."deepseek-chat"]   # dollars per million tokens
input = 0.28
output = 0.42
cache_read = 0.028
cache_write = 0                                # 0: cache writes cost the input price

[providers.deepseek.context_window]
deepseek-chat = 131072
[providers.deepseek.max_output]
deepseek-chat = 8192
```

or `nexo pricing deepseek/deepseek-chat --set in=0.28 out=0.42 cache=0.028`
(and `/pricing in=0.28 out=0.42 cache=0.028` inside the interface), which
writes the same block to your user file without touching the rest of it.
`nexo pricing` with no values shows what is known and where it comes from.

The values also correct themselves from the providers' own error messages:
when a request is refused because it is longer than the window, the number
in the message (`maximum context length is 131072 tokens`, `exceeds the
context window of this model (256k)`...) is saved as the model's
`context_window` and the history is compacted; when `max_tokens` is above
what the model allows, the limit named is saved as `max_output` and the
request is retried with it; an unknown model name gets a notice with the
closest names. Each correction shows a `model` card with `from error`.

### The web

Two built-in tools reach the web. `web_search` searches and answers with
titles, URLs and snippets; `web_fetch` reads one page and gives it back as
Markdown (title first, scripts, styles, menus and forms dropped, links made
absolute; text, Markdown and JSON as they are; PDFs and images are refused).
Long pages come in pieces of 20 000 characters: the output ends with the
`start` of the next piece.

| `[web] search` | Key | Notes |
|---|---|---|
| `auto` (default) | | The first backend whose key is present, else SearXNG when `url` is set, else DuckDuckGo. A keyed backend that fails falls back to DuckDuckGo, and the output says so. |
| `duckduckgo` | none | Works out of the box, but it rate-limits and sometimes answers with a bot check. |
| `tavily` | `TAVILY_API_KEY` | Made for agents: clean snippets, a free tier. |
| `brave` | `BRAVE_API_KEY` or `BRAVE_SEARCH_API_KEY` | Its own index, a free tier. |
| `exa` | `EXA_API_KEY` | Semantic search. |
| `searxng` | none | Your own instance: set `url`. |

For heavy use, a Tavily or Brave key, or a SearXNG instance, avoids
DuckDuckGo's limits.

```toml
[web]
search = "auto"                         # auto | duckduckgo | tavily | brave | exa | searxng
# url = "https://searx.example.org"     # SearXNG, or another address for the chosen backend
# api_key_env = "MY_SEARCH_KEY"         # another variable for the chosen backend's key
# user_agent = "..."                    # sent by both tools
# enabled = false                       # no web tools at all
```

**Permissions.** `web_search` is a read: it runs without asking, also in
`read-only` mode. `web_fetch` asks, showing the whole URL, unless a rule
allows it. A rule names hosts: `web_fetch(docs.rs)`, and
`web_fetch(*.github.com)` covers `github.com` and its subdomains. The dialog
offers the rule for the page's host. A redirect from a public address to this
machine or a private one (`localhost`, `127.0.0.1`, `10.*`, `192.168.*`,
`169.254.*`, `[::1]`, ...) is refused, by name and by the address reached.
A name that resolves to such an address is refused before anything connects to it; to read a
local service, write its address (`http://127.0.0.1:8080/`) or `localhost`.
Every redirect is checked by the rules as if its target had been asked for: a
deny rule refuses it, and another site no rule allows is not read on the way
(the reply gives its address, and fetching it is asked about as usual).

### Your own system prompt

`nexo prompt` prints the prompt as it would be sent from the directory;
The prompt has two layers. The built-in one always comes first: how to work,
the tools, subagents. Then come the directory, date, tool guidance and project
instructions (`NEXO.md`, `AGENTS.md`), and last your preferences:
`nexo prompt --edit` (or `/prompt edit` inside) opens `~/.nexo/prompt.md`
in your editor (who you are, your machine and stack, how you like answers;
`<!-- comments -->` are not sent). A file that starts like the built-in text
(`You are nexo,`) replaces it instead, and one that starts with the line
`<!-- nexo: raw -->` is sent exactly as written. `[agent] system_prompt_file` points
at another file; `system_prompt_extra` appends text. Edits apply from the
next request.

### The curator

`policy = "curator"` runs the context curator and sends the model its curated
view instead of the whole history. The curator is built into nexo (the
`nexo-curator` crate), runs in process and needs nothing else installed; each
call costs a few milliseconds and only the messages that changed are
converted. `auto`, the default, is the curator, everywhere; a setting in
`curator_spec` the curator cannot run is left out (the rest still applies),
and a warning names it.

By default the view is pure: the model gets no order the whole history would
not give it. With `directives=1` the curator also adds its efficiency
directives at the end of the system prompt (batch independent calls, read
only what is needed, check once, close in three lines, do not cut test
output, write `Finding:` and `Plan:` lines for the curator to keep); they are
the same on every request, so they stay in the cache, and a raw prompt file
(`<!-- nexo: raw -->`) is left without them.
`/prompt` and `nexo prompt` show them.

```toml
[context]
policy = "curator"                   # auto | plain | compact | curator
# curator_spec = "eoq=15000;ledger=10000"   # change the engine's settings
```

`--curator-spec SPEC` (or `NEXO_CURATOR_SPEC`) sets them for one run, over the file.

The status bar says what the policy is doing, next to the gauge:

| Label | Meaning |
|---|---|
| `curator −62%` | this call sends 62% fewer tokens than the whole history |
| `curator idle` | the whole history still costs less to send than to rewrite: a rewrite pays the provider's prompt cache again, so the curator steps in as the history grows (in recorded sessions, from 30k to 70k tokens, around the 6th to 20th call) |
| `+3 in` | this call put 3 messages in the view: new ones, and old ones brought back |
| `-12 out` | this call took 12 messages out of the view, or cut them (they stay in the history and can be asked for) |
| `curator off` | the curator stopped; every call sends the whole history (the reason is in `/context`) |
| `plain` | every call sends the whole history; when `auto` chose it, a warning says why |

Each tool card in the transcript also says, at the right of its first line, what the last model call
did with that card's output:

| Mark | Meaning | What it costs |
|---|---|---|
| `● cached` | sent whole, inside the part of the request that did not change since the call before, so the provider serves it from its prompt cache | about a tenth of the normal price |
| `● full price` | sent whole, after the point where the request changed, so the provider reads it again | the normal input price |
| `◐ trimmed` | sent shortened: the curator kept the lines that matter (errors, results, the first and last lines) and left a note saying what was cut | less than whole |
| `○ set aside` | not sent this time; the model did not see it in this call. It is still in the history, with its name, and nothing is lost | nothing |
| `↺ recalled` | it had been set aside and came back, whole or in part, because the model asked for it (`recall`) or the work turned back to it | as `full price` for what came back |

A card with no mark was not part of the last call's decision (for example it is still running).

On `● cached` and `● full price` the card adds what that output cost in the last request (`0.00150 $`: its tokens at the cached or the full input price), when the model's input, output and cached prices are all known and the line has room for it; with a price missing, or on a narrow terminal, only the mark shows.

`/context` and `/status` add the sentence behind the label. Every message the curator leaves out keeps a name (`m00042`), and the model can bring it back exactly with the `recall` tool, which nexo answers from the whole history it keeps: leaving a message out never loses it. The view never passes the model's window: when the history is too big for it, the oldest messages leave first, and a single output too big on its own keeps its first and last lines with a mark saying which lines were left out. Every fallback is
said, never silent: a `curator_spec` setting the curator cannot run or an
unknown policy name shows a warning in the transcript and in the log. A history
that stopped between a tool call and its result (a session closed or killed
mid-step) is mended before it is sent: the call is answered as not run, and
nothing it holds is lost.

Replayed offline on the 7 whole-history recordings in
`crates/nexo-curator/tests/fixtures/` (`examples/replay.rs`), the curator sends
26% fewer tokens in total, from −49% to +7% per session
([`docs/results.md`](../docs/results.md), "Measured in nexo"). On recordings
that are not in this repository: a scripted 37-call session (reading and searching this
repository's modules, an OpenAI-compatible endpoint): plain sends 2.78M tokens,
the curator 0.90M (−68%); with cached input at a tenth of the price, −42% of
the input cost. Replaying 28 recorded sessions of 8 to 52 calls: −42% of the
tokens sent; the 9 sessions
that ended with 18k–80k tokens of history were never cut. The default engine is not made more eager: in the cache-aware
simulation, rewriting earlier saves less money and adds recovery calls.
`curator_spec = "eoq=15000;ledger=10000"` traded a little saving for fewer
calls (−6.4% of cost against −6.7%, 517 calls against 553 on 503 recorded steps);
those figures were measured with the previous default.

The default spec is `D_SPEC` in the crate; [`docs/how-it-works.md`](../docs/how-it-works.md)
shows it whole, with what each setting does. A spec is `key=value` pairs
separated by `;`, laid over it.
Besides the ledger, the anchor keeps three blocks of the messages that left the
view, each within its budget in tokens (0 turns it off):

- `rules=` **the rules block**: the person's standing instructions, whole
  (lines with never, always, don't, from now on, nunca, siempre, prohibido,
  recuerda, a partir de ahora …) and the lines the model restates as
  `Rule: …`. They are kept for the whole session and never dropped: when the
  anchor is short of room the ledger shrinks instead, so a correction typed
  once in the chat holds natively, without a system prompt or a file;
- `notes=` the model's own `Finding:` and `Plan:` lines, in its text or in a
  `note` call, so settled facts are not worked out again (oldest out first).
  After `nudge=N` steps without a note (off by default, `nudge=0`) the curator
  appends one request for the notes to the end of the last tool result (a
  `note` call when that tool is offered, lines of the reply when not), never
  inside the cached prefix, at most once per `nudge` steps, not between two
  parts of one file until twice that count, and only the latest request stays
  in view, until a note answers it; the stat is `nudges`;
- `pin=` the last full read of the files the request names and the last full
  write of the files the model wrote (1,500 tokens each at most, head and tail
  kept), sharing the budget newest first, scratch files (`_`, `tmp`) dropped
  first; a later edit marks a written file "re-read before trusting", a large
  edit or a failed write drops it, a new write replaces it.

The ledger takes what they leave. Nothing is written to disk: the blocks live
in the anchor of the curated view.

`/context-live` shows, beside the transcript, a map of the whole history with
what each call sends (whole, cut, left out) and the cells that just changed
flashing. With the curator it is the curator's whole memory in zones: the
anchor, the topic in progress (`●`, sent), and the other topics newest first,
each marked by what the call takes of it (`◐` parts brought back, `▭` only a
card, `○` nothing), so you see, of all it holds, what it is taking. With `plain` everything is sent; with `compact` the older half is summarised when
the history passes `compact_at` tokens.

### Benchmark

`nexo bench` runs agent tasks against a real model under several context
policies and scores each run with the task's own checker:

```sh
printf %s "$DEEPSEEK_API_KEY" | nexo bench --upstream https://api.deepseek.com/anthropic --model deepseek-chat \
  --key - --tasks realm --arms plain,compact,curator \
  --repeats 1 --max-steps 1000 --budget 5 --out bench-out
```

An arm is `[NAME@]plain|compact|curator[:k=v;k=v]`: `lean@curator:eoq=15000`
is the curator with that spec, reported as `lean`. Each run's transcript goes
to `--out`, with `results.jsonl` (one line per run) and `summary.md` (score,
steps, requests, tokens, cost, tool errors and repeated calls per arm). The
`realm` task asks the agent to rebuild a game's coin rule into a `coins.rs`
from experiments on a binary; the small tasks are Rust mini-projects with a
trap long runs tend to fall into. `--budget` caps the spending, in dollars; for a
model nexo has no prices for, give them with `--price MISS,HIT,OUT` (dollars per
million tokens), or the bench refuses to start.
External tasks `repoqa` and `canonstep` (with `--llmb DIR`) and `nexo bench report` (report.html): see `nexo bench --help`.

#### Aider Polyglot

`--tasks polyglot` runs [Aider's Polyglot benchmark](https://github.com/Aider-AI/polyglot-benchmark):
225 hard Exercism exercises in C++, Go, Java, JavaScript, Python and Rust.
The agent gets the instructions and the stub, works with its tools, then the
hidden tests run; a failure gets one more try with the test output, as Aider
does (pass@1, pass@2). The tests and every command of the agent run in a
sealed box without network; the run is hermetic (no `NEXO.md`, `AGENTS.md` or
personal prompt) and at temperature 0 unless `--temperature`.

```sh
printf %s "$DEEPSEEK_API_KEY" | nexo bench --upstream https://api.deepseek.com/anthropic --model deepseek-chat \
  --key - --tasks polyglot --arms plain --max-steps 40 --budget 3 --out polyglot-out
```

DeepSeek `deepseek-chat`, 2026-11-06, one run each, `plain` arm, measured
before nexo left the calls of a reply cut at max_tokens out of its history
(8 runs hit DeepSeek's 8192-token reply cap; their retries had looped):

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/polyglot-deepseek-dark.svg">
  <img alt="Aider Polyglot, 225 exercises: nexo with deepseek-chat passes 49 % on the first try and 75 % after one retry (.49); DeepSeek V3.2-Exp in Aider, from the public leaderboard: Chat 39 % and 70 % (-e.88), Reasoner 40 % and 74 % (.30). Per language for nexo: C++ 62 then 96 %, Rust 70 then 77 %, Python 44 then 74 %, JavaScript 49 then 71 %, Java 23 then 70 %, Go 59 then 69 %" src="docs/polyglot-deepseek.svg" width="760">
</picture>

pass@1 is the share solved on the first try; pass@2 counts those solved after
one retry that sees the output of the failing tests.

| Language | Exercises | pass@1 | pass@2 | Cost |
|---|---:|---:|---:|---:|
| C++ | 26 | 61.5 % | 96.2 % | $0.18 |
| Go | 39 | 59.0 % | 69.2 % | $0.21 |
| Java | 47 | 23.4 % | 70.2 % | $0.36 |
| JavaScript | 49 | 49.0 % | 71.4 % | $0.29 |
| Python | 34 | 44.1 % | 73.5 % | $0.16 |
| Rust | 30 | 70.0 % | 76.7 % | $0.28 |
| **All** | **225** | **48.9 %** | **74.7 %** | **$1.49** |

Aider's leaderboard has DeepSeek V3.2-Exp chat at 70.2 % (pass@2, 0.88 $).
The numbers are not like for like: nexo is an agent that reads, runs and edits
with tools in a sealed box, Aider sends whole-file or diff edits without running
anything, and `deepseek-chat` is whatever DeepSeek serves on the day. The
other differences are listed in each run's `summary.md`.

On 16 of these exercises (two or three per language), `deepseek-reasoner` passed 12
at pass@2 and 8 at pass@1, where three runs of `deepseek-chat` (one with the curator) passed 8, 9 and 10
(5, 5 and 4 at pass@1), for 2.6 times the cost ($0.25 against $0.10). A sample
that small says the gap is real but not how large it is.

DeepSeek's model list now names `deepseek-flash` (V4.1 Flash) and
`deepseek-v4-pro`, with a 1M-token window and replies up to 384K tokens;
`deepseek-chat` is the older name, and its replies still stop at 8192 tokens.
nexo's DeepSeek preset starts on `deepseek-flash`. One exercise is not a
measure: on Go's forth, the one where `deepseek-chat` failed after two cut
replies, `deepseek-flash` passed at pass@1 in 10 steps for $0.0095.

## Layout of the code

See `ARCHITECTURE.md`: the crates, one rule (`core ← providers, tools, agent,
tui ← cli`), the public API each crate commits to, the session file format
and the interface's specification.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo run -p nexo-tui --example demo      # the interface with a scripted agent, no key needed
```
