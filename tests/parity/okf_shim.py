#!/usr/bin/env python3
"""Expose tools/okf.py internals as JSON for the Rust parity tests.

Loads okf.py from the path in $OKF_PY (standard library only, no install).

usage:
    okf_shim.py frontmatter ROOT     every .md under ROOT: parsed frontmatter or error line and message
    okf_shim.py markdown ROOT        every .md under ROOT: body headings with slugs, footnotes
    okf_shim.py render               stdin: JSON list of values; stdout: each with its renderings
    okf_shim.py sha256               okf.py's SHA-256, to check that the goldens are current
"""
import hashlib
import importlib.util
import json
import os
import sys
from types import ModuleType
from typing import NoReturn, Union

Json = Union[None, bool, int, str, list["Json"], dict[str, "Json"]]


def usage(problem: str) -> NoReturn:
    """Print the problem and the usage text to stderr, then exit with status 2."""
    sys.stderr.write(f"okf_shim: {problem}\n{__doc__}")
    sys.exit(2)


def okf_path() -> str:
    path = os.environ.get("OKF_PY")
    if not path or not os.path.isfile(path):
        usage("set OKF_PY to the path of tools/okf.py")
    return path


def load_okf() -> ModuleType:
    # Never leave a __pycache__ next to okf.py in someone's catalog checkout.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("okf", okf_path())
    if spec is None or spec.loader is None:
        usage("OKF_PY is not a loadable Python file")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def frontmatter(okf: ModuleType, root: str) -> list[Json]:
    out: list[Json] = []
    for rel in okf.walk_md(root):
        doc = okf.Doc(os.path.join(root, rel))
        entry: dict[str, Json] = {"path": rel, "has_frontmatter": doc.has_fm}
        if doc.has_fm:
            try:
                entry["value"] = doc.parse()
            except okf.YamlError as ex:
                entry["error_line"] = ex.line
                entry["error_message"] = str(ex)
        out.append(entry)
    return out


def markdown(okf: ModuleType, root: str) -> list[Json]:
    out: list[Json] = []
    for rel in okf.walk_md(root):
        # Text mode with universal newlines, as okf.py reads files for analysis.
        with open(os.path.join(root, rel), encoding="utf-8") as fh:
            text = fh.read()
        _, body = okf.body_split(text)
        out.append({"path": rel,
                    "headings": [[i, level, t, slug] for i, level, t, slug in okf.heading_slugs(body)],
                    "footnotes": okf.footnote_defs(body)})
    return out


def render(okf: ModuleType, values: list[Json]) -> list[Json]:
    return [{"input": v, "flow": okf.flow_value(v), "block": okf.block_item(v, 2)} for v in values]


def sha256() -> str:
    with open(okf_path(), "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def main(argv: list[str]) -> None:
    if not argv:
        usage("missing command")
    command, args = argv[0], argv[1:]
    result: Json
    if command in ("frontmatter", "markdown"):
        if len(args) != 1:
            usage(f"{command} takes exactly one ROOT")
        walker = frontmatter if command == "frontmatter" else markdown
        result = walker(load_okf(), args[0])
    elif command == "render" and not args:
        values = json.load(sys.stdin)
        if not isinstance(values, list):
            usage("render reads a JSON list on stdin")
        result = render(load_okf(), values)
    elif command == "sha256" and not args:
        result = sha256()
    else:
        usage(f"bad arguments {argv!r}")
    # ASCII-only output: under `python3 -I` stdout's encoding comes from the
    # locale, and the JSON must not depend on it.
    json.dump(result, sys.stdout, ensure_ascii=True)


if __name__ == "__main__":
    main(sys.argv[1:])
