# Kubernetes Maintenance Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare a dedicated runner image and a safely suspended Argo-managed maintenance CronJob.

**Architecture:** A separate OCI image contains the existing runner and checksum-pinned native Claude. Argo resources provide bounded scheduling, persistent private recovery state, and credential references without enabling paid work. The operator's concurrent release checkout remains untouched.

**Tech Stack:** Docker/BuildKit, existing Rust binary, GitHub Actions, Kubernetes CronJob/PVC/ConfigMap/ExternalSecret, Kustomize, offline schema/container tests.

**Spec:** `docs/superpowers/specs/2026-11-08-okf-maintenance-kubernetes-design.md`

## Global Constraints

- Work only in the current isolated okf_mcp worktree and `/home/pgrace/repos/argo2025`; do not modify `/home/pgrace/repos/okf_mcp` main/release state.
- No commits, branches, pushes, image publication, live apply, secrets retrieval/provisioning, paid inference, or automatic activation. No catalog/memory/frozen-checkout access.
- Native Claude 2.1.319 linux-x64 SHA-256 `dabafec82ca1319fa43d8be36a10fc61b1d11821cb98ad5df04ad95e199b8eed`; verify downloaded bytes and executable version/help. Main runtime UID/GID 10001, private umask 077.
- CronJob suspended; both consent flags false; exact provider ID `claude-sonnet-4-6` is a proposed, unverified default. Future image `r.gfpd.us/ghcrio/petergrace/okf-maintenance:0.10.2` is an explicit publication prerequisite, not a ready artifact.
- Schedule `15 5 * * *`, UTC, Forbid overlap, backoffLimit 0, restartPolicy Never, startingDeadlineSeconds 3600, job cap 1860, grace 90, runner max 1740 seconds.
- PVC 1Gi RWO, iscsi-delete, protected from ordinary Argo prune/delete but not manual/namespace deletion. Strict 0700 UID-10001 journal child under trusted root-owned ancestor; no fsGroup workaround, recursive chown, or deletion of state.
- ExternalSecret path `kv/okf-mcp-maintenance`, store `vault-backend`, only env names `OKF_MAINTENANCE_TOKEN` and `ANTHROPIC_API_KEY`. No real values.
- Existing runtime/DB/permissions/canonical behavior, server Dockerfile/workflow, and release recipes unchanged. Record pre-existing failures honestly.

## Review Focus

- A checksum/version/architecture mismatch must fail image validation, never silently download latest.
- Missing publication or credentials must not accidentally activate a CronJob or paid model call.
- Existing wrong-owned/mode/symlinked journal directories must fail bootstrap without takeover or content changes.
- SIGTERM/hard job deadlines/restarts must preserve the persistent journal and must not enable automatic inference retries.
- Tag/manual workflow events must not publish latest or label an unrelated source as a stable release; suspended manually-created Jobs still require explicit authorization.

---

### Task 1: Dedicated image and publication path

**Files:** Create `Dockerfile.maintenance`, `.github/workflows/maintenance-image.yml`, `scripts/maintenance-entrypoint.sh`, `scripts/test_maintenance_image.py`; add focused operator guidance under `docs/maintenance-kubernetes.md`.

**Interfaces:** Image executable `/usr/local/bin/okf-maintenance`; native Claude `/usr/local/bin/claude-2.1.319`; default entrypoint runs the CLI with umask 077, no installation. Image repository `ghcr.io/petergrace/okf-maintenance`. amd64 only. `/tmp` and externally mounted state are the only required writable locations.

