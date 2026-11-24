#!/usr/bin/env python3
"""Read-only, bounded inventory of explicitly named build caches and Git worktrees."""
import argparse
import json
import math
import os
from pathlib import Path
import shutil
import stat
import subprocess
import time


def git(repo, *args):
    env = os.environ.copy()
    # Git hooks and callers can export repository-local overrides. Inheriting
    # them would make -C inspect a different checkout or even another index.
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
                "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                "GIT_NAMESPACE", "GIT_PREFIX"):
        env.pop(key, None)
    env["GIT_OPTIONAL_LOCKS"] = "0"
    result = subprocess.run(
        # A configured fsmonitor hook is executable code and can modify files;
        # disable it even when configured in the inspected repository.
        ["git", "-c", "core.fsmonitor=false", "-C", os.fspath(repo), *args], capture_output=True,
        timeout=15, check=False, env=env,
    )
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors="replace").strip())
    return result.stdout


def worktrees(repo):
    """Include all registered worktrees, not only those created by Nexo."""
    raw = git(repo, "worktree", "list", "--porcelain", "-z")
    records = []
    for block in raw.split(b"\0\0"):
        fields = {}
        for field in block.split(b"\0"):
            key, _, value = field.partition(b" ")
            fields[os.fsdecode(key)] = os.fsdecode(value)
        if "worktree" not in fields:
            continue
        row = {"path": fields["worktree"], "branch": fields.get("branch"),
               "detached": "detached" in fields, "bare": "bare" in fields,
               "locked": fields.get("locked"), "prunable": fields.get("prunable"),
               "ignored_entries": None}
        if row["bare"]:
            row["status"] = "bare (no checkout)"
        else:
            try:
                status = git(row["path"], "status", "--porcelain=v1", "-z",
                             "--untracked-files=all", "--ignored=matching",
                             "--ignore-submodules=none")
                changes = False
                ignored = 0
                records_iter = iter(status.split(b"\0"))
                for record in records_iter:
                    if not record:
                        continue
                    if record.startswith(b"!! "):
                        # Matching mode reports ignored directories without enumerating
                        # their contents. Count entries, not files or reclaimable data.
                        ignored += 1
                    else:
                        changes = True
                        if b"R" in record[:2] or b"C" in record[:2]:
                            # In porcelain -z a rename/copy has a second, raw pathname;
                            # it may itself start with "!! " and is not a status record.
                            next(records_iter, None)
                row["ignored_entries"] = ignored
                row["status"] = "changes present" if changes else "no tracked/untracked changes"
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
                row["status"] = "unknown"
                row["error"] = str(exc)
        records.append(row)
    return records


def inventory(path, max_entries=200000, seconds=10):
    """Skip observed symlinks and other devices; concurrent changes can race the scan."""
    path = Path(os.path.abspath(os.path.expanduser(os.fspath(path))))
    row = {"path": str(path), "entries": 0, "logical_bytes": 0,
           "allocated_bytes": 0, "complete": True, "notes": []}
    try:
        root = path.lstat()
    except OSError as exc:
        row.update(complete=False, notes=[str(exc)])
        return row
    if not stat.S_ISDIR(root.st_mode):
        row.update(complete=False, notes=["not a directory (symlinks are not followed)"])
        return row
    try:
        row["disk_free_bytes"] = shutil.disk_usage(path).free
    except OSError as exc:
        row.update(complete=False, notes=[str(exc)])
        return row
    seen = set()
    stack = [path]
    deadline = time.monotonic() + seconds
    newest = None
    try:
        while stack:
            # Pending directories can be empty: checking only yielded entries
            # would let traversal continue after the deadline and report success.
            if time.monotonic() >= deadline:
                row["complete"] = False
                row["notes"].append("scan budget reached; sizes are lower bounds")
                return row
            with os.scandir(stack.pop()) as entries:
                for entry in entries:
                    if row["entries"] >= max_entries or time.monotonic() >= deadline:
                        row["complete"] = False
                        row["notes"].append("scan budget reached; sizes are lower bounds")
                        return row
                    row["entries"] += 1
                    info = entry.stat(follow_symlinks=False)
                    if stat.S_ISLNK(info.st_mode):
                        row["complete"] = False
                        if "symlink skipped" not in row["notes"]:
                            row["notes"].append("symlink skipped")
                        continue
                    if info.st_dev != root.st_dev:
                        row["complete"] = False
                        row["notes"].append("different device skipped")
                        continue
                    if stat.S_ISDIR(info.st_mode):
                        stack.append(entry.path)
                    elif stat.S_ISREG(info.st_mode):
                        key = (info.st_dev, info.st_ino)
                        if key in seen:
                            continue
                        seen.add(key)
                        row["logical_bytes"] += info.st_size
                        row["allocated_bytes"] += getattr(info, "st_blocks", 0) * 512
                        newest = info.st_mtime if newest is None else max(newest, info.st_mtime)
                    else:
                        row["complete"] = False
                        if "special file skipped" not in row["notes"]:
                            row["notes"].append("special file skipped")
    except OSError as exc:
        row["complete"] = False
        row["notes"].append(str(exc))
    finally:
        row["newest_observed_file_mtime"] = newest
    return row


