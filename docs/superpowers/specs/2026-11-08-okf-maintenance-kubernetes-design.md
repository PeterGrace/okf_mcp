# Kubernetes maintenance runner deployment

## Intent

Prepare an Argo-managed deployment for the separate report-only OKF investigator. Preserve canonical content, durable recovery, explicit upload/billing consent, and the existing human-review/live-smoke gates. Do not disturb the operator's concurrent v0.10.1 server release.

## Approach

Selected scope: dedicated runner image plus argo2025 manifests. An Argo-only CronJob cannot execute the existing server image: it contains neither okf-maintenance nor Claude. Downloading/installing tools at pod startup would add mutable supply-chain and availability dependencies. A separate image with pinned tools is the chosen approach. A systemd host remains possible but does not satisfy the requested Argo deployment.

## Container packaging

- Add Dockerfile.maintenance and a separate maintenance image workflow in okf_mcp. Leave the existing server Dockerfile/workflow and release version untouched.
- Build the existing Rust runner with the repository's pinned Rust version and locked dependencies; ship no server or database credentials.
- Download the exact native Claude 2.1.319 linux-x64 artifact, not its latest-version installer. Pin SHA-256 dabafec82ca1319fa43d8be36a10fc61b1d11821cb98ad5df04ad95e199b8eed from the official version manifest. Verify digest and offline version/help. Source: https://downloads.claude.ai/claude-code-releases/2.1.319/manifest.json.
- Publish the separate ghcr.io/petergrace/okf-maintenance image on future v* tags; disable latest. The initial implementation targets linux/amd64, matching the existing image workflow; explicitly constrain scheduling to amd64. Manual image workflow behavior must not claim a release version for an unrelated commit.
- Runtime UID/GID 10001, root-owned executables at absolute paths, CA certificates and required native libraries, umask 077, writable /tmp only plus the mounted journal. No credentials, automatic installation/update, interactive login, tools, or MCP configuration baked into the image.
- Build/run tests are local and unpaid; --version/help, synthetic MCP and fake model fixtures do not satisfy the live-provider gate.

## Argo resources

Add resources under argo2025/kustomizations/okf-mcp and include them in its existing Kustomization. Leave the server Deployment, database, ingress, and unrelated workloads unchanged.

- Suspended CronJob, proposed schedule 05:15 UTC daily, concurrencyPolicy Forbid, startingDeadlineSeconds 3600, backoffLimit 0, restartPolicy Never, finite retained job history, activeDeadlineSeconds 1860, terminationGracePeriodSeconds 90. This is a hard job cap; image/startup delays consume its slack, and it is not an unconditional graceful-shutdown guarantee. No automatic restart may repeat ambiguous inference.
- The version-pinned runner image reference initially stages version 0.10.2 as a clearly documented future-release prerequisite, not an assertion that it exists. The operator must select an actually published runner version before unsuspending. v0.10.1 remains a server-only release and is not changed by this work.
- Main container non-root UID/GID 10001, read-only root, no privilege escalation, all capabilities dropped, RuntimeDefault seccomp, no service-account token, bounded CPU/memory and ephemeral storage. Proposed memory limit 1Gi is a starting containment limit, not a paid-inference footprint guarantee.
- ConfigMap supplies HTTPS endpoint https://okf-mcp.k.vsix.me/mcp, catalog pg, exact Claude path/version, and state directory. Proposed provider model is claude-sonnet-4-6, unverified by paid execution; it must be reviewed with the provider account before enabling. Daily batch/invocation limits default to 10 with $0.10 per-invocation reservations and $1 per-run stopping threshold, not a hard billing cap or global daily quota. Runner deadline 1740 seconds normally leaves shutdown margin, subject to pod startup time and the hard job cap. Verify the published image digest before activation; a version tag alone is not immutable.
- Both allow_model_upload and acknowledge_inflight_budget_overshoot remain false. require_hard_billing_cap remains false because it is unsupported. Config plus suspended scheduling prevents accidental paid work.
- ExternalSecret follows existing vault-backend ClusterSecretStore conventions. Proposed dedicated Vault path kv/okf-mcp-maintenance holds OKF_MAINTENANCE_TOKEN and ANTHROPIC_API_KEY. No actual secrets are read, minted, provisioned, or committed. Token must be Read + maintenance_report restricted to pg; no canonical Write and no database access.
- A 1Gi ReadWriteOnce PVC uses the existing iscsi-delete storage class. Argo Prune=false/Delete=false annotations prevent ordinary Argo pruning/application deletion of the PVC. They do not protect against manual PVC/namespace deletion: iscsi-delete reclaims storage on deletion. Preserve or back up the volume before destructive operations; do not change the shared StorageClass. RWO and Forbid complement, not replace, the runner's file lock.
- Mount volume at a root-owned trusted ancestor with a UID-10001-owned 0700 journal child; files remain 0600. A narrowly scoped root initialization container may bootstrap the fresh directory with only the capabilities needed for ownership, no credentials/token mount, and code that inspects directory metadata only without traversing journal files. Root still has residual volume privileges; do not claim it cannot access journal contents. It must refuse existing wrong ownership/mode rather than silently taking over or deleting recoverable state. Avoid fsGroup changes that would make journal directories group-writable and violate runner validation. Document this bootstrap exception and admission-policy validation limits.
- Temporary execution directories are on an emptyDir mounted at /tmp. Journal storage must never be emptyDir.

## Enablement and operations

1. Merge/build the runner image through a later release containing this packaging. Confirm its version/digest; do not use or retag the concurrent server release.
2. Deploy manifests suspended, provision the restricted MCP credential/provider key into Vault, and review exact model/account quotas and storage/admission compatibility.
3. Complete independent human corpus review and separately authorize a one-primary sanitized smoke run with batch_size=max_invocations=1 and $0.10 stopping thresholds. Preflight requires both consents but does not upload evidence or invoke a model. Neither a successful preflight nor CronJob exit 0 proves present-day facts or semantic quality.
4. Review evidence, accounting, cancellation/recovery, privacy, and absence of canonical writes. Only then explicitly change the consent flags and unsuspend normal scheduling. Creating a manual Job bypasses CronJob suspension, so it is an explicit paid-action decision too.
5. Preserve the PVC and binding across retries. Credential/endpoint/catalog changes require an intentional state-binding transition; never delete or overwrite an unresolved journal to bypass recovery. No automated credential provisioning or PVC deletion.

## Validation

- Actual maintenance container build; offline executable/version/help checks without credentials or paid inference. Exercise read-only/non-root/tmp/private-persistent-state conditions using synthetic data and tests where feasible.
- Kustomize rendering and Kubernetes schema validation offline, plus focused assertions for suspended/no-consent state, image/tool version alignment, no secret values, persistent private storage, non-root main container, bounded/no-retry lifecycle, and valid secret references.
- Independent review covering supply chain, CronJob lifecycle, permissions, persistent recovery, and unintended activation.
- Run existing project checks covering changed packaging/workflow assets and report real outcomes; do not claim the known pre-existing release-harness fixture failures are fixed.
- No live kubectl apply, secret retrieval, provider inference, image publication, git commit/push, or production rollout in this implementation unless separately requested. Changes stay local in both repositories.

## Non-goals

No changes to investigator behavior, server deployment version, database migrations, provider adapters, canonical content, release recipes, multi-architecture image support, or automatic activation. Do not modify the release-in-progress checkout or user-owned changes.
