#!/usr/bin/env python3
"""Expose tools/okf.py internals as JSON for the Rust parity tests.

Loads okf.py from the path in $OKF_PY (standard library only, no install).

usage:
    okf_shim.py frontmatter ROOT     every .md under ROOT: parsed frontmatter or error line and message
    okf_shim.py markdown ROOT        every .md under ROOT: body headings with slugs, footnotes
    okf_shim.py render               stdin: JSON list of values; stdout: each with its renderings
    okf_shim.py sha256               okf.py's SHA-256, to check that the goldens are current
    okf_shim.py analysis ROOT NOW COMMITS
                                     links, oldest, sprawl and lint --json for ROOT with
                                     now_utc frozen at NOW; COMMITS is a commit-times JSON
                                     file, "none" (git unavailable) or "git" (real git)
    okf_shim.py ops ROOT NOW         stdin: an ops script ({"setup": {path: text}, "steps": [...]});
                                     runs each step through okf.py's cmd_* functions on a temp copy
                                     of ROOT (plus setup) with now_utc frozen at NOW; stdout: each
                                     step's output or error, and every file that changed
"""
import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import shutil
import sys
import tempfile
from types import ModuleType
from typing import Any, NoReturn, Union

Json = Union[None, bool, int, float, str, list["Json"], dict[str, "Json"]]


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


def analysis(okf: ModuleType, root: str, now: str, commits: str) -> Json:
    """Run the analysis commands as okf.py's CLI does, returning their JSON.

    okf.py looks `now_utc` and `last_commit_times` up as module globals at
    call time, so replacing them here freezes the clock and injects the
    commit times for every command below. (`setattr` rather than attribute
    assignment: a ModuleType has no declared `now_utc` for mypy to check.)
    """
    fixed = okf.parse_ts(now)
    setattr(okf, "now_utc", lambda: fixed)
    if commits == "none":
        setattr(okf, "last_commit_times", lambda _root: (None, set()))
    elif commits != "git":
        with open(commits, encoding="utf-8") as fh:
            data = json.load(fh)
        times = {path: okf.parse_ts(at) for path, at in data["times"].items()}
        dirty = set(data["dirty"])
        setattr(okf, "last_commit_times", lambda _root: (times, dirty))

    def run(*argv: str) -> Json:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            try:
                okf.main([*argv, "--root", root])
            except SystemExit:  # lint exits 1 when there are errors
                pass
        # Parsed, not passed through as text: the harness compares JSON
        # values, so okf.py's own formatting (indent, ASCII escapes) is moot.
        result: Json = json.loads(out.getvalue())
        return result

    return {
        "links": run("links", "--json"),
        "oldest": run("oldest", "--json", "-n", "1000000"),
        "oldest_open_questions": run("oldest", "--json", "--open-questions"),
        "sprawl": run("sprawl", "--json", "-n", "1000000"),
        "lint": run("lint", "--json"),
    }


# argparse defaults of the commands the ops script may run (okf.py's `main`);
# a step's own fields override them. `by` defaults to "fixture" here, not
# okf.py's DEFAULT_ACTOR (which follows $OKF_ACTOR), so goldens are stable.
OPS_DEFAULTS: dict[str, dict[str, Json]] = {
    "stamp": {"by": "fixture"},
    "review": {"question": None, "by": "fixture"},
    "question": {},
    "unquestion": {},
    "source": {"title": None, "author": None},
}


def tree_bytes(root: str) -> dict[str, bytes]:
    """Every file under root, by root-relative path."""
    out: dict[str, bytes] = {}
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            full = os.path.join(dirpath, name)
            with open(full, "rb") as fh:
                out[os.path.relpath(full, root)] = fh.read()
    return out


def run_op(okf: ModuleType, work: str, step: dict[str, Any]) -> Json:
    """Run one script step as okf.py's CLI would, returning its output or error.

    okf.py's `die` prints `okf: <msg>` to stderr and exits; that becomes
    {"error": msg}. Otherwise stdout is the output, and stderr lines (okf.py's
    notes) are kept without their `okf: ` prefix. Paths under the temp copy
    are made catalog-relative, so the result does not depend on where it ran.
    """
    command = str(step["cmd"])
    fields = {**OPS_DEFAULTS[command], **{k: v for k, v in step.items() if k != "cmd"}}
    out, err = io.StringIO(), io.StringIO()
    failed = False
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            getattr(okf, f"cmd_{command}")(argparse.Namespace(root=work, **fields))
        except SystemExit:
            failed = True
    prefix = work + os.sep
    stdout = out.getvalue().replace(prefix, "")
    stderr = err.getvalue().replace(prefix, "")
    if failed:
        return {"error": stderr.removeprefix("okf: ").rstrip("\n")}
    entry: dict[str, Json] = {"output": stdout.rstrip("\n")}
    notes: list[Json] = [line.removeprefix("okf: ") for line in stderr.splitlines()]
    if notes:
        entry["notes"] = notes
    return entry


def ops(okf: ModuleType, root: str, now: str, script: dict[str, Any]) -> Json:
    """Run an ops script on a temp copy of root with now_utc frozen at now.

    `now_stamp` and `cmd_log` read `now_utc` as a module global at call
    time, so replacing it freezes every timestamp the commands write. The
    working directory is the copy while the steps run, because
    `resolve_concept` also tries its argument relative to the cwd.
    """
    fixed = okf.parse_ts(now)
    setattr(okf, "now_utc", lambda: fixed)
    home = os.getcwd()
    with tempfile.TemporaryDirectory() as tmp:
        work = os.path.join(tmp, "catalog")
        shutil.copytree(root, work, symlinks=True)
        for rel, text in script["setup"].items():
            full = os.path.join(work, rel)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8", newline="") as fh:
                fh.write(text)
        before = tree_bytes(work)
        os.chdir(work)
        try:
            steps: list[Json] = [run_op(okf, work, step) for step in script["steps"]]
        finally:
            os.chdir(home)
        after = tree_bytes(work)
    changed = sorted(p for p in before.keys() | after.keys() if before.get(p) != after.get(p))
    files: dict[str, Json] = {p: after[p].decode("utf-8") if p in after else None for p in changed}
    return {"steps": steps, "files": files}


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
    elif command == "analysis":
        if len(args) != 3:
            usage("analysis takes ROOT NOW COMMITS")
        result = analysis(load_okf(), *args)
    elif command == "ops":
        if len(args) != 2:
            usage("ops takes ROOT NOW (and the script on stdin)")
        script = json.load(sys.stdin)
        if not isinstance(script, dict):
            usage("ops reads a JSON object on stdin")
        result = ops(load_okf(), args[0], args[1], script)
    else:
        usage(f"bad arguments {argv!r}")
    # ASCII-only output: under `python3 -I` stdout's encoding comes from the
    # locale, and the JSON must not depend on it.
    json.dump(result, sys.stdout, ensure_ascii=True)


if __name__ == "__main__":
    main(sys.argv[1:])
