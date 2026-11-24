"""Offline tests: only temporary fixtures are changed; real caches are never touched."""
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("build-space-report.py")
spec = importlib.util.spec_from_file_location("build_space_report", SCRIPT)
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class InventoryTests(unittest.TestCase):
    def test_newest_mtime_handles_epoch_zero_and_pre_epoch_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first, second = root / "epoch", root / "older"
            first.write_bytes(b"first")
            second.write_bytes(b"second")
            os.utime(first, (0, 0))
            os.utime(second, (-10, -10))
            if first.stat().st_mtime != 0 or second.stat().st_mtime != -10:
                self.skipTest("filesystem cannot represent pre-epoch times")
            # Control enumeration order: zero followed by an older timestamp.
            with os.scandir(root) as entries:
                ordered = sorted(entries, key=lambda entry: entry.name != "epoch")
            scan = mock.MagicMock()
            scan.__enter__.return_value = iter(ordered)
            with mock.patch.object(report.os, "scandir", return_value=scan):
                data = report.inventory(root)
            self.assertEqual(data["newest_observed_file_mtime"], 0)

    def test_cli_warns_for_duplicate_nested_and_aliased_caches(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / "repo"
            repo.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            cache = root / "cache"
            cache.mkdir()
            nested = cache / "nested"
            nested.mkdir()
            artifact = nested / "artifact"
            artifact.write_bytes(b"unsaved")
            unrelated = root / "cache-other"
            unrelated.mkdir()
            alias = root / "alias"
            try:
                alias.symlink_to(cache, target_is_directory=True)
            except OSError:
                self.skipTest("symlinks unavailable")
            paths = (cache, nested, cache, alias / "nested", unrelated)
            args = [sys.executable, "-B", str(SCRIPT), "--repo", str(repo)]
            for path in paths:
                args.extend(("--cache", str(path)))
            result = subprocess.run(args, capture_output=True, check=True)
            data = json.loads(result.stdout)
            pairs = {(p["first"], p["second"]) for p in data["cache_overlaps"]}
            self.assertEqual(len(pairs), 5)
            self.assertIn((str(cache), str(nested)), pairs)
            self.assertIn((str(nested), str(alias / "nested")), pairs)
            self.assertFalse(any(str(unrelated) in pair for pair in pairs))
            self.assertTrue(any("must not be added" in w for w in data["warnings"]))
            self.assertEqual(artifact.read_bytes(), b"unsaved")

    def test_cache_worktree_overlaps_in_both_directions_without_prefix_false_positives(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = root / "repo"
            tree.mkdir()
            (tree / "target").mkdir()
            trees = [{"path": str(tree)}]
            caches = [{"path": str(p)} for p in
                      (tree, tree / "target", root, root / "repo-other")]
            overlaps = report.cache_worktree_overlaps(caches, trees)
            self.assertEqual([row["cache"] for row in overlaps],
                             [str(tree), str(tree / "target"), str(root)])

    def test_cache_worktree_overlap_resolves_symlinked_parent(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = root / "repo"
            tree.mkdir()
            link = root / "alias"
            try:
                link.symlink_to(tree, target_is_directory=True)
            except OSError:
                self.skipTest("symlinks unavailable")
            caches = [{"path": str(link / "target")}]
            self.assertEqual(report.cache_worktree_overlaps(caches, [{"path": str(tree)}]),
                             [{"cache": str(link / "target"), "worktree": str(tree)}])

    def test_cli_reports_overlap_and_preserves_unsaved_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True, capture_output=True)
            target = root / "target"
            target.mkdir()
            unsaved = target / "work.txt"
            unsaved.write_text("unsaved")
            result = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(root),
                                     "--cache", str(target)], capture_output=True, check=True)
            data = json.loads(result.stdout)
            self.assertEqual(data["cache_worktree_overlaps"],
                             [{"cache": str(target), "worktree": str(root)}])
            self.assertTrue(any("paths overlap" in warning for warning in data["warnings"]))
            self.assertEqual(unsaved.read_text(), "unsaved")

    def test_counts_hardlinks_once_and_preserves_contents(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "build.bin"
            source.write_bytes(b"abc")
            try:
                os.link(source, root / "duplicate.bin")
            except OSError:
                self.skipTest("hardlinks unavailable")
            result = report.inventory(root)
            self.assertTrue(result["complete"])
            self.assertEqual(result["logical_bytes"], 3)
            self.assertEqual(source.read_bytes(), b"abc")
            self.assertEqual(sorted(p.name for p in root.iterdir()), ["build.bin", "duplicate.bin"])

    def test_does_not_follow_external_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cache = root / "cache"
            cache.mkdir()
            outside = root / "outside"
            outside.mkdir()
            (outside / "secret").write_bytes(b"do not count")
            try:
                (cache / "link").symlink_to(outside, target_is_directory=True)
            except OSError:
                self.skipTest("symlinks unavailable")
            result = report.inventory(cache)
            self.assertFalse(result["complete"])
            self.assertEqual(result["logical_bytes"], 0)
            self.assertFalse(report.inventory(cache / "link")["complete"])

    @unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes unavailable")
    def test_special_file_is_reported_as_incomplete_without_opening_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fifo = root / "pipe"
            os.mkfifo(fifo)
            result = report.inventory(root)
            self.assertFalse(result["complete"])
            self.assertIn("special file skipped", result["notes"])
            self.assertEqual(result["entries"], 1)
            self.assertEqual(result["logical_bytes"], 0)
            self.assertTrue(fifo.exists())

    def test_budget_and_missing_directory_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ("a", "b", "c"):
                (root / name).write_bytes(b"x")
            result = report.inventory(root, max_entries=1)
            self.assertFalse(result["complete"])
            self.assertEqual(result["entries"], 1)
            self.assertFalse(report.inventory(root / "missing")["complete"])

    def test_deadline_is_checked_before_opening_pending_empty_directories(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "empty").mkdir()
            clock = [0.0]
            scanned = []
            real_scandir = os.scandir

            @contextmanager
            def scan(path):
                scanned.append(Path(path))
                with real_scandir(path) as entries:
                    yield entries
                # Enumerating the parent exhausted the time budget. The pending
                # child is empty, so an entry-only deadline check cannot see it.
                clock[0] = 11.0

            with mock.patch.object(report.time, "monotonic", side_effect=lambda: clock[0]), \
                    mock.patch.object(report.os, "scandir", side_effect=scan):
                result = report.inventory(root, seconds=10)

            self.assertFalse(result["complete"])
            self.assertEqual(result["entries"], 1)
            self.assertEqual(scanned, [root])
            self.assertIn("scan budget reached; sizes are lower bounds", result["notes"])
            self.assertIsNone(result["newest_observed_file_mtime"])

    def test_cli_rejects_nonfinite_scan_budgets_before_inventory(self):
        for seconds in ("nan", "inf", "-inf"):
            with self.subTest(seconds=seconds):
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), f"--seconds={seconds}"],
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertIn(b"scan budgets must be positive and finite", result.stderr)

    def test_cli_returns_nonzero_for_partial_scan(self):
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run([sys.executable, str(SCRIPT), "--repo", tmp,
                                     "--cache", str(Path(tmp) / "missing")], capture_output=True)
            self.assertEqual(result.returncode, 1)
            parsed = json.loads(result.stdout)
            self.assertTrue(parsed["read_only"])
            self.assertIn("worktrees_error", parsed)