- [ ] Write focused tests asserting fixed SHA/version, no latest-installer execution, fixed absolute binaries, non-root/runtime/read-only compatibility, version-tag versus manual-SHA workflow metadata, and safe entrypoint argument forwarding.
- [ ] Run the tests before creating the packaging and record the expected missing-file failure.
- [ ] Implement the Dockerfile using existing Rust/Debian conventions, locked runner build, exact native download with SHA verification, required CA/native libraries, and non-root runtime. Add the separate workflow using existing pinned actions: future v* pushes and workflow_dispatch; semver only for tag events, SHA for manual, never latest. Do not publish now.
- [ ] Run static tests and ShellCheck; build actual local image and run offline version/help with no credentials, read-only root and writable temporary storage. No provider/model calls.
- [ ] Document prerequisites and the fact v0.10.1 does not gain this image retroactively. Hand off exact paths/security identity to Task 2. Do not commit.

### Task 2: Argo configuration, recovery storage, and suspended scheduling

**Files:** Under `/home/pgrace/repos/argo2025/kustomizations/okf-mcp`, create `maintenance-cronjob.yaml`, `maintenance-configmap.yaml`, `maintenance-pvc.yaml`, `maintenance-externalsecret.yaml`, `README.maintenance.md`; modify existing `kustomization.yaml`. Add focused local validation test/script for these resources without modifying unrelated repository-wide policies.

**Interfaces:** Consume Task 1 image/paths/UID. Config path `/etc/okf-maintenance/config.toml`; journal `/var/lib/okf-maintenance/journal`; persistent mount ancestor `/var/lib/okf-maintenance`; endpoint `https://okf-mcp.example.com/mcp`, catalog `pg`. ConfigMap/Secret/PVC resources named `okf-maintenance-config`, `okf-maintenance-credentials`, `okf-maintenance-state`. CronJob `okf-maintenance`.

- [ ] Write offline rendered-manifest assertions for all global limits, false consents, suspension, named credential references (not values), protected persistent storage, no service-account token/fsGroup, non-root/read-only main, temporary writable /tmp, version alignment, and unchanged server resources.
- [ ] Run before manifests exist and record missing-resource failure.
- [ ] Implement resources following existing YAML/ExternalSecret/storage conventions. Proposed main resources: requests CPU 100m/memory 256Mi, limits CPU 1/memory 1Gi/ephemeral storage 512Mi; bounded /tmp emptyDir. Configuration defaults batch/invocations 10, reservation 100000 microUSD, run 1000000 microUSD, observed token stop 100000. Model/version/consent remain as spec.
- [ ] Implement a tightly scoped root init command with no credentials/token: bootstrap only an absent private child; validate existing directory metadata without traversing journal files, reject symlinks/wrong UID/mode, never recursively change ownership or repair existing journal state. Root still has residual volume privileges, documented. Validate the fresh mount assumptions; do not weaken runner ancestry checks.
- [ ] Render with Kustomize and validate standard schemas offline with kubeconform; explicitly handle the existing ExternalSecret CRD rather than falsely claiming core-schema coverage. Run focused assertions, ShellCheck for extracted shell, and container metadata/bootstrap tests for fresh, existing-correct, wrong UID/mode and symlink states.
- [ ] Document immutable digest/image publication, credentials/account quotas, cluster storage/admission validation, human review and separately authorized one-item smoke, explicit later consent/unsuspension, state binding/rotation, and PVC retention limits. Do not apply or commit.

### Task 3: Whole-change review and final verification

**Files:** Only correct scoped issues in prior task files; private evidence under `.superpowers/pr178-review/maintenance-k8s-*`.

**Interfaces:** Both repositories must render/build against the shared image/path/UID/config contract; neither is deployed or published by this task.

- [ ] Fresh independent reviewer assesses combined image/manifests and tests, especially five Review Focus conditions. Fix concrete blocking findings and run one scoped re-review.
- [ ] Parent independently runs new tests, actual local container checks, Kustomize/kubeconform, `just fmt`, `just lint`, and `RUST_TEST_THREADS=4 just test`. Report pre-existing release-fixture failures separately; do not fix them here.
- [ ] Verify the release-in-progress checkout is not modified by this work, both changesets contain no credentials or activation, and no live/remote actions occurred. Report exact files, verification, staged future image/model assumptions, remaining activation gates, and local/uncommitted status. Do not run a release or push Argo main.
