"""Packaging checks (PyYAML required); set MAINTENANCE_TEST_IMAGE for offline Docker checks.

python3 -B -m unittest discover -s scripts -p 'test_maintenance_image.py' -v
"""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]
CLAUDE = "/usr/local/bin/claude-2.1.319"
RUNNER = "/usr/local/bin/okf-maintenance"
SHA256 = "dabafec82ca1319fa43d8be36a10fc61b1d11821cb98ad5df04ad95e199b8eed"
IMAGE = os.environ.get("MAINTENANCE_TEST_IMAGE")


class Packaging(unittest.TestCase):
    def read(self, path):
        self.assertTrue((ROOT / path).is_file(), f"missing packaging file: {path}")
        return (ROOT / path).read_text()

    def test_native_download_is_fixed_and_verified_before_execution(self):
        dockerfile = self.read("Dockerfile.maintenance")
        self.assertIn("https://downloads.claude.ai/claude-code-releases/2.1.319/linux-x64/claude", dockerfile)
        self.assertIn(SHA256, dockerfile)
        verify = dockerfile.index("sha256sum -c -")
        self.assertLess(verify, dockerfile.index("install -m 0755"))
        self.assertLess(verify, dockerfile.index(f"{CLAUDE} --version"))
        self.assertIn(f'{CLAUDE} --help', dockerfile)
        self.assertNotRegex(dockerfile, r"install\.sh|curl[^\n]*\|[^\n]*(?:sh|bash)|/latest/")

    def test_corrupt_download_fails_before_installing_executable(self):
        dockerfile = self.read("Dockerfile.maintenance")
        command = dockerfile.split("RUN curl ", 1)[1].split("\n\nFROM ", 1)[0]
        command = "curl " + command.replace("\\\n", " ")
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            download = directory / "download"
            installed = directory / "installed"
            curl = directory / "curl"
            curl.write_text("#!/usr/bin/env python3\nimport pathlib, sys\npathlib.Path(sys.argv[sys.argv.index('--output') + 1]).write_bytes(b'corrupt download')\n")
            curl.chmod(0o755)
            command = command.replace("/tmp/claude", str(download)).replace(CLAUDE, str(installed))
            result = subprocess.run(["/bin/sh", "-c", command], text=True, capture_output=True,
                                    env={**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"]},
                                    timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("FAILED", result.stdout + result.stderr)
            self.assertFalse(installed.exists(), "unverified native bytes must never be installed")

    def test_build_uses_locked_runner_and_only_production_inputs(self):
        dockerfile = self.read("Dockerfile.maintenance")
        self.assertIn("FROM rust:1.99.1-slim-bookworm", dockerfile)
        self.assertIn("cargo build --release --locked --bin okf-maintenance", dockerfile)
        self.assertNotRegex(dockerfile, r"COPY\s+\.\s")
        # build.rs generates the console asset table (src/http/ui.rs includes it).
        for path in ("Cargo.toml", "Cargo.lock", "build.rs", "src", "migrations"):
            self.assertRegex(dockerfile, rf"(?m)^COPY .*\b{re.escape(path)}\b")
        self.assertNotRegex(dockerfile, r"(?m)^COPY .*\b(?:tests|fixtures|\.superpowers)\b")
        self.assertIn(f"/usr/local/bin/okf-maintenance {RUNNER}", dockerfile)

    def test_runtime_identity_libraries_and_absolute_entrypoint(self):
        runtime = self.read("Dockerfile.maintenance").split("FROM debian:bookworm-slim AS runtime", 1)[1]
        self.assertIn("USER 10001:10001", runtime)
        for library in ("ca-certificates", "libstdc++6", "libgcc-s1"):
            self.assertIn(library, runtime)
        self.assertIn('ENTRYPOINT ["/usr/local/bin/maintenance-entrypoint"]', runtime)
        self.assertIn('CMD ["--help"]', runtime)
        self.assertNotRegex(runtime, r"(?m)^VOLUME|ANTHROPIC_API_KEY=|OKF_MAINTENANCE_TOKEN=")

    def workflow(self):
        return yaml.load(self.read(".github/workflows/maintenance-image.yml"), Loader=yaml.BaseLoader)

    def test_workflow_is_separate_amd64_with_existing_action_pins(self):
        workflow = self.workflow()
        existing = yaml.load(self.read(".github/workflows/image.yml"), Loader=yaml.BaseLoader)
        steps = workflow["jobs"]["image"]["steps"]
        pins = {step["uses"] for step in existing["jobs"]["image"]["steps"] if "uses" in step}
        self.assertEqual({step["uses"] for step in steps if "uses" in step}, pins)
        build = next(step["with"] for step in steps if step.get("uses", "").startswith("docker/build-push-action@"))
        self.assertEqual(build["file"], "Dockerfile.maintenance")
        self.assertEqual(build["platforms"], "linux/amd64")
        self.assertEqual(build["push"], "true")
        self.assertEqual(set(workflow["on"]), {"push", "workflow_dispatch"})
        self.assertEqual(workflow["on"]["push"], {"tags": ["v*"]})

    def test_workflow_never_labels_manual_build_as_release_or_latest(self):
        steps = self.workflow()["jobs"]["image"]["steps"]
        meta = next(step["with"] for step in steps if step.get("id") == "meta")
        self.assertEqual(meta["images"], "ghcr.io/hypotheosis/okf-maintenance")
        self.assertEqual(meta["flavor"].strip(), "latest=false")
        self.assertEqual(meta["tags"].splitlines(), [
            "type=semver,pattern={{version}},enable=${{ github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') }}",
            "type=sha,format=long,enable=${{ github.event_name == 'workflow_dispatch' }}",
        ])
        # metadata-action otherwise synthesizes a version label from the dispatch ref.
        self.assertIn("org.opencontainers.image.version=${{ github.event_name == 'push' && github.ref_name || github.sha }}", meta["labels"])

    def test_entrypoint_preserves_arguments_private_umask_exit_and_exec(self):
        script = self.read("scripts/maintenance-entrypoint.sh")
        self.assertIn(f'exec {RUNNER} "$@"', script)
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            fake = directory / "runner"
            fake.write_text("#!/usr/bin/env python3\nimport json, os, sys\nprint(json.dumps([sys.argv[1:], os.umask(0), os.getpid()]))\nsys.exit(23)\n")
            fake.chmod(0o755)
            entry = directory / "entrypoint"
            entry.write_text(script.replace(RUNNER, str(fake)))
            args = ["run-once", "--config", "path with spaces", "", "*", "$(false)"]
            process = subprocess.Popen(["/bin/sh", str(entry), *args], text=True, stdout=subprocess.PIPE)
            stdout, _ = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 23)
            self.assertEqual(json.loads(stdout), [args, 0o077, process.pid])


@unittest.skipUnless(IMAGE, "set MAINTENANCE_TEST_IMAGE to run real offline container checks")
class Container(unittest.TestCase):
    def run_container(self, *args, entrypoint=None):
        command = ["docker", "run", "--rm", "--network=none", "--read-only",
                   "--cap-drop=ALL", "--security-opt=no-new-privileges",
                   "--tmpfs", "/tmp:rw,nosuid,nodev,size=128m,mode=1777"]
        if entrypoint:
            command += ["--entrypoint", entrypoint]
        result = subprocess.run([*command, IMAGE, *args], text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_image_metadata(self):
        metadata = json.loads(subprocess.check_output(["docker", "image", "inspect", IMAGE], text=True))[0]
        self.assertEqual(metadata["Architecture"], "amd64")
        self.assertEqual(metadata["Config"]["User"], "10001:10001")
        self.assertEqual(metadata["Config"]["Entrypoint"], ["/usr/local/bin/maintenance-entrypoint"])
        self.assertFalse(metadata["Config"].get("Volumes"))
        for value in metadata["Config"]["Env"]:
            self.assertFalse(value.startswith(("ANTHROPIC_API_KEY=", "OKF_MAINTENANCE_TOKEN=")))

    def test_runner_version_and_help(self):
        self.assertRegex(self.run_container("--version"), r"^okf-maintenance \d+\.\d+\.\d+\s*$")
        help_text = self.run_container("--help")
        for command in ("preflight", "run-once", "evaluate"):
            self.assertIn(command, help_text)

    def test_native_exact_version_help_and_digest(self):
        self.assertEqual(self.run_container("--version", entrypoint=CLAUDE).strip(), "2.1.319 (Claude Code)")
        self.assertIn("--output-format", self.run_container("--help", entrypoint=CLAUDE))
        self.assertEqual(self.run_container("-c", f"sha256sum {CLAUDE}", entrypoint="/bin/sh").split()[0], SHA256)

    def test_private_writes_and_root_owned_binaries(self):
        script = '''set -eu
[ "$(id -u):$(id -g)" = 10001:10001 ]
[ -z "${OKF_MAINTENANCE_TOKEN+x}" ] && [ -z "${ANTHROPIC_API_KEY+x}" ]
for binary in /usr/local/bin/okf-maintenance /usr/local/bin/claude-2.1.319 /usr/local/bin/maintenance-entrypoint; do
    [ "$(stat -c '%u:%g:%a' "$binary")" = 0:0:755 ]
done
umask 077
mkdir /tmp/private
: > /tmp/private/state
[ "$(stat -c '%a' /tmp/private)" = 700 ]
[ "$(stat -c '%a' /tmp/private/state)" = 600 ]
! touch /usr/local/bin/forbidden 2>/dev/null
'''
        self.run_container("-c", script, entrypoint="/bin/sh")


if __name__ == "__main__":
    unittest.main()
