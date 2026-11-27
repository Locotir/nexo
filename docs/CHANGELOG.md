# Changelog

CI builds a release on every push to `main` and names it `build-<commit>` (the last five are kept).
`nexo --version` prints the same commit. The version in `nexo/Cargo.toml` is 0.2.0 since 2026-11-17
(0.1.0 before). Dates are commit dates; newest first.

## 2026-11-27

### Added
- `nexo --wire` records every request to the model as sent, with its answer, the curator's view and the window fit, and keeps a live report beside them; `nexo wire --open` opens it (see `nexo/docs/WIRE.md`).

### Fixed
- Choosing another provider (`--provider`, `NEXO_PROVIDER`, or a project `config.toml` that names one and no model) runs that provider's own default model, not the model set for the previous one. `--provider X --model M` is `X/M`, and `--model provider/model` switches the provider too. A `task` that names a provider and no model uses that provider's default.
- A curator view that passed the model's window by a little, with images in the history, was refused ("request over the window") with no way out: the curator now cuts it again, up to three times, before refusing.
- After the context policy failed, the whole history was sent with no fitting; the fitting curator now brings it under the window.

## 2026-11-17

### Added
- Ready-built releases for macOS (Apple silicon and Intel): `install.sh`, `nexo update` and the GitHub Action download them, checked against their SHA-256.
- Fixes for macOS: browsers are found as app bundles, Option+arrow jumps words, and the clipboard, notifications, sandbox paths and case-folding permission rules work there. macOS is not yet tested in CI.
- `nexo dbg` and the `debug` tool: run a program, break, step, watch, read variables from DWARF, hook a function with typed arguments and trace library calls, on Linux, Windows and macOS (Intel). System call tracing works on Linux only.
- The debugger also reads Linux core files and Windows minidumps, debugs 32-bit Linux programs, follows forked children, sets breakpoints in libraries loaded later, and calls a function in the live program.
- More debugger commands: launch options (env, cwd, aslr, argv0), `dump` to a file, frame selection with `up` and `down`, block coverage with named snapshots, and the gdb commands you already type.
- `nexo dbg` can connect to a gdb stub, and on Linux it can serve itself as one on 127.0.0.1 for other gdb tools.
- `nexo bin` reads more files: Mach-O, AArch64 (disassembly, functions, xrefs), 32-bit x86, WebAssembly, .NET, JVM classes, PDB symbols and separate debug files.
- On x86 programs, `nexo bin` can decompile to C, emulate a function, show control flow, slice data flow, and assemble bytes. It can also diff two builds, patch bytes and match library signatures.
- `nexo re --mcp` serves the reverse-engineering kit to other tools over MCP; without `--allow-changes` it is read-only.
- The `browser` tool and `/chrome`: nexo drives your own logged-in Chrome through an extension, or a session browser, reads pages as a tree, and records the page to an animated GIF.
- `nexo team` and the `/team` window: background routines that run on a clock, with `team add` and `team start --now`.
- `/panel` shows this session, its sub-agents, team routines and other sessions as one tree; `nexo sessions live` lists them as text or JSON.
- You can type a message into another live session on this machine and open a sub-agent's conversation from `/jobs`.
- Status bar: a cache miss chip, the cache share in `/context`, the active session, and how much of a subscription plan is left (also in `/usage`).
- Calm motion in the screen: the logo ignites when a session opens, a breathing line while the agent works, and a welcome galaxy.
- Windows open on a dimmed backdrop and all line up the same way; help can be filtered, and there is a settings window.
- `/goal` subcommands (pause, resume, clear, done) are understood by their first letters, and a goal pauses when its work changes nothing.
- Workflows: a file of phases and steps runs its sub-agents in order, reports each step, and can be stopped.
- `ultracode` agent plans can be approved and run, and past costs are shown.
- Shared memory between sessions, and `note` can replace or forget one saved rule.
- A plain click on a link in the transcript opens it.
- A session is titled by your own words, and every theme switch shows.
- A local provider you configured is scanned at its own address, so a local server with no key unlocks the home screen; providers named by a preset are scanned the same way.
- `grep` shows at most N matches per file and says how many it left, and searches files in parallel.
- Apache-2.0 license; every build ships LICENSE and the third-party notices.

