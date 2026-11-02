# Design revision: Postgres storage (2026-11-02)

## Changed

- `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`: catalogs move from git
  clones on a PVC to Postgres (CNPG), decided by pgrace before M3 was planned. A new
  "Revision: Postgres storage" section gives the reasons.
- **Removed:** the `git` module, clones, per-session worktrees, `pull --rebase` publish,
  forge credentials (`OKF_GIT_TOKEN`), the Forgejo webhook, the refresh timer, the PVC,
  the `[[catalogs]]` and `[[keys]]` config sections, and the `Git` and `RebaseConflict`
  errors. Nothing in the server is forge-specific.
- **Storage:** `catalogs`, `documents` (exact text plus derived columns and the execute
  bit), `revisions`, `changesets`, `changeset_entries`, `api_keys`, `key_catalogs`.
  Documents keep their exact bytes, so M1's edits and M2's snapshot and analyses work
  unchanged over an in-memory workspace loaded from the rows.
- **Writes:** per-session changesets in the database replace worktrees. Publish checks
  base revisions in one transaction (`Conflict { files }`); `log` and `index` edits are
  replayed onto the current text, as git's `union` merge did. Changesets survive a pod
  restart; `adopt_changeset` picks up a stranded one.
- **Added:** import (directory or tarball; `git archive` for a clean checkout) and
  export (an authenticated route building a tarball on request); `catalog`, `key`,
  `import`, `export` and `migrate` subcommands; `history`, `revert` and
  `adopt_changeset` tools; `allowed_hosts` config for rmcp's DNS-rebinding check.
- **Milestones re-cut:** M3 storage, M4 read-only server, M5 writes, M6 restructuring,
  M7 deployment.
- **Carried issues:** #31 becomes revision-based commit times; #32-#35 are placed in
  M3 and M4. The per-session write lock, `core.symlinks=false` and `.okf-*.tmp`
  handling no longer apply.

## Notes

- Verified while planning M3 (rmcp 3.8.0 probe): rmcp inserts `http::request::Parts`,
  with middleware extensions, into every tool call's context; its default
  `allowed_hosts` is localhost only, so it must be configured behind an ingress.
- Cutover of the existing git catalog and the Claude Code setup is out of scope until
  the server is complete; it is first tested from other harnesses.
