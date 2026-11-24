# Build space and worktree inspection (read-only)

Before considering cleanup, inspect explicitly selected build directories and **all** Git worktrees:

```sh
# Run from nexo/. Add only directories you actually want measured.
python3 -B tools/build-space-report.py --repo .. \
  --cache ./target \
  --cache /absolute/path/to/old-build-cache \
  --max-entries 200000 --seconds 10

python3 -B -m unittest discover -s tools -p test_build_space_report.py -v
```

The script uses Python's standard library and Git. It prints JSON and has no delete, prune, clean,
rename, checkout or write operation. `GIT_OPTIONAL_LOCKS=0` prevents optional Git index refresh writes;
Git invocations also disable `core.fsmonitor` so a configured fsmonitor hook is not executed. This is
not a sandbox against hostile Git configuration or concurrent filesystem changes.
It does not search the home directory or discover arbitrary caches. Worktrees come from Git's registry,
including detached, locked and non-Nexo worktrees. Status errors/timeouts are `unknown`, **not clean**.
Each checkout reports tracked/untracked changes separately from `ignored_entries`: Git's matching mode
counts an ignored directory once without enumerating its contents. An `ignored_entries` value of zero is
not evidence that ignored or unsaved work is absent from submodules or elsewhere. Commits not pushed
anywhere, running builds and unsaved editor buffers are not detected. A no-changes status is not
permission to remove a worktree. Paths are local diagnostic data; review before sharing the report.

Each explicit cache scan has its own entry/time budget. It skips symlinks and other devices; a partial
scan is marked incomplete and its size is only a lower bound. Missing/inaccessible paths also fail
closed. File logical bytes and allocated bytes (where the platform exposes `st_blocks`) count each
inode once **per scan**. Nested/overlapping arguments and hardlinks between scans may double-count;
space shared through external hardlinks is not necessarily reclaimable. The report flags explicit
cache/worktree path overlaps in either direction after resolving paths, for manual review. This warning
does not establish that other paths are safe or that their contents can be deleted. These are best-effort,
non-atomic filesystem observations, not a security boundary against concurrent path replacement.
`newest_observed_file_mtime` is evidence about file writes, not proof of last use or safe expiration.
Exit status is 1 for incomplete scans or Git errors and 0 for a complete report; changes alone are not
an error. Even exit 0 is not a cleanup authorization.

## Safe operating procedure

1. Check the filesystem's reported free space in the report; do not start another large build when
   space is constrained. Inventory only the named build locations, not the whole disk.
2. Review every worktree's status, branch, locked/prunable markers and ownership with the people/agents
   using it. Preserve uncommitted/untracked/ignored files and commits, not just source tracked by Git.
3. Identify active build processes before changing a cache. Do not run competing Cargo builds in the
   same target directory. Reusing a known compatible target can save space, but source snapshots can
   accidentally reuse artifacts; benchmark arms require verified distinct binaries/source provenance.
4. Treat old cache directories as **candidates for human review**, never automatic deletion targets.
   This tool intentionally does not generate deletion commands or claim that any directory is safe.
5. Choose any later cleanup separately after backups and owner confirmation. This task does not
   remove anything and supplies no automatic retention policy.

`nexo worktree list` only lists Nexo-created worktrees. `nexo worktree remove` is destructive and
`nexo worktree prune` is **not** a disk-space dry-run (see [WORKTREES.md](WORKTREES.md)). Do not use
those commands as substitutes for this report. Existing worktree lifecycle cleanup is outside this
tool's scope; this report's unknown/error handling does not change that implementation.
