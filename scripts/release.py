"""Private release implementation; public interfaces are the two just recipes."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


STABLE = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
MARKER = "okf-release-prepare-v1"


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def git(*args):
    return run("git", *args)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def version():
    value = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
    require(re.fullmatch(STABLE, value), "package version must be stable MAJOR.MINOR.PATCH")
    return value


def clean_main():
    require(git("branch", "--show-current") == "main", "must run on main")
    require(not git("status", "--porcelain"), "working tree must be clean")


def fetch_main():
    git("fetch", "--no-tags", "origin", "+refs/heads/main:refs/remotes/origin/main")
    return git("rev-parse", "refs/remotes/origin/main")


def remote_ref(ref):
    # An empty successful ls-remote is absence; errors must never mean absence.
    lines = git("ls-remote", "--refs", "origin", ref).splitlines()
    require(len(lines) <= 1, f"ambiguous origin ref: {ref}")
    if not lines:
        return None
    oid, name = lines[0].split()
    require(name == ref, f"unexpected origin ref: {name}")
    return oid


def unpublished(tag):
    require(remote_ref(f"refs/tags/{tag}") is None, f"{tag} already exists on origin")


def checks():
    for recipe in ("fmt", "lint", "test"):
        subprocess.run(["just", recipe], check=True)


def prepare(level):
    require(level in ("patch", "minor", "major") or re.fullmatch(STABLE, level),
            "expected patch, minor, major, or stable MAJOR.MINOR.PATCH")
    old = version()
    parts = [int(part) for part in old.split(".")]
    if level in ("patch", "minor", "major"):
        index = {"major": 0, "minor": 1, "patch": 2}[level]
        parts[index] += 1
        parts[index + 1:] = [0] * (2 - index)
        target = ".".join(map(str, parts))
    else:
        target = level
    require(tuple(map(int, target.split("."))) > tuple(map(int, old.split("."))),
            "release version must increase")
    require(run("cargo", "release", "--version") == "cargo-release 0.25.18",
            "install cargo-release 0.25.18")
    clean_main()
    base = fetch_main()
    require(git("rev-parse", "HEAD") == base, "main must equal fetched origin/main")
    tag = f"v{target}"
    require(not git("tag", "--list", tag), f"local tag {tag} already exists")
    unpublished(tag)
    # Explicit isolated config prevents per-user release settings changing this protocol.
    subprocess.run(["cargo", "release", level, "--execute", "--no-confirm",
                    "--isolated", "--config", "release.toml", "--no-push", "--no-publish"],
                   check=True)
    require(version() == target, "cargo-release produced an unexpected version")
    require(git("rev-parse", "HEAD^") == base, "expected one release commit")
    require(git("rev-parse", f"{tag}^{{commit}}") == git("rev-parse", "HEAD"),
            "release tag does not point at the release commit")
    clean_main()
    print(f"Prepared {tag} locally. Nothing pushed; run just release-ship.")


def hook(old, new):
    require(os.environ.get("DRY_RUN") == "false", "hook requires cargo-release execute")
    require(version() == new and re.fullmatch(STABLE, old), "unexpected hook versions")
    path = Path("docs/operations.md")
    text = path.read_text()
    # These are the pinned image examples, not arbitrary mentions of old versions.
    pattern = rf"(?<=[:v]){re.escape(old)}\b"
    updated, count = re.subn(pattern, new, text)
    require(count == 4, "expected four pinned image version mentions")
    path.write_text(updated)
    timestamp = run("date", "-u", "+%Y-%m-%dT%H:%M:%SZ")
    filename_time = timestamp.replace(":", "")
    previous = git("tag", "--merged", "HEAD", "--list", "v*", "--sort=-version:refname")
    previous = next((tag for tag in previous.splitlines()
                     if re.fullmatch("v" + STABLE, tag)), None)
    history = git("log", "--no-merges", "--format=- %h %s",
                  f"{previous}..HEAD" if previous else "HEAD")
    notes = Path(f"changelog/{filename_time}-release-v{new}.md")
    with notes.open("x") as stream:
        stream.write(f"# Release v{new} ({timestamp})\n\n"
                     f"Committed changes since {previous or 'the start of history'}:\n\n"
                     f"{history}\n")
    checks()
    # cargo-release stages tracked updates only; explicitly include the new notes.
    git("add", "--", str(notes))


def ship():
    clean_main()
    prepared = git("rev-parse", "HEAD")
    tag = f"v{version()}"
    require(git("cat-file", "-t", f"refs/tags/{tag}") == "tag", "expected annotated release tag")
    annotation = git("for-each-ref", "--format=%(contents)", f"refs/tags/{tag}")
    initial_message = f"Release {tag}\n\n{MARKER}"
    resumed_message = f"{initial_message}\nPrepared: {prepared}"
    require(annotation in (initial_message, resumed_message), "tag was not created by release-prepare")
    target = git("rev-parse", f"{tag}^{{commit}}")
    require(annotation == resumed_message or target == prepared, "prepared tag must point at HEAD")
    require(git("show", "-s", "--format=%s", prepared) == f"chore: Release {tag}",
            "HEAD is not the prepared release commit")
    parents = git("show", "-s", "--format=%P", prepared).split()
    require(len(parents) == 1, "prepared commit must have one parent")
    base = parents[0]
    unpublished(tag)
    fetch_urls = git("remote", "get-url", "--all", "origin").splitlines()
    push_urls = git("remote", "get-url", "--push", "--all", "origin").splitlines()
    require(len(fetch_urls) == len(push_urls) == 1 and fetch_urls == push_urls,
            "origin must have exactly one resolved fetch URL and one matching push URL")
    origin = git("config", "--get", "remote.origin.url")
    match = re.fullmatch(r"(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)([\w.-]+/[\w.-]+?)(?:\.git)?", origin)
    require(match, "origin must be a GitHub HTTPS or SSH repository URL")
    repo = match[1]
    branch = f"release/{tag}"

    def gh(*args):
        return run("gh", "pr", *args, "--repo", repo)

    def view(number):
        return json.loads(gh("view", str(number), "--json",
                             "number,state,headRefOid,headRefName,baseRefName,mergeCommit,url"))

    remote_main = fetch_main()
    prs = json.loads(gh("list", "--state", "all", "--head", branch, "--base", "main",
                        "--json", "number"))
    require(isinstance(prs, list) and len(prs) <= 1, "ambiguous release PRs")
    pr = view(prs[0]["number"]) if prs else None
    if pr is None or pr["state"] != "MERGED":
        require(remote_main == base, "origin/main moved since preparation")
        require(target == prepared, "unmerged release tag must point at prepared HEAD")
        require(pr is None or pr["state"] == "OPEN", "release PR is closed without merging")
    else:
        require(target in (prepared, pr["mergeCommit"]["oid"]), "unexpected resumed tag target")

    checks()
    clean_main()
    require(git("rev-parse", "HEAD") == prepared, "HEAD changed during checks")
    require(git("rev-parse", f"{tag}^{{commit}}") == target, "tag changed during checks")
    unpublished(tag)
    branch_oid = remote_ref(f"refs/heads/{branch}")
    require(branch_oid in (None, prepared), "remote release branch differs from prepared commit")
    if pr is None or pr["state"] != "MERGED":
        require(fetch_main() == base, "origin/main moved during checks")
        if branch_oid is None:
            # An empty lease is create-only, not permission to force-update a ref.
            # Even a concurrent branch creation must fail rather than fast-forward it.
            git("push", "--no-follow-tags", "--force-with-lease=refs/heads/" + branch + ":", "origin",
                f"{prepared}:refs/heads/{branch}")
        if pr is None:
            gh("create", "--base", "main", "--head", branch,
               "--title", f"chore: Release {tag}", "--body",
               "Prepared by cargo-release; fmt, lint and test run locally. No PR CI is configured.")
            prs = json.loads(gh("list", "--state", "all", "--head", branch, "--base", "main",
                                "--json", "number"))
            require(len(prs) == 1, "could not identify created PR; rerun to resume")
            pr = view(prs[0]["number"])
    require(pr["headRefOid"] == prepared and pr["headRefName"] == branch
            and pr["baseRefName"] == "main", "PR does not match prepared release")
    if pr["state"] != "MERGED":
        require(pr["state"] == "OPEN", "release PR is not open")
        require(fetch_main() == base, "origin/main moved before merge")
        gh("merge", str(pr["number"]), "--merge", "--match-head-commit", prepared)
        pr = view(pr["number"])
    require(pr["state"] == "MERGED", "PR has not merged; rerun after it merges")
    merged = pr["mergeCommit"]["oid"]
    require(re.fullmatch(r"[0-9a-f]{40}", merged), "invalid PR merge commit")
    remote_main = fetch_main()
    git("merge-base", "--is-ancestor", merged, remote_main)
    require(git("show", "-s", "--format=%P", merged).split() == [base, prepared],
            "release merge parents differ; main moved or PR was not merge-committed")
    require(git("rev-parse", f"{merged}^{{tree}}") == git("rev-parse", f"{prepared}^{{tree}}"),
            "merged tree differs from validated prepared tree")
    git("merge-base", "--is-ancestor", prepared, merged)
    unpublished(tag)
    require(git("rev-parse", f"{tag}^{{commit}}") == target, "local tag changed")
    # Only the unpublished, marked preparation tag may move. HEAD stays prepared for retries.
    if target != merged:
        git("tag", "--force", "--annotate", tag, merged, "--message", resumed_message)
    git("push", "--no-follow-tags", "origin", f"refs/tags/{tag}:refs/tags/{tag}")
    print(f"Shipped {tag} at {merged}. Local main remains at {prepared}; fast-forward manually.")


if __name__ == "__main__":
    try:
        args = sys.argv[1:]
        if len(args) == 2 and args[0] == "prepare":
            prepare(args[1])
        elif args == ["ship"]:
            ship()
        elif len(args) == 3 and args[0] == "hook":
            hook(*args[1:])
        else:
            raise ValueError("usage: just release-prepare <patch|minor|major|VERSION> | just release-ship")
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"Release stopped: {error}. State is preserved; inspect before retrying or cleaning up.")
