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
with `unsandboxed: true`; that call goes through the normal permission prompt.

## Limits

- Unix sockets on disk (for example the Docker socket) are still reachable: Landlock does not govern
  `connect` to a path. Do not run the sandbox as a boundary against a hostile command that can reach one.
- The sandbox covers the `bash` tool, its background jobs and what they start. The `!cmd` shell escape,
  MCP servers, hooks and language-server tools run as before.
- `read-only` keeps temp directories writable, so tools that need scratch space still start.
- `--add-dir` roots feed `sandbox_write_dirs` (`SandboxSettings.extra_dirs`) once that flag exists.