class WorktreeTests(unittest.TestCase):
    def git(self, root, *args):
        result = subprocess.run(["git", "-C", str(root), *args], capture_output=True, check=True)
        return result.stdout

    @unittest.skipIf(os.name == "nt", "fixture uses a POSIX shell hook")
    def test_inventory_does_not_execute_configured_fsmonitor_hook(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            root.mkdir()
            self.git(root, "init", "-q")
            (root / "tracked").write_text("original")
            self.git(root, "add", "tracked")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "initial")
            (root / "tracked").write_text("unsaved")
            marker = Path(tmp) / "hook-ran"
            hook = Path(tmp) / "monitor"
            hook.write_text('#!/bin/sh\nprintf ran > "$1"\nexit 1\n'.replace('$1', str(marker)))
            hook.chmod(0o755)
            self.git(root, "config", "core.fsmonitor", str(hook))
            # Verify this Git actually invokes the configured hook for status.
            self.git(root, "status", "--porcelain=v1")
            self.assertTrue(marker.exists())
            marker.unlink()

            rows = report.worktrees(root)

            self.assertFalse(marker.exists())
            self.assertEqual(rows[0]["status"], "changes present")
            self.assertEqual((root / "tracked").read_text(), "unsaved")

    def test_inherited_git_environment_cannot_redirect_inventory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "requested"
            other = Path(tmp) / "other"
            for repo in (root, other):
                repo.mkdir()
                self.git(repo, "init", "-q")
                self.git(repo, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                         "commit", "--allow-empty", "-qm", "initial")
            (root / "unsaved").write_text("keep")
            with mock.patch.dict(os.environ, {"GIT_DIR": str(other / ".git"),
                                             "GIT_WORK_TREE": str(other),
                                             "GIT_INDEX_FILE": str(other / ".git" / "index")}):
                rows = report.worktrees(root)
            self.assertEqual([r["path"] for r in rows], [str(root)])
            self.assertEqual(rows[0]["status"], "changes present")
            self.assertEqual((root / "unsaved").read_text(), "keep")

    def test_all_worktrees_dirty_detached_locked_and_unusual_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "main"
            root.mkdir()
            self.git(root, "init", "-q")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "--allow-empty", "-qm", "initial")
            linked = Path(tmp) / "linked space\nline"
            self.git(root, "worktree", "add", "--detach", str(linked), "HEAD")
            self.git(root, "worktree", "lock", "--reason", "active build", str(linked))
            (linked / "unsaved.txt").write_text("keep me")
            rows = {r["path"]: r for r in report.worktrees(root)}
            self.assertEqual(rows[str(root)]["status"], "no tracked/untracked changes")
            self.assertEqual(rows[str(root)]["ignored_entries"], 0)
            self.assertEqual(rows[str(linked)]["status"], "changes present")
            self.assertTrue(rows[str(linked)]["detached"])
            self.assertEqual(rows[str(linked)]["locked"], "active build")
            self.assertEqual((linked / "unsaved.txt").read_text(), "keep me")

    def test_dirty_submodule_is_not_hidden_by_ignore_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "main"
            child = Path(tmp) / "child"
            root.mkdir()
            child.mkdir()
            for repo in (root, child):
                self.git(repo, "init", "-q")
            (child / "tracked").write_text("original")
            self.git(child, "add", "tracked")
            self.git(child, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "child")
            self.git(root, "-c", "protocol.file.allow=always", "submodule", "add", "-q",
                     str(child), "nested")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "main")
            self.git(root, "config", "submodule.nested.ignore", "all")
            changed = root / "nested" / "tracked"
            changed.write_text("unsaved")
            self.assertEqual(self.git(root, "status", "--porcelain=v1"), b"")
            row = report.worktrees(root)[0]
            self.assertEqual(row["status"], "changes present")
            self.assertEqual(changed.read_text(), "unsaved")

    def test_cli_complete_report_preserves_dirty_ignored_files_and_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            root.mkdir()
            self.git(root, "init", "-q")
            tracked = root / "tracked.txt"
            tracked.write_bytes(b"committed\n")
            (root / ".gitignore").write_text("ignored.bin\n")
            self.git(root, "add", "tracked.txt", ".gitignore")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "initial")
            tracked.write_bytes(b"unsaved tracked changes\n")
            untracked = root / "untracked.txt"
            untracked.write_bytes(b"unsaved untracked changes\n")
            ignored = root / "ignored.bin"
            ignored.write_bytes(b"ignored work must survive\x00")
            cache = Path(tmp) / "cache"
            cache.mkdir()
            artifact = cache / "artifact.bin"
            artifact.write_bytes(b"build artifact\x00")
            index = root / ".git" / "index"
            preserved = (tracked, untracked, ignored, artifact, index)
            status_before = self.git(root, "status", "--porcelain=v1", "-z")
            before = {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in preserved}

            result = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), "--repo", str(root),
                 "--cache", str(cache)], capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            parsed = json.loads(result.stdout)
            self.assertTrue(parsed["read_only"])
            self.assertEqual(parsed["worktrees"][0]["status"], "changes present")
            self.assertEqual(parsed["worktrees"][0]["ignored_entries"], 1)
            self.assertTrue(parsed["caches"][0]["complete"])
            self.assertEqual(parsed["caches"][0]["logical_bytes"], len(before[artifact][0]))
            self.assertTrue(any("ignored files" in warning for warning in parsed["warnings"]))
            self.assertEqual(
                {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in preserved}, before,
            )
            self.assertEqual(self.git(root, "status", "--porcelain=v1", "-z"), status_before)
            self.assertEqual(sorted(p.name for p in cache.iterdir()), ["artifact.bin"])

    def test_ignored_only_checkout_is_reported_without_exposing_or_modifying_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "main"
            root.mkdir()
            self.git(root, "init", "-q")
            (root / ".gitignore").write_text("hidden/\n")
            self.git(root, "add", ".gitignore")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "initial")
            hidden = root / "hidden"
            hidden.mkdir()
            secret = hidden / "private\nnotes.txt"
            secret.write_bytes(b"unsaved ignored work")
            index = root / ".git" / "index"
            before = {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in (secret, index)}

            rows = report.worktrees(root)

            self.assertEqual(rows[0]["status"], "no tracked/untracked changes")
            self.assertEqual(rows[0]["ignored_entries"], 1)
            self.assertNotIn("private", json.dumps(rows))
            self.assertEqual({p: (p.read_bytes(), p.stat().st_mtime_ns)
                              for p in (secret, index)}, before)

    def test_rename_source_that_looks_like_ignored_record_is_not_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.git(root, "init", "-q")
            source = root / "!! misleading.txt"
            source.write_text("tracked content")
            self.git(root, "add", ".")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "-qm", "initial")
            self.git(root, "mv", source.name, "renamed.txt")

            row = report.worktrees(root)[0]

            self.assertEqual(row["status"], "changes present")
            self.assertEqual(row["ignored_entries"], 0)

    def test_missing_checkout_status_is_unknown_not_clean(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "main"
            root.mkdir()
            self.git(root, "init", "-q")
            self.git(root, "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                     "commit", "--allow-empty", "-qm", "initial")
            linked = Path(tmp) / "linked"
            self.git(root, "worktree", "add", "--detach", str(linked), "HEAD")
            moved = Path(tmp) / "moved"
            linked.rename(moved)
            rows = {r["path"]: r for r in report.worktrees(root)}
            self.assertEqual(rows[str(linked)]["status"], "unknown")
            self.assertIsNone(rows[str(linked)]["ignored_entries"])
            self.assertTrue((moved / ".git").exists())


if __name__ == "__main__":
    unittest.main()
