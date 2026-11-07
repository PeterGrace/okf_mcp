"""Isolated release integration tests: python3 -B -m unittest discover -s scripts -p 'test_release.py'."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
CARGO = shutil.which("cargo")
JUST = shutil.which("just")

FAKE_CARGO = '''#!/usr/bin/env python3
import os, subprocess, sys
if sys.argv[1] in ('fmt', 'clippy', 'doc', 'test'):
    sys.exit(1 if os.environ.get('FAIL_CHECK') == sys.argv[1] else 0)
sys.exit(subprocess.call([os.environ['REAL_CARGO'], *sys.argv[1:]]))
'''

FAKE_GH = '''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
p = pathlib.Path(os.environ['PR_STATE'])
def git(*a):
    return subprocess.check_output(['git', '--git-dir', os.environ['BARE'], *a], text=True).strip()
def option(name):
    return args[args.index(name) + 1]
if os.environ.get('GH_FAIL'):
    sys.exit(1)
pr = json.loads(p.read_text()) if p.exists() else None
command = args[1]
if command == 'list':
    print(json.dumps([{'number': 1}] if pr else []))
elif command == 'view':
    print(json.dumps(pr))
elif command == 'create':
    head = git('rev-parse', 'refs/heads/' + option('--head'))
    pr = dict(number=1, state='OPEN', headRefOid=head, headRefName=option('--head'),
              baseRefName='main', mergeCommit=None, url='https://github.com/test/repo/pull/1',
              tagsAtCreate=git('for-each-ref', '--format=%(refname)', 'refs/tags/').splitlines())
    p.write_text(json.dumps(pr))
    if os.environ.get('CREATE_FAIL_AFTER'):
        sys.exit(1)
    print(pr['url'])
elif command == 'merge':
    if os.environ.get('MERGE_FAIL_BEFORE'):
        sys.exit(1)
    assert option('--match-head-commit') == pr['headRefOid']
    base = git('rev-parse', 'refs/heads/main')
    tree = git('rev-parse', pr['headRefOid'] + '^{tree}')
    if os.environ.get('BAD_TREE'):
        tree = git('rev-parse', base + '^{tree}')
    if os.environ.get('MOVE_MAIN'):
        base = git('commit-tree', git('rev-parse', base + '^{tree}'), '-p', base, '-m', 'concurrent')
    merged = git('commit-tree', tree, '-p', base, '-p', pr['headRefOid'], '-m', 'merge release')
    git('update-ref', 'refs/heads/main', merged)
    pr['state'] = 'MERGED'
    pr['mergeCommit'] = {'oid': merged}
    p.write_text(json.dumps(pr))
    if os.environ.get('MERGE_FAIL_AFTER'):
        sys.exit(1)
else:
    raise AssertionError(args)
'''


class Releases(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="okf-release-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.bare = self.root / "origin.git"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        REAL_CARGO=CARGO, CARGO_NET_OFFLINE="true", BARE=str(self.bare),
                        PR_STATE=str(self.root / "pr.json"),
                        GIT_AUTHOR_NAME="Release Test", GIT_AUTHOR_EMAIL="test@example.com",
                        GIT_COMMITTER_NAME="Release Test", GIT_COMMITTER_EMAIL="test@example.com",
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        for name, content in (("cargo", FAKE_CARGO), ("gh", FAKE_GH)):
            path = self.bin / name
            path.write_text(content)
            path.chmod(0o755)
        for name in ("justfile", "release.toml", "scripts/release.py",
                     "scripts/release-prepare.sh", "scripts/release-ship.sh", "docs/operations.md"):
            destination = self.repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, destination)
        (self.repo / "src").mkdir()
        (self.repo / "src/lib.rs").write_text("pub fn example() {}\n")
        (self.repo / "changelog").mkdir()
        (self.repo / "Cargo.toml").write_text(
            '[package]\nname = "release_fixture"\nversion = "0.9.0"\nedition = "2021"\n')
        (self.repo / ".gitignore").write_text("target/\n__pycache__/\n")
        self.command("git", "init", "-b", "main")
        self.command("git", "init", "--bare", str(self.bare))
        self.command("git", "config", f"url.{self.bare}.insteadOf", "https://github.com/test/repo.git")
        self.command("git", "remote", "add", "origin", "https://github.com/test/repo.git")
        self.command(CARGO, "generate-lockfile", "--offline")
        self.command("git", "add", ".")
        self.command("git", "commit", "-m", "feat: fixture")
        self.command("git", "tag", "v0.9.0")
        self.command("git", "push", "origin", "main", "v0.9.0")
        self.base = self.git("rev-parse", "HEAD")

    def command(self, *args, ok=True, **env):
        result = subprocess.run(args, cwd=self.repo, env=dict(self.env, **env),
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result.stdout.strip()

    def git(self, *args):
        return self.command("git", *args)

    def prepare(self, level="patch", **env):
        return self.command(JUST, "release-prepare", level, **env)

    def ship(self, **env):
        return self.command(JUST, "release-ship", **env)

    def prepared(self):
        self.prepare()
        self.assertEqual(self.git("rev-list", "--count", f"{self.base}..HEAD"), "1")
        self.assertEqual(self.git("status", "--porcelain"), "")
        self.assertEqual(self.git("cat-file", "-t", "v0.9.1"), "tag")
        self.assertEqual(self.git("rev-parse", "origin/main"), self.base)
        self.assertIn('version = "0.9.1"', (self.repo / "Cargo.lock").read_text())
        self.assertIn("okf-mcp:0.9.1", (self.repo / "docs/operations.md").read_text())
        notes = list((self.repo / "changelog").glob("*-release-v0.9.1.md"))
        self.assertEqual(len(notes), 1)
        self.assertIn("Release v0.9.1", notes[0].read_text())
        return self.git("rev-parse", "HEAD")

    def test_prepare_and_ship(self):
        head = self.prepared()
        self.ship()
        state = json.loads(Path(self.env["PR_STATE"]).read_text())
        merged = state["mergeCommit"]["oid"]
        self.assertNotEqual(merged, head)
        self.assertEqual(self.git("rev-parse", "v0.9.1^{commit}"), merged)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v0.9.1^{}"),
                         f"{merged}\trefs/tags/v0.9.1^{{}}")
        tag = self.git("rev-parse", "v0.9.1")
        self.ship(ok=False)
        self.assertEqual(self.git("rev-parse", "v0.9.1"), tag)

    def test_levels_and_exact_version(self):
        self.prepare("1.2.3")
        self.assertEqual(self.git("rev-parse", "v1.2.3^{commit}"), self.git("rev-parse", "HEAD"))

    def test_bad_arguments_no_mutation(self):
        for value in ("", "rc", "--execute", "0.9.0", "0.8.0", "01.2.3", "1.2.3-rc.1",
                      "patch; touch INJECTED", "$(touch INJECTED)"):
            with self.subTest(value=value):
                self.prepare(value, ok=False)
                self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
                self.assertEqual(self.git("status", "--porcelain"), "")
        self.command(JUST, "release-prepare", ok=False)
        self.command(JUST, "release-ship", "extra", ok=False)

    def test_dirty_and_wrong_branch(self):
        (self.repo / "dirty").touch()
        self.prepare(ok=False)
        (self.repo / "dirty").unlink()
        self.git("switch", "-c", "other")
        self.prepare(ok=False)
        self.ship(ok=False)

    def test_stale_main(self):
        self.git("commit", "--allow-empty", "-m", "local")
        self.prepare(ok=False)

    def test_tag_conflicts(self):
        self.git("tag", "v0.9.1")
        self.prepare(ok=False)
        self.git("push", "origin", "v0.9.1")
        self.git("tag", "-d", "v0.9.1")
        self.prepare(ok=False)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)

    def test_failed_prepare_checks_preserve_changes(self):
        self.prepare(FAIL_CHECK="test", ok=False)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertIn('version = "0.9.1"', (self.repo / "Cargo.toml").read_text())
        self.assertEqual(self.git("tag", "--list", "v0.9.1"), "")

    def test_failed_ship_checks_no_pr(self):
        self.prepared()
        self.ship(FAIL_CHECK="clippy", ok=False)
        self.assertFalse(Path(self.env["PR_STATE"]).exists())

    def test_pr_reuse_after_create_partial_failure(self):
        self.prepared()
        self.ship(CREATE_FAIL_AFTER="1", ok=False)
        self.assertEqual(json.loads(Path(self.env["PR_STATE"]).read_text())["state"], "OPEN")
        self.ship()

    def test_resume_after_merge_response_failure(self):
        self.prepared()
        self.ship(MERGE_FAIL_AFTER="1", ok=False)
        self.assertEqual(json.loads(Path(self.env["PR_STATE"]).read_text())["state"], "MERGED")
        self.ship()

    def test_resume_after_tag_push_failure(self):
        self.prepared()
        hook = self.bare / "hooks/pre-receive"
        hook.write_text('#!/bin/sh\nwhile read old new ref; do\ncase "$ref" in refs/tags/*) exit 1;; esac\ndone\n')
        hook.chmod(0o755)
        self.ship(ok=False)
        self.assertIn("Prepared:", self.git("for-each-ref", "--format=%(contents)", "refs/tags/v0.9.1"))
        hook.unlink()
        self.ship()

    def test_bad_merged_tree_refused_before_retag(self):
        head = self.prepared()
        self.assertIn("merged tree differs", self.ship(BAD_TREE="1", ok=False))
        self.assertEqual(self.git("rev-parse", "v0.9.1^{commit}"), head)
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/tags/v0.9.1"), "")

    def test_concurrent_main_move_refused(self):
        self.prepared()
        self.assertIn("release merge parents differ", self.ship(MOVE_MAIN="1", ok=False))
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/tags/v0.9.1"), "")

    def test_remote_branch_conflict(self):
        self.prepared()
        self.git("push", "origin", f"{self.base}:refs/heads/release/v0.9.1")
        self.assertIn("remote release branch differs", self.ship(ok=False))

    def test_api_failure(self):
        self.prepared()
        self.ship(GH_FAIL="1", ok=False)
        self.assertFalse(Path(self.env["PR_STATE"]).exists())

    def test_origin_errors_fail_closed(self):
        self.prepared()
        self.git("remote", "set-url", "origin", str(self.root / "missing.git"))
        self.ship(ok=False)

    def test_minor(self):
        self.prepare("minor")
        self.assertEqual(self.git("rev-parse", "v0.10.0^{commit}"), self.git("rev-parse", "HEAD"))

    def test_major(self):
        self.prepare("major")
        self.assertEqual(self.git("rev-parse", "v1.0.0^{commit}"), self.git("rev-parse", "HEAD"))

    def test_notes_use_committed_history(self):
        self.git("commit", "--allow-empty", "-m", "fix: new committed change")
        self.git("push", "origin", "main")
        self.prepare()
        notes = next((self.repo / "changelog").glob("*-release-v0.9.1.md")).read_text()
        self.assertIn("fix: new committed change", notes)
        self.assertNotIn("feat: fixture", notes)
        self.assertRegex(notes, r"# Release v0\.9\.1 \(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z\)")

    def test_resume_after_main_advances_past_valid_merge(self):
        self.prepared()
        self.ship(MERGE_FAIL_AFTER="1", ok=False)
        merged = json.loads(Path(self.env["PR_STATE"]).read_text())["mergeCommit"]["oid"]
        tree = self.command("git", "--git-dir", str(self.bare), "rev-parse", merged + "^{tree}")
        later = self.command("git", "--git-dir", str(self.bare), "commit-tree", tree,
                             "-p", merged, "-m", "later main change")
        self.command("git", "--git-dir", str(self.bare), "update-ref", "refs/heads/main", later)
        self.ship()
        self.assertEqual(self.git("rev-parse", "v0.9.1^{commit}"), merged)

    def test_closed_pr_refused(self):
        self.prepared()
        self.ship(CREATE_FAIL_AFTER="1", ok=False)
        path = Path(self.env["PR_STATE"])
        state = json.loads(path.read_text())
        state["state"] = "CLOSED"
        path.write_text(json.dumps(state))
        self.assertIn("closed without merging", self.ship(ok=False))

    def test_mismatched_pr_head_refused(self):
        self.prepared()
        self.ship(CREATE_FAIL_AFTER="1", ok=False)
        path = Path(self.env["PR_STATE"])
        state = json.loads(path.read_text())
        state["headRefOid"] = self.base
        path.write_text(json.dumps(state))
        self.assertIn("PR does not match", self.ship(ok=False))

    def test_lightweight_or_unmarked_tag_refused(self):
        self.prepared()
        self.git("tag", "-d", "v0.9.1")
        self.git("tag", "v0.9.1")
        self.assertIn("expected annotated release tag", self.ship(ok=False))
        self.git("tag", "-f", "-a", "v0.9.1", "-m", "other annotation")
        self.assertIn("not created by release-prepare", self.ship(ok=False))

    def test_ship_dirty_tree(self):
        self.prepared()
        (self.repo / "dirty").touch()
        self.assertIn("working tree must be clean", self.ship(ok=False))

    def test_fetch_failure_before_prepare(self):
        self.git("remote", "set-url", "origin", str(self.root / "missing.git"))
        self.prepare(ok=False)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def follow_tags_prepared(self):
        self.prepared()
        self.git("config", "push.followTags", "true")
        self.git("tag", "-a", "unrelated", "-m", "not part of this release")

    def assert_only_original_remote_tag(self):
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/tags/*"),
                         f"{self.git('rev-parse', 'v0.9.0')}\trefs/tags/v0.9.0")
        state = json.loads(Path(self.env["PR_STATE"]).read_text())
        self.assertEqual(state["tagsAtCreate"], ["refs/tags/v0.9.0"])

    def test_follow_tags_create_failure_publishes_no_tags(self):
        self.follow_tags_prepared()
        self.ship(CREATE_FAIL_AFTER="1", ok=False)
        self.assert_only_original_remote_tag()

    def test_follow_tags_merge_failure_publishes_no_tags(self):
        self.follow_tags_prepared()
        self.ship(MERGE_FAIL_BEFORE="1", ok=False)
        self.assert_only_original_remote_tag()
        self.assertEqual(json.loads(Path(self.env["PR_STATE"]).read_text())["state"], "OPEN")

    def test_follow_tags_success_publishes_only_validated_tag(self):
        self.follow_tags_prepared()
        self.ship()
        state = json.loads(Path(self.env["PR_STATE"]).read_text())
        self.assertEqual(state["tagsAtCreate"], ["refs/tags/v0.9.0"])
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/tags/unrelated"), "")
        self.assertEqual(self.git("cat-file", "-t", "unrelated"), "tag")
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v0.9.1^{}"),
                         f"{state['mergeCommit']['oid']}\trefs/tags/v0.9.1^{{}}")

    def test_source_mapping_cannot_redirect_release_tag(self):
        self.prepared()
        self.git("config", "remote.origin.push", "refs/tags/v0.9.1:refs/tags/unintended")
        self.ship()
        merged = json.loads(Path(self.env["PR_STATE"]).read_text())["mergeCommit"]["oid"]
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v0.9.1^{}"),
                         f"{merged}\trefs/tags/v0.9.1^{{}}")
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/tags/unintended"), "")
        self.assertEqual(len(self.git("ls-remote", "--refs", "origin", "refs/tags/*").splitlines()), 2)

    def test_multiple_push_urls_refused_before_any_push(self):
        self.prepared()
        other = self.root / "other.git"
        self.command("git", "init", "--bare", str(other))
        self.git("config", "--add", "remote.origin.pushurl", "https://github.com/test/repo.git")
        self.git("config", "--add", "remote.origin.pushurl", str(other))
        self.assertIn("exactly one resolved fetch URL", self.ship(ok=False))
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/heads/release/*", "refs/tags/v0.9.1"), "")
        self.assertEqual(self.command("git", "--git-dir", str(other), "for-each-ref"), "")
        self.assertFalse(Path(self.env["PR_STATE"]).exists())

    def test_multiple_fetch_urls_refused_before_any_push(self):
        self.prepared()
        self.git("config", "--add", "remote.origin.url", str(self.root / "unchecked.git"))
        self.assertIn("exactly one resolved fetch URL", self.ship(ok=False))
        self.assertEqual(self.git("ls-remote", "--refs", "origin", "refs/heads/release/*", "refs/tags/v0.9.1"), "")
        self.assertFalse(Path(self.env["PR_STATE"]).exists())

    def test_unprepared_head_refused(self):
        self.prepared()
        self.git("commit", "--allow-empty", "-m", "unrelated")
        self.ship(ok=False)


if __name__ == "__main__":
    unittest.main()
