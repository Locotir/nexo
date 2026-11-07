# Git worktrees

A worktree gives a session, or one sub-agent, its own checkout and its own branch. Parallel agents cannot
overwrite each other or your files, and you merge the result when you are happy with it.

## A session in a worktree

```
nexo --worktree            # a short random name
nexo -w fix-login          # your own name
nexo -w fix-login -p "make the login test pass"
```

nexo makes `<repo>/.nexo/worktrees/<name>` on a new branch `nexo/<name>` from the current HEAD and runs the
session there. It refuses outside a git repository, in a repository with no commit yet, and with a name that
is not letters, digits, `-`, `_` or `.`.

`.nexo/worktrees/` is added once to the repository's `.git/info/exclude` (not to `.gitignore`), so it is
never committed.

When the session ends:

- no changes and no new commits: the worktree and its branch are removed;
- anything else: they stay, and nexo prints the path, the branch and the commands to merge
  (`git merge nexo/<name>`) or throw away (`nexo worktree remove <name>`).

`/status` shows `worktree <name> on branch ...` when the session runs in one.

## The `nexo worktree` command

| Command | What it does |
| --- | --- |
| `nexo worktree list` | the worktrees nexo made here, clean or with changes |
| `nexo worktree remove <name>` | removes the worktree and its branch, changes included |
| `nexo worktree prune` | removes the clean ones and the leftovers of folders deleted by hand |

It works from the main checkout or from inside a worktree.

## Sub-agents

A custom sub-agent (`.nexo/agents/*.md`, `~/.nexo/agents/*.md`, or `.claude/agents/*.md`) can ask for a
worktree of its own in its front matter:

```
---
name: refactor
description: renames things across the project
isolation: worktree
---
Do the refactor. Commit nothing; say what you changed.
```

Each run of it gets a fresh worktree (`agent-<name>-<id>`), and its tools work there. The result the parent
reads ends with the branch, the path and a `git diff --stat`, or says that nothing changed and the worktree
was removed, with the same rule as for a session. Only agents that can change files get one; a read-only
agent ignores `isolation`.

## Setup in a new worktree

```toml
[agent]
worktree_setup = ["npm ci", "cp ../.env.local .env"]
```

The commands run in order in every new worktree (session or sub-agent); one that fails undoes the worktree.
A project that is not trusted (`nexo config trust`) cannot set `worktree_setup`, because it runs commands.
A worktree under a trusted repository counts as trusted too.

`.worktreeinclude` in the repository root lists, in `.gitignore` syntax, untracked files to copy into every new
worktree, usually secrets that git ignores:

```
.env
.env.local
config/*.key
```

Files the copy and the setup leave in the tree are not counted as changes when deciding whether to keep it.

## Left out

- A worktree is not removed if nexo is killed; `nexo worktree prune` cleans up the clean ones.
- `--worktree` does nothing with the other subcommands (`providers`, `config`, ...).