### Changed
- Pressing Esc stops a request, tells the model so, and keeps what you typed meanwhile: messages queue and run as one request on the next turn, and show at once in the transcript.
- A model with a small window gets a shorter prompt, shorter tool text and a fitted request; MCP tool text and replies are held to what the window leaves.
- The curator keeps its state next to each session, so a resume replays only what came after, and it reads more kinds of text.
- The 1-hour prompt cache is written when your pauses make it pay, and every call carries about 640 fewer fixed tokens.
- Startup is faster: the first screen does not wait for MCP servers, and only what the session list shows is read.
- On a subscription, cost reads as API value, and the model picker lists every chat model a ready provider serves.
- A file the model wrote in full, unchanged since, is not attached again when you name it. A short line says so instead.
- A bash call outside the sandbox or detached always asks, and the permission dialog for an MCP call shows every argument.
- The default screen is quieter: a finished turn leaves time and cost, and the rest opens on demand.

### Fixed
- Bash permission check: `sed` with write or execute flags, `rg --pre` and similar are no longer treated as read-only.
- AltGr characters type correctly on Windows consoles.
- A shell command that ran to completion is never shown as failed.
- A reply cut at the reasoning budget is sent again with less thinking.
- A long draft stays in place, pastes go to the search bar, and the text above a streaming reply no longer slides down.
- The status bar keeps whole parts by priority, shows local time and fresh token numbers.
- Resume reports no replayed calls as new in `nexo -p --resume`.
- The Windows debugger no longer fails a wait that only timed out.

## 2026-11-10

### Added
- Benchmark pages: nexo against Claude Code and against Codex CLI on the same model, with the plan published first.
- Sessions remember their provider and model and come back on them.
- Status bar items answer a click; the curator label says by how many tokens it changed the window in use.
- Models with a window of 32k or less get a compact prompt and compact tool text.
- Claude 5.5 models: adaptive thinking, effort and real prices.

### Changed
- Tools and system text stay fixed for the whole session, so the prompt cache survives each new message.
- Steps that follow each other go in one reply, and a failed one stops the rest.
- After the check that proves the last change passes, the agent answers at once.
- `read` returns at most 64k characters and says how to read on.
- Subscription providers cap the window at what their backend allows.
- The install scripts pick the file for the current system, and CI runs them.
- The curator prices each decision by the view and what is new, not the whole history.
- The front page and the guide are shorter and match the code.

### Fixed
- First-run errors name the file, the key and the next step; the first screen answers `/help`.
- `web_search` stops retrying a refused search; `web_fetch` handles 404 and 410.
- A reply that only reasons past `agent.reasoning_budget` is cut and retried without doubling the output.
- `update` help says what it really does.

## 2026-11-09

### Added
- `/sent` shows the last request exactly as the model got it; your own message shows only what you typed.
- `note` saves global and project rules; `[compat] claude_code` ignores Claude Code files; `nexo prompt --tools`.
- Ctrl+click opens a link in the transcript; clicking the goal opens its window.
- Enter on an empty draft sends the queued request now.
- Whole-file receipts, one-line unchanged re-reads, data files checked by format, and a `$NEXO_TMP` folder for commands.
- Language-server diagnostics in `edit` and `write` results.
- The bench times out a stuck run, records why every run stopped, and adds long-session tasks.

### Changed
- The curator rewrites the view at epochs, indexes the history by key, and keeps a 20k to 80k working window.
- A model with a large window gets its largest reply from the first request.
- Edits that change nothing, identical writes and edits after an outside change are refused.
- README shows current numbers, the harness race and a new demo.

### Fixed
- Esc after an interrupting Esc no longer opens the rewind picker.
- The global config folder is never taken as a project; home skills are listed once.
- No project files are attached outside a project.

## 2026-11-08

### Added
- `/diff` viewer, `/add-dir`, `/memory`, `/note`, `/logout`.
- `nexo "request"` opens the session and sends it; `-i` attaches an image, and so does dropping one on the terminal.
- `edit` takes several changes to one file; `read` takes several files and Jupyter notebooks.
- `edit` and `write` show the lines around the change and report compiler errors in the edited file.
- `wait_until` polls for a condition instead of a fixed sleep.
- Oriented start: a project map in the prompt and files named in a request attached.
- `agent.close_on_verified` ends a turn after a passing check (off by default).
- Session panel and supervisor: mechanical watch, check-in per stopped turn, sturdier states.

### Changed
- Nine commands fold into their parents and stay typable as hidden aliases.
- The default theme is nexo, with dark second.
- `nexo bench` is a cargo feature (on by default).
- `nexo update` retries a busy release server and shows progress.
- Shorter tool descriptions.

### Fixed
- A failed update on Windows restores the old program.
- A config file that cannot be read is not overwritten by a remembered rule.
- Parallel tool calls numbered index 0 stay separate.
- Ctrl+U cuts to the start of the line; long diff and code rows end in an ellipsis; a pasted chip deletes as one unit.

