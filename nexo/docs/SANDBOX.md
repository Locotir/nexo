# Sandbox for the bash tool

Deny rules are a guard against mistakes. The sandbox is the OS stopping the command itself.

## Use

```toml
[permissions]
sandbox = "workspace-write"      # off (default) | workspace-write | read-only
sandbox_network = false          # true lets sandboxed commands use the network
sandbox_write_dirs = ["/srv/data"]   # more writable directories (only those that exist)
```

`nexo --sandbox workspace-write` (or `NEXO_SANDBOX`) sets it for one run; `/sandbox [mode]` changes it in the
TUI (no argument: switch between off and workspace-write); `/status` shows the mode, the backend and how the
network is shut.

A project file (not yet trusted) can only tighten the sandbox: it cannot turn it off, open the network or add
writable directories. `nexo config trust` lifts that, as for the other settings.

## What a command may do

| | `workspace-write` | `read-only` |
| --- | --- | --- |
| read | the whole disk | the whole disk |
| write | project root, `sandbox_write_dirs`, temp dirs (`$TMPDIR`, `/tmp`, `/var/tmp`), `/dev`, the caches below | temp dirs and `/dev` |
| network | off unless `sandbox_network = true` | same |

Caches (under the home directory, skipped when missing): `.cargo/registry`, `.cargo/git`, `.npm`,
`.cache/pip`, `.cache/go-build`, `.cache/yarn`, `.cache/pnpm`, `.local/share/pnpm`, `.gradle`,
`.m2/repository`, `.nuget/packages`, `go/pkg`. `CARGO_TARGET_DIR` and the `registry` and `git` folders of
`CARGO_HOME` are added when set. The list is `CACHE_DIRS` in `crates/nexo-core/src/sandbox.rs`.

## Backends

- **Linux, Landlock** (kernel 5.13 or later with the LSM on): applied in the child just before `exec`
  with the `landlock` crate. Network off is a new user and network namespace (loopback is brought up inside,
  so a test can run its own server on 127.0.0.1; the host's listeners are out of reach). Where a namespace
  cannot be made, Landlock's TCP rules (kernel 6.7 or later) are used. If neither exists, `/status` says
  the network is not blocked.
- **Linux, bwrap**: used when the kernel has no Landlock and `bwrap` is on the PATH (read-only bind of `/`,
  the write roots bound on top, `--unshare-net`). `NEXO_SANDBOX_BACKEND=bwrap` (or `landlock`) forces one.
- **macOS, sandbox-exec** with a generated profile (writes denied except the roots, `network*` denied unless
  allowed). Written to the documented profile language; not run on a Mac in this repository's tests.
- **Windows**: none yet. The mode is accepted, a notice says it is not available, and every command asks
  before it runs, as without a sandbox.

If the sandbox is on and no backend works (Linux with neither Landlock nor bwrap), the bash tool refuses to
run and says why. It never runs the command unsandboxed on its own.

## Asking and failures

In the `ask` and `accept-edits` modes a bash call inside a working sandbox runs without a question (a deny
rule still wins; read-only and plan modes still refuse commands). When a command fails with a message the
sandbox can cause (permission denied, read-only file system, network unreachable, connection refused, name
resolution), the tool result ends with a short `[sandbox: ...]` note. The model can then call `bash` again
with `unsandboxed: true`; that call always asks, and the request says it runs outside the sandbox. No allow
rule and no remembered "always" answer covers it (the same holds for `detach: true`); only yolo, or a hook
of yours that answers allow, skips the question, and a deny rule still refuses it.

## What the sandbox does not stop

The sandbox keeps a command from writing outside the project and from using the network. It does not make
the command harmless.

- It reads the whole disk and the whole environment, API keys and `~/.ssh` included. What it reads can end
  up in the output the model sees and in the session file.
- It writes anything in the project, including paths that run later outside the sandbox or change what nexo
  may do: `.git/hooks`, `.git/config`, `.nexo`, `.mcp.json`, `.claude`, `.codex`, `.envrc`, `.vscode`,
  `.husky`. nexo does not block this. When a foreground command changes one of them (created, removed,
  or a new size or time; only `stat` is used, no file is read), the result ends with a line such as
  `[sandbox: this command changed .git/hooks/pre-commit, which runs outside the sandbox later]` and you get a
  warning. In a git worktree the hooks and config of the real git directory count too, where the sandbox can
  write them. A background job gets the same line in its output when it ends, but no warning.
  A command that runs for a while and also touches those paths may be a false alarm if you edited them yourself.
- Unix sockets on disk (for example the Docker socket) stay reachable even with the network off: Landlock does
  not govern `connect` to a path, and the macOS profile allows them. A command that can reach one can ask
  that program to act outside the sandbox. Do not use the sandbox as a boundary against a hostile command.

For more safety: turn the sandbox off for the project (`/sandbox off`, or `sandbox = "off"`), so every `bash`
call asks first; add deny rules for what must never run; keep keys out of the environment you start nexo from.

## Limits

- The sandbox covers the `bash` tool, its background jobs and what they start. The `!cmd` shell escape,
  MCP servers, hooks and language-server tools run as before.
- `read-only` keeps temp directories writable, so tools that need scratch space still start.
- `--add-dir` roots feed `sandbox_write_dirs` (`SandboxSettings.extra_dirs`) (with or without `-p`), and so does `/add-dir` mid-session, unless a managed policy fixes those folders.
