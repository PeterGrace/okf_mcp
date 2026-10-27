#!/usr/bin/env python3
"""Expose tools/okf.py internals as JSON for the Rust parity tests.

Loads okf.py from the path in $OKF_PY (standard library only, no install).

    okf_shim.py frontmatter ROOT     every .md under ROOT: parsed frontmatter or error line
    okf_shim.py headings ROOT        every .md under ROOT: body headings with slugs
    okf_shim.py render               stdin: JSON list of values; stdout: flow/block renderings
    okf_shim.py run --now TS ARG...  run okf.py's CLI with now_utc() frozen at TS (later milestones)
"""
import importlib.util
import json
import os
import sys
from datetime import datetime


def load_okf():
    # Never leave a __pycache__ next to okf.py in someone's catalog checkout.
    sys.dont_write_bytecode = True
    path = os.environ.get("OKF_PY")
    if not path or not os.path.isfile(path):
        sys.exit("okf_shim: set OKF_PY to the path of tools/okf.py")
    spec = importlib.util.spec_from_file_location("okf", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def frontmatter(okf, root):
    out = []
    for rel in okf.walk_md(root):
        doc = okf.Doc(os.path.join(root, rel))
        entry = {"path": rel, "has_frontmatter": doc.has_fm}
        if doc.has_fm:
            try:
                entry["value"] = doc.parse()
            except okf.YamlError as ex:
                entry["error_line"] = ex.line
        out.append(entry)
    return out


def headings(okf, root):
    out = []
    for rel in okf.walk_md(root):
        with open(os.path.join(root, rel), encoding="utf-8") as fh:
            text = fh.read()
        _, body = okf.body_split(text)
        out.append({"path": rel,
                    "headings": [[i, level, t, slug] for i, level, t, slug in okf.heading_slugs(body)]})
    return out


def render(okf, values):
    return [{"flow": okf.flow_value(v), "block": okf.block_item(v, 2)} for v in values]


def main():
    okf = load_okf()
    command, args = sys.argv[1], sys.argv[2:]
    if command == "frontmatter":
        result = frontmatter(okf, args[0])
    elif command == "headings":
        result = headings(okf, args[0])
    elif command == "render":
        result = render(okf, json.load(sys.stdin))
    elif command == "run":
        frozen = datetime.fromisoformat(args[1].replace("Z", "+00:00"))
        okf.now_utc = lambda: frozen
        okf.main(args[2:])
        return
    else:
        sys.exit(f"okf_shim: unknown command {command!r}")
    json.dump(result, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
