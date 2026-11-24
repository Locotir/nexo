#!/usr/bin/env python3
"""Exercise cancellation of the checker and a real child process, offline.

Run on POSIX: python3 tools/tests/pre-push-process-signals.py
This models Ctrl-C / termination delivered to the foreground process group;
it does not test signals delivered to the checker alone.
"""
import os
import pathlib
import shutil
import signal
import subprocess
import tempfile
import time

CHECKER = pathlib.Path(__file__).resolve().parents[1] / "pre-push-check.sh"


def run(sig, shell, phase):
    with tempfile.TemporaryDirectory(prefix="nexo-pre-push-") as tmp:
        root = pathlib.Path(tmp)
        # Run the real checker in an isolated tree; stubs prevent recursive suites.
        tools = root / "tree" / "tools"
        suites = tools / "tests"
        suites.mkdir(parents=True)
        checker = tools / "pre-push-check.sh"
        shutil.copyfile(CHECKER, checker)
        (suites / "pre-push-check.sh").write_text("exit 0\n")
        (suites / "pre-push-process-signals.py").write_text("pass\n")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        calls = root / "calls"
        ready = root / "ready"
        for name, body in {
            "uname": "#!/bin/sh\necho Darwin\n",
            "cargo": "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CALLS\"\n"
                     "if [ \"$*\" = \"$BLOCK_AT\" ]; then\n"
                     "  echo ready > \"$READY\"\n  exec sleep 60\nfi\n",
        }.items():
            p = bin_dir / name
            p.write_text(body)
            p.chmod(0o755)
        env = os.environ.copy()
        env.pop("NEXO_UPDATE_GOLDEN", None)
        env.pop("NEXO_BLESS", None)
        block_at = "+1.89.0 --version" if phase == "preflight" else "build --workspace --locked"
        env.update(PATH=f"{bin_dir}:{env['PATH']}", CALLS=str(calls), READY=str(ready),
                   BLOCK_AT=block_at)
        args = [shell, str(checker)] + (["--full"] if phase == "preflight" else [])
        proc = subprocess.Popen(args, env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, start_new_session=True, text=True)
        try:
            deadline = time.monotonic() + 10
            while not ready.exists():
                if proc.poll() is not None or time.monotonic() >= deadline:
                    raise AssertionError("checker did not start the real child")
                time.sleep(0.01)
            os.killpg(proc.pid, sig)
            output, _ = proc.communicate(timeout=10)
            assert proc.returncode == 128 + sig, (sig, proc.returncode, output)
            expected = (["+1.89.0 --version"] if phase == "preflight" else
                        ["fmt --check", "build --workspace --locked"])
            assert calls.read_text().splitlines() == expected
            assert "Requested local checks passed" not in output
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError("child process group still exists after cancellation")
        finally:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.communicate()
    print(f"ok: process-group {shell} {phase} {sig.name}")


def cancel(signum, _frame):
    # Unwind run()'s finally block, which kills its detached child process group.
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    for sig in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, cancel)
    for shell in ("sh", "bash"):
        for phase in ("preflight", "build"):
            for s in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
                run(s, shell, phase)
