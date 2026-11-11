# Maintenance runner image

## Publication prerequisite

The dedicated runner is **not** part of the concurrent v0.10.1 server release.
It becomes available only after a future packaging release containing
`Dockerfile.maintenance` and `.github/workflows/maintenance-image.yml`.
The staged Argo reference
`r.gfpd.us/ghcrio/hypotheosis/okf-maintenance:0.10.2` is an **unpublished
prerequisite**, not an available image. Do not retag an old server release.

The separate workflow publishes `ghcr.io/hypotheosis/okf-maintenance` for
`linux/amd64` only. Future `v*` tag pushes produce semver tags; manual dispatch
produces only `sha-<full commit SHA>`, even when dispatched from a tag ref.
Neither produces `latest`. Manual OCI version labels identify the commit, not
a release. The existing server image/workflow and version files are unchanged.
Before deployment, select an actually published runner and verify/pin its
immutable digest; a version tag is not immutable. Validate mirror availability
separately. No image publication or deployment is performed by local checks.

## Runtime contract for Argo

| Setting | Value |
| --- | --- |
| Runner | `/usr/local/bin/okf-maintenance` |
| Claude | `/usr/local/bin/claude-2.1.319` |
| Claude expected version output | `2.1.319 (Claude Code)` |
| Entrypoint | `/usr/local/bin/maintenance-entrypoint` |
| Default arguments | `--help` (no inference) |
| Runtime UID:GID | `10001:10001` |
| Executable ownership/mode | root:root, `0755` |
| Entrypoint umask | `077`; `exec` preserves arguments, exit status and signals |
| Architecture | `linux/amd64`; schedule only on amd64 nodes |
| Writable temporary storage | mount bounded writable `/tmp`; `HOME`/`TMPDIR` are `/tmp` |
| Persistent state | separately mount trusted ancestor `/var/lib/okf-maintenance`; private journal child `/var/lib/okf-maintenance/journal` |
| Proposed config path | `/etc/okf-maintenance/config.toml` (read-only mount) |

Read-only root, dropped capabilities and no privilege escalation work for the
executables' offline version/help probes with a writable `/tmp`. Journal storage
must be persistent, not `/tmp`: Task 2 must bootstrap/validate the UID-10001,
mode-0700 child without weakening trusted-ancestor checks or using `fsGroup`.
No journal is created or repaired by the image entrypoint. Runtime CA certificates,
C/C++ runtime libraries and libatomic are installed; no runtime downloader or
installer is needed. Executables use fixed absolute paths.

Native Claude is downloaded from
`https://downloads.claude.ai/claude-code-releases/2.1.319/linux-x64/claude`.
Its SHA-256 is
`dabafec82ca1319fa43d8be36a10fc61b1d11821cb98ad5df04ad95e199b8eed`,
from the [official version manifest](https://downloads.claude.ai/claude-code-releases/2.1.319/manifest.json).
The build verifies bytes before making the artifact executable, then checks exact
version and help without network access. It never executes a latest installer.
Rust uses the existing `1.99.1` toolchain convention and `cargo build --locked`;
only manifests, production `src` (including the runtime MCP contract) and
migrations enter the Rust build stage. Tests/fixtures are not build inputs.

Credentials are supplied **only at runtime through environment references** named
`OKF_MAINTENANCE_TOKEN` and `ANTHROPIC_API_KEY`; neither values nor credentials are
baked into this image. The MCP token must be restricted to Read +
`maintenance_report` on `pg`, without canonical Write/database access.

## Local verification (no provider calls)

Requires Python 3 with PyYAML, ShellCheck and Docker/BuildKit:

```sh
python3 -B -m unittest discover -s scripts -p 'test_maintenance_image.py' -v
shellcheck scripts/maintenance-entrypoint.sh
```

Build `Dockerfile.maintenance` as a local image with `--platform linux/amd64`.
For a private workspace, first copy only existing paths from
`git ls-files --cached --others --exclude-standard -z` into an isolated context,
skipping deleted paths and preserving symlinks. This includes untracked new
packaging files without sending ignored private `.superpowers/` artifacts.
Do not use the dirty release checkout or `git archive HEAD` (which omits new files).
Then run the full packaging/container suite against that local image:

```sh
MAINTENANCE_TEST_IMAGE=okf-maintenance:task1-local \
  python3 -B -m unittest discover -s scripts -p 'test_maintenance_image.py' -v
```

Container tests use `--network=none`, `--read-only`, dropped capabilities and a
bounded writable `/tmp`, with no credential injection. They exercise version,
help, executable digest/ownership and private file modes, not provider inference.
These checks do not validate a paid invocation, persistent-volume bootstrap,
cluster admission/storage behavior, present-day facts or report quality.

## Activation remains a separate decision

Task 2 must keep scheduling suspended and both model-upload/budget consents false.
Before enabling: publish/verify the future image, provision dedicated credentials,
review the unverified proposed model `claude-sonnet-4-6` and account quotas,
validate persistent storage/admission, and complete human corpus review plus a
separately authorized one-item sanitized live smoke. Preflight is not inference
validation. Creating a manual Job bypasses CronJob suspension and also requires
explicit authorization. No paid provider call is part of image verification.
