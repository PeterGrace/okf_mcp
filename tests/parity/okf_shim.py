#!/usr/bin/env python3
"""Expose tools/okf.py internals as JSON for the Rust parity tests.

Loads okf.py from the path in $OKF_PY (standard library only, no install).

    okf_shim.py frontmatter ROOT     every .md under ROOT: parsed frontmatter or error line and kind
    okf_shim.py markdown ROOT        every .md under ROOT: body headings with slugs, footnotes
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


def error_kind(message: str) -> str:
    """The stable part of a YamlError message: the text before the first quote
    character or " in ", whichever comes first, without trailing spaces or
    colons. What follows is quoted source text, which okf.py writes with
    repr() and the Rust port with Debug escaping, so only the prefix compares.
    tests/parity.rs implements the same rule."""
    cuts = [i for i in (message.find("'"), message.find('"'), message.find(" in ")) if i >= 0]
    return message[:min(cuts, default=len(message))].rstrip(" :")


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
                entry["error_kind"] = error_kind(str(ex))
        out.append(entry)
    return out


def markdown(okf, root):
    out = []
    for rel in okf.walk_md(root):
        # Text mode with universal newlines, as okf.py reads files for analysis.
        with open(os.path.join(root, rel), encoding="utf-8") as fh:
            text = fh.read()
        _, body = okf.body_split(text)
        out.append({"path": rel,
                    "headings": [[i, level, t, slug] for i, level, t, slug in okf.heading_slugs(body)],
                    "footnotes": okf.footnote_defs(body)})
    return out


def render(okf, values):
    return [{"flow": okf.flow_value(v), "block": okf.block_item(v, 2)} for v in values]


def main():
    okf = load_okf()
    command, args = sys.argv[1], sys.argv[2:]
    if command == "frontmatter":
        result = frontmatter(okf, args[0])
    elif command == "markdown":
        result = markdown(okf, args[0])
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