## 2026-11-07

### Added
- Subscription usage in the status bar, `/usage` and `nexo usage`.
- `/fast` switches to a model's fast twin and back.
- Images are read by the model (PNG, JPEG, GIF, WebP, PPM, PGM, BMP); `grep` has a multiline option.
- `bash` can detach a background service; a "command not found" note appears when a script exits.
- Trust prompt before the first request in a folder with project hooks, servers or commands.

### Changed
- Fewer steps per task: todo and note ride with real work, one verification.
- The prompt asks for the soonest correct path and proof of done from command output.
- Picking a model of another provider switches the provider too.
- The OpenAI prompt cache key is sent by default only on OpenAI and a ChatGPT login.

### Fixed
- Bundled Mozilla roots are used when the system CA store is empty; TLS setup never panics.
- Claude Code's user hooks at the home folder are ignored.
- Commands that would wipe the machine are never run.

## 2026-11-06

### Added
- `/review`, `/commit`, `/pr` and `nexo review`.
- Agent Skills: `SKILL.md` discovery, a `skill` tool, `/skills`.
- Reads `CLAUDE.md`, `@imports`, `.claude` and `.codex` commands and agents.
- Headless contract: output and input formats, limits, tool filters, JSON schema, exit codes.
- MCP over HTTP with OAuth, `nexo mcp add/list/remove/login`, `.mcp.json`, a `/mcp` window.
- `nexo acp`, an Agent Client Protocol server for Zed, JetBrains and Neovim.
- GitHub Action and GitLab CI template for review and `@nexo` comments.
- Hooks with 11 events, JSON decisions and matchers.
- OS sandbox for `bash` (Landlock, bwrap, sandbox-exec) with `/sandbox` and `--sandbox`.
- Git worktree isolation: `nexo --worktree`, `nexo worktree`, isolated sub-agents.
- `nexo export` (HTML, Markdown, JSON with redaction) and a local audit log.
- Plugins with a marketplace, and a managed policy file administrators control.
- A first-run model setup with sign-in cards.
- A guess at the next message in the empty input, taken with Tab or Right.

### Changed
- The model setup groups models by provider and stays locked until a model works.
- Subscription models show the API value as included, not as spend.
- Releases keep only the last five builds.

### Fixed
- Windows paths drop the `\\?\` prefix so allow rules, worktrees and plugin sources match.
- A stale Codex CLI login is renewed at sign-in.
- Runs of `nexo -p` are left out of `/resume`.
- Rate limits are never learned as windows; the spend file is locked and backed up.

## 2026-11-05

### Added
- Ready-built releases for Linux and Windows (x86_64 and ARM) from every passing `main` commit.
- Install scripts, a daily update notice and `nexo update`.
- A goal loop with `/goal pause` and `/goal resume`.
- Mail between live sessions (`/msg`, `send_message`, `list_sessions`).
- A to-do window with progress, jobs window, inline word completion and clean box-aware copy.
- Folded groups of tool calls; the commit shows in `--version`, for example `0.1.0 (54e9541)`.
- Themes: Monokai colours and a grey layout; a colour for every cost.
- Polyglot benchmark (Aider's exercises) and DeepSeek results; `deepseek-v4-pro` with a 1M window.

### Changed
- The built-in prompt is rewritten and always comes first; `~/.nexo/prompt.md` adds your own after it.
- Background commands get a review that answers CONTINUE or STOP.
- The agent checkpoints at the tool-round limit instead of stopping.

### Fixed
- A call cut at `max_tokens` says how to fit the next one, and its arguments leave the history.
- A repeated failure for the same cause says so in one line.
- Rule paths with `..` are normalised; non-regular files are refused.
- Sub-agent rounds cut by the server are asked again.

## 2026-11-04

### Added
- First public version: a terminal coding agent for any model API, with a cache-aware context curator.
- CI on Linux and Windows; README, SECURITY and CONTRIBUTING.
- `nexo bench` with `--key -` (key from stdin), `--price` and `--budget`.
- Sub-agents on another model or provider, running at the same time.
- Heavy and noisy session benchmarks, with charts.
- Timestamps in the interface, queued message editing and a status line.

### Changed
- A small window caps the reply at a quarter of it; the view is fitted in the provider's own count.
- Shell cards name the shell that runs them.

### Fixed
- Session locks survive pid reuse, other hosts and filesystems without hard links; a paused holder is named.
- A reply of only reasoning no longer ends the request.
- Untrusted projects cannot lift spend limits; the auth store is under an OS lock.
- Runs that read outside their working directory are marked contaminated in the bench.