def cache_worktree_overlaps(caches, trees):
    """Flag path overlap for manual review; this is not a cleanup safety check."""
    overlaps = []
    for cache in caches:
        cache_path = Path(cache["path"]).resolve()
        for tree in trees:
            tree_path = Path(tree["path"]).resolve()
            if cache_path == tree_path or cache_path in tree_path.parents or tree_path in cache_path.parents:
                overlaps.append({"cache": cache["path"], "worktree": tree["path"]})
    return overlaps


def cache_overlaps(caches):
    """Report duplicate or nested scan paths; independently measured sizes overlap."""
    overlaps = []
    for index, first in enumerate(caches):
        first_path = Path(first["path"]).resolve()
        for second in caches[index + 1:]:
            second_path = Path(second["path"]).resolve()
            if first_path == second_path or first_path in second_path.parents or second_path in first_path.parents:
                overlaps.append({"first": first["path"], "second": second["path"]})
    return overlaps


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--cache", type=Path, action="append", default=[],
                        help="explicit directory to measure; repeatable, no automatic cache search")
    parser.add_argument("--max-entries", type=int, default=200000)
    parser.add_argument("--seconds", type=float, default=10)
    args = parser.parse_args()
    if args.max_entries < 1 or not math.isfinite(args.seconds) or args.seconds <= 0:
        parser.error("scan budgets must be positive and finite")
    report = {"read_only": True, "warnings": [
        "Nothing is deleted. Sizes do not imply reclaimable space; hard links may exist outside each scan.",
        "Worktree status separates tracked/untracked changes from ignored entries; neither proves commits are backed up or a worktree is idle.",
        "Ignored entries may contain unsaved work; directories count once without listing ignored files or measuring their contents.",
        "File age does not prove a cache is unused. Stop and identify build processes before manual cleanup.",
        "Allocated size is platform-dependent (zero if st_blocks is unavailable); scans are not atomic, and concurrent path replacement may change what is traversed.",
    ], "caches": [inventory(p, args.max_entries, args.seconds) for p in args.cache]}
    report["cache_overlaps"] = cache_overlaps(report["caches"])
    if report["cache_overlaps"]:
        report["warnings"].append(
            "Cache paths overlap: their measured sizes must not be added together."
        )
    try:
        report["worktrees"] = worktrees(args.repo)
        report["cache_worktree_overlaps"] = cache_worktree_overlaps(report["caches"], report["worktrees"])
        if report["cache_worktree_overlaps"]:
            report["warnings"].append(
                "Cache/worktree paths overlap: review these paths carefully; they may contain unsaved work."
            )
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        report["worktrees_error"] = str(exc)
    print(json.dumps(report, indent=2, ensure_ascii=True))
    return int("worktrees_error" in report or any(not r["complete"] for r in report["caches"])
               or any(r["status"] == "unknown" for r in report.get("worktrees", [])))


if __name__ == "__main__":
    raise SystemExit(main())
