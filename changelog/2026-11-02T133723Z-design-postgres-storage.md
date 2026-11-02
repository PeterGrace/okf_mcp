# Design revision: Postgres storage (2026-11-02)

## Changed

- `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`: catalogs move from git
  clones on a PVC to Postgres (CNPG), decided by pgrace before M3 was planned. A new
  "Revision: Postgres storage" section gives the reasons.
- **Removed:** the `git` module, clones, per-session worktrees, `pull --rebase` publish,
  forge credentials (`OKF_GIT_TOKEN`), the Forgejo webhook, the refresh timer, the PVC,
  the `[[catalogs]]` and `[[keys]]` config sections, and the `Git` and `RebaseConflict`
  errors. Nothing in the server is forge-specific.
- **Storage:** `catalogs`, `documents` (exact bytes as `bytea`, derived columns, the
  execute bit, and tombstones so revision numbers are never reused), `revisions`, `changesets`, `changeset_entries`, `api_keys`, `key_catalogs`.
  Documents keep their exact bytes, so M1's edits and M2's snapshot and analyses work
  unchanged over an in-memory workspace loaded from the rows.
- **Writes:** per-session changesets in the database replace worktrees. Publish checks
  base revisions in one transaction (`Conflict { files }`); `log` and `index` edits are
  replayed onto the current text, as git's `union` merge did. That covers every
  bookkeeping change to an `index.md` (`index`, `create_concept`'s listing, and the
  listing changes of move, split and delete, including a new folder's `index.md` that
  another session created first); a free-text `index.md` edit is not replayable.
- **Publish lock:** publishes and imports of a catalog are serialized (an in-process
  mutex plus a Postgres advisory lock), so each publish checks against the current
  state: base revisions, replay, the write rules and lint on the result, and links into
  paths it deletes or moves (another session may have published a link meanwhile). A
  `generation` counter lets the server notice a CLI import. Unique constraints on
  `revisions`, open `changesets` per session, and `api_keys.sha256` are the backstop. Changesets survive a pod
  restart; `adopt_changeset` picks up a stranded one.
- **Added:** import (directory or tarball; `git archive` for a clean checkout) and
  export (an authenticated route building a tarball on request, with no top-level
  directory so an export imports back as it was); `catalog`, `key`,
  `import`, `export` and `migrate` subcommands; `history`, `revert` and
  `adopt_changeset` tools; `allowed_hosts` config for rmcp's DNS-rebinding check.
- **Outside changes:** `admin_generation` and each catalog's `generation` are polled
  every `poll_interval` (10 s), so a CLI key revoke, grant, catalog create or delete,
  or import reaches the running server within that bound.
- **Import rules:** a tarball with links, devices, `..`, absolute, non-UTF-8,
  non-normalized or duplicate paths is refused; `max_import_bytes` caps the
  uncompressed total. `catalog delete` refuses while a non-empty changeset is open,
  unless `--force`, and cascades. Non-empty stranded changesets are never pruned.
- **Access:** `read` < `write` < `admin`, always within the key's granted catalogs;
  creating and deleting catalogs is subcommand-only. `adopt_changeset` matches the key,
  not the actor string; `revert` is subject to the machinery rule.
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
