"""Linux-only isolated test driver; adopts and reaps only its owned probe/helper."""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

# Become the orphan reaper in this isolated driver, never in the parallel harness.
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
    raise OSError(ctypes.get_errno(), "subreaper failed")

root = Path(os.environ["CLI_TEST_ROOT"])
runner = subprocess.Popen(
    [sys.argv[1], "--ignored", "--exact", "tests::cli_scenario", "--nocapture"],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
)
probe = None
try:
    deadline = time.monotonic() + 10
    while probe is None:
        try:
            value = (root / "probe.pid").read_text()
            if value.isdecimal():
                probe = int(value)
                break
        except FileNotFoundError:
            pass
        if runner.poll() is not None or time.monotonic() >= deadline:
            raise AssertionError("readiness probe did not start")
        time.sleep(0.005)
    if sys.argv[2] != "deadline":
        runner.send_signal(getattr(signal, sys.argv[2]))
    stdout, stderr = runner.communicate(timeout=5)
    assert runner.returncode == 0, (stdout.decode(), stderr.decode())
    assert not Path(f"/proc/{probe}").exists(), "probe not reaped before runner exit"
    assert not stderr, stderr.decode()
    assert b"CANARY" not in stdout
finally:
    # Kill/reap only processes created by this driver, even on assertion/timeout.
    if runner.poll() is None:
        runner.kill()
    runner.wait(timeout=5)
    if probe is None and (root / "probe.pid").exists():
        probe = int((root / "probe.pid").read_text())
    if probe is not None:
        try:
            reaped, _ = os.waitpid(probe, os.WNOHANG)
            if reaped == 0:
                os.kill(probe, signal.SIGKILL)
                os.waitpid(probe, 0)
        except ChildProcessError:
            pass  # The runner already reaped it; never signal a reused PID.
