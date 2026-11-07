#!/usr/bin/python3
"""Synthetic owned child only: no provider, network, or Claude invocation."""
import json
import os
import sys
import time
import threading

root = os.path.dirname(os.path.realpath(__file__))
with open(root + "/scenario.json") as f:
    cfg = json.load(f)
with open(root + "/record.json", "w") as f:
    json.dump({"argv": sys.argv[1:], "env": dict(os.environ), "cwd": os.getcwd(),
               "mode": os.stat(os.getcwd()).st_mode & 0o777, "pid": os.getpid()}, f)
if "--version" in sys.argv:
    print(cfg.get("version", "2.1.319 (Claude Code)"))
    sys.exit(0)
if "--help" in sys.argv:
    print(cfg["help"])
    sys.exit(0)
if cfg.get("stall"):
    time.sleep(120)
prompt = sys.stdin.buffer.read()
with open(root + "/stdin", "wb") as f:
    f.write(prompt)
def stderr():
    sys.stderr.buffer.write(b"S" * cfg.get("stderr", 0))
    sys.stderr.buffer.flush()
t = threading.Thread(target=stderr)
t.start()
with open(root + "/success.json", "rb") as f:
    payload = f.read()
size = cfg.get("stdout", len(payload))
sys.stdout.buffer.write(payload + b" " * max(0, size - len(payload)))
sys.stdout.buffer.flush()
t.join()
if cfg.get("hold_pipes"):
    # A short-lived owned fake descendant keeps the pipes open after parent exit.
    # It exits naturally; the adapter must never signal an unowned PID/group.
    if os.fork() == 0:
        time.sleep(3)
        os._exit(0)
if cfg.get("after_output_stall"):
    time.sleep(120)
sys.exit(cfg.get("exit", 0))
