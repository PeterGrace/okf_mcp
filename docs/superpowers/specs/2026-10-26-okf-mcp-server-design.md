# OKF MCP server: design

- **Date:** 2026-10-26
- **Status:** approved design. M1 (core library) and M2 (analysis) are delivered; see
  "Milestones"
- **Catalog concept:** `/projects/someday/okf-mcp-server.md` in `pg_knowledge_catalog`
- **Replaces:** `tools/okf.py` in every OKF catalog (retired once this server reaches parity
  and has been in real use)

## Goal

A shared HTTP MCP server, running on the home k8s cluster, that gives any agent harness,
including ones with no filesystem, a full read-write interface to one or more OKF
knowledge catalogs. It enforces the catalog rules (server-clock timestamps, byte-preserving
frontmatter edits, secret rejection, lint before publish, pull --rebase before push)
instead of relying on agents following instructions.

## Decisions

| Question | Decision |
|---|---|
| Where it runs | One shared service on the k3s cluster (`kubectl --context default`), deployed by Argo CD |
| Transport | MCP streamable HTTP via `rmcp`, mounted in an `axum` router, stateful sessions |
| Auth | Bearer API keys; config stores only SHA-256 hashes; per-key catalog list and `read`/`write` access |
| Scope | Full read-write: every `okf.py` command plus content editing, search and concept retrieval |
| Implementation | Port everything to Rust. No Python at runtime. `okf.py` is retired afterwards |
| Write grouping | One git worktree per (MCP session, catalog), created on first write; explicit `publish` |
| Stranded work | Accepted: a session that ends with uncommitted edits leaves its worktree on disk, listed by `stranded_sessions` |
| Git | git CLI via `tokio::process` (exact `pull --rebase` semantics, `union` merge driver) |

## Catalog selection

Every tool resolves the catalogs it acts on in this order, stopping at the first that is set:

1. The tool call's `catalog` argument (`catalogs`, a list, for `search`).
2. The connection: the `X-OKF-Catalogs: pg,work` header, or the `?catalogs=pg,work` query
   parameter on the MCP URL. `mcp.json` sets either; Claude Code expands `${OKF_CATALOGS}`
   there, which covers "env var in the harness". The first name is the default write
   target; the whole list is the read set.
3. The API key's `default_catalog`.
4. The server's `default_catalog`.

Every step is limited to the key's allowed catalogs. Naming a catalog the key can't reach,
or one the server doesn't host, is an error. Catalogs are never silently dropped. Write
tools act on exactly one catalog: the explicit `catalog` argument, or the default write
target.

Example `mcp.json` entry:

```json
{
  "mcpServers": {
    "okf": {
      "type": "http",
      "url": "https://okf-mcp.gfd.dev/mcp",
      "headers": {
        "Authorization": "Bearer ${OKF_MCP_KEY}",
        "X-OKF-Catalogs": "${OKF_CATALOGS:-pg}"
      }
    }
  }
}
```

The hostname is illustrative; the real one is chosen at deployment (M6).

## Architecture

### Process and routes

One binary, `okf-mcp`, on tokio with an axum router:

| Route | Purpose |
|---|---|
| `POST/GET/DELETE /mcp` | rmcp `StreamableHttpService` in stateful mode (`Mcp-Session-Id`), behind the auth layer |
| `POST /webhook/forgejo` | Verifies Forgejo's HMAC signature, then fetches the named catalog |
| `GET /healthz` | Liveness and readiness, last fetch per catalog, clock offset |

`tower` middleware: request tracing, timeouts, and the auth layer on `/mcp`.

Subcommands: `serve` (default) and `hash-key`, which generates a random key and prints it
with its SHA-256 for the config.

### Configuration

A TOML file (k8s ConfigMap), overridable with `clap` flags and environment variables:

```toml
listen = "0.0.0.0:8080"
data_dir = "/data"
default_catalog = "pg"
session_idle_timeout = "2h"
refresh_interval = "5m"
time_server = "mosaic-x5-3819907.internal"

[[catalogs]]
name = "pg"
remote = "https://git.gfpd.us/pgrace/pg_knowledge_catalog.git"
branch = "main"

[[keys]]
id = "workstation"
sha256 = "<hex digest>"
catalogs = ["pg"]
access = "write"            # or "read"
default_catalog = "pg"
actor = "claude-code/claude-opus-5-5"
git_name = "Peter Grace"
git_email = "pete.grace@gmail.com"
```

Secrets come only from environment variables backed by a k8s Secret: the Forgejo push
token (`OKF_GIT_TOKEN`) and the webhook secret (`OKF_WEBHOOK_SECRET`). Raw API keys are
never stored or logged; incoming keys are hashed and compared in constant time.

### Storage

A network-backed PVC (workers churn, so node-local storage is not an option) mounted at
`data_dir`:

```
/data/<catalog>/main/                      server's clone; never edited, only fast-forwarded
/data/<catalog>/sessions/<mcp-session-id>/ git worktree, detached at origin/<branch>
```

### In-memory snapshot

Each catalog holds an `RwLock<Arc<Snapshot>>`. A snapshot holds every document's parsed
frontmatter, body, headings and anchors, the link graph (outbound and inbound), and the
search fields. It is rebuilt off the reactor (`spawn_blocking`, `rayon` across documents)
after each fetch or publish, and swapped in atomically. Readers clone the `Arc` and never
wait on git.

A session that has a worktree for a catalog reads that catalog from a snapshot of its
worktree instead, rebuilt lazily after the session's writes, so an agent sees its own
pending edits.

M2 delivered the immutable snapshot itself as `okf::snapshot::Snapshot`
(`Snapshot::build(&dyn Workspace)`): pure and workspace-based, with each document's
kind, text, parsed frontmatter, title and aliases, links, anchors, backlinks, search
fields, and the secret findings of every file. M3's `catalog` owns the
`RwLock<Arc<Snapshot>>`, the rebuilds and the swap.

### Crate layout

A library crate with a thin `main.rs`:

| Module | Responsibility |
|---|---|
| `config` | Load, merge (file, env, flags) and validate configuration |
| `auth` | Bearer-key middleware; inserts a `Principal { key_id, catalogs, access, default_catalog, actor, git identity }` into request extensions |
| `selection` | Resolve the catalog set and write target from tool argument, header/query, key default, server default |
| `okf::yaml` | Port of `okf.py`'s YAML-subset parser and renderer (scalars stay strings) |
| `okf::doc` | Frontmatter/body split; textual, byte-preserving key edits (`set_block`, `append_seq_item`, `delete_block`, `KEY_ORDER` insertion) |
| `okf::markdown` | ATX headings outside fences, GitHub slugs with repeat numbering, HTML anchors, footnotes, link parsing and resolution |
| `okf::ops` | `stamp`, `review`, `question`, `unquestion`, `source`, `log`, `index`, `create`, `edit`, `replace_body`, `set_frontmatter`, `mv`, `split`, `delete` |
| `okf::timestamp` | Port of the `datetime.fromisoformat` subset `okf.py` relies on (CPython's C parser, quirks included), `isoformat()` and `ISO_RE` |
| `okf::snapshot` | The in-memory catalog snapshot (see "In-memory snapshot"); `catalog` in M3 holds it behind `RwLock<Arc<_>>` |
| `okf::analysis` | Links report, `oldest`, `sprawl`, `lint`, secret scan: pure functions over a `Snapshot` and an injected "now". `lint` takes git commit times as an input (`Option<&CommitTimes>`; `None` means git is unavailable), which the `git` module fills in M3 |
| `okf::search` | Ranked search within one snapshot (decision 3 of the M2 plan); hits carry an `Ord` rank key so the `search` tool can merge catalogs |
| `okf::workspace` | `Workspace` trait (read, write atomically, list, exists, rename, remove) with a disk implementation and an in-memory one for tests |
| `git` | git CLI wrapper: fetch, ff-only merge, worktree add/remove, status, add, commit, pull --rebase, push, rebase --abort |
| `catalog` | Per-catalog state: clone, snapshot, refresh, session worktrees, publish, pruning |
| `mcp` | rmcp `ServerHandler` and `#[tool]` definitions; thin delegation to the modules above |
| `clock` | `Utc::now()` source (injectable for tests) and the NTP offset check |

`okf::*` is pure logic over `Workspace` and a clock, so it needs neither git nor disk in
tests.

## Tools

All tools take an optional `catalog` (`search` takes `catalogs`). Results are structured
JSON (rmcp `Json<T>`, `schemars` schemas) plus a short text summary. Concept paths accept
`/infra/time-server.md`, `infra/time-server` or `/infra/time-server`, as `okf.py` does.

### Read tools (need `read`)

| Tool | Parameters | Result |
|---|---|---|
| `list_catalogs` | none | Catalogs the key can reach; this connection's selection; default write target; last fetch time and HEAD per catalog |
| `search` | `query`, `catalogs?`, `type?`, `tags?`, `status?`, `limit?` (default 20) | Ranked hits: catalog, path, title, description, snippet. Every whitespace-separated term must occur (case-insensitive substring) in some field; the tier is the best field holding the whole query, else the best holding a term (fields, best first: title/alias, tag, description, body); deprecated concepts last; ties by path (decision 3 of the M2 plan). The `tags` filter matches whole tags only, while free text matches tags by substring: the tool description must say so |
| `get_concept` | `path`, `links?`, `raw?` | Frontmatter as JSON and the body; with `links`, resolved outbound links (path, title, description, broken flag) and inbound backlinks; with `raw`, the file verbatim |
| `read_document` | `path?` (default `/index.md`) | Raw text of an `index.md`, `log.md` or `AGENTS.md` |
| `open_questions` | none | Every queued `open_questions` entry, grouped by concept, 1-based |
| `oldest` | `n?` (default 5), `open_questions_only?` | Port of `oldest --json` |
| `sprawl` | `path?`, `n?` (default 5) | Port of `sprawl --json` |
| `links_report` | `mentions?` (default true) | Port of `links --json` |
| `lint` | `pending?` | Port of `lint --json`, on main or (with `pending`) on this session's worktree |
| `pending_changes` | none | This session's uncommitted changes in the catalog: changed paths and unified diff |
| `stranded_sessions` | none | Worktrees with uncommitted or unpushed work whose MCP session has ended: id, catalog, last change, changed paths |

### Write tools (need `write`)

Each acts on this session's worktree for the target catalog, creating it on first use.

| Tool | Parameters | Behavior |
|---|---|---|
| `create_concept` | `path`, `frontmatter` (object), `body`, `index_summary?`, `index_section?`, `by?` | Validates the path (`new_concept_path` rules: inside the catalog, not reserved, not machinery, must not exist; warns on a non-kebab-case name). Requires `type`. Writes `generated: { by, at: now }` itself and rejects `generated`, `verified`, `reviewed` in `frontmatter`. Renders frontmatter in `KEY_ORDER`. With `index_summary`, lists it in the folder's `index.md`, creating folder indexes and parent listings as `ensure_index` does |
| `edit_document` | `path`, `old_str`, `new_str`, `replace_all?` | Exact replacement in a concept body or an `index.md`. Without `replace_all`, `old_str` must occur exactly once. Refuses a match that touches frontmatter. Refuses `log.md` and machinery |
| `replace_body` | `path`, `body` | Replaces a concept's body; frontmatter unchanged byte for byte |
| `set_frontmatter` | `path`, `key`, `value` (JSON, or null to delete) | Sets or deletes a descriptive key with a textual edit. Refuses `generated`, `verified`, `reviewed`, `open_questions`, `sources` (dedicated tools) and refuses deleting `type` |
| `stamp` | `path`, `field` (`generated` or `verified`), `by?` | Port of `stamp` |
| `review` | `path`, `outcome`, `note`, `questions?`, `by?` | Port of `review` |
| `question` | `path`, `question` | Port of `question` |
| `unquestion` | `path`, `question` (1-based index or exact text) | Port of `unquestion` |
| `add_source` | `path`, `id`, `resource`, `title?`, `author?` | Port of `source` |
| `log` | `text`, `kind?` (`Update`, `Creation`, `Change`, `Import`) | Port of `log`. The only way to write `log.md` |
| `index` | `path`, `summary`, `section?`, `title?` | Port of `index` |
| `move_concept` | `old`, `new` | Port of `mv`; returns touched files |
| `split_concept` | `path`, `section`, `to`, `type?`, `title?`, `description?`, `stub?`, `sources?`, `by?` | Port of `split`; returns touched files and notes |
| `delete_concept` | `path` | Removes the concept and its listing in its folder's `index.md`. Refuses while any other document links to it, and lists those links |
| `publish` | `message` | See "Publish" below |
| `discard` | `paths?` | Reverts this session's uncommitted changes (all, or the named paths) |

`by` defaults to the key's `actor`. A call may override it (for example `human:pgrace`
when a person really did the verifying); AGENTS.md's honesty rule still applies.

### Rules enforced on every write, before anything reaches disk

- **Secret scan** (port of `SECRET_RULES`, `ASSIGN_RE`, `_literal_secret`) on the new
  file content; any hit refuses the write and reports the rule and line with the value
  masked.
- **Frontmatter still parses** after the edit (as `Doc.save` checks).
- **Paths stay inside the catalog**: normalized, no `..`, no absolute filesystem paths.
- **Machinery is read-only**: `AGENTS.md`, `CLAUDE.md`, `README.md`, `tools/`, `.claude/`,
  `.git*`, `.worktrees/` cannot be written.
- **Timestamps come from the server clock** and no tool accepts one.
- Files are written atomically (temp file in the same directory, then rename).

## Data flow

### Read

1. `auth` hashes the bearer key, looks it up, and inserts a `Principal`; failures are
   HTTP 401 (missing or unknown key) or 403 (no access to `/mcp`).
2. rmcp hands the HTTP request parts (headers, URI) to the tool handler through the
   request context extensions. (To confirm against rmcp 3.8 in M3; if parts are not
   exposed, the auth layer resolves the connection's catalog selection and inserts it into
   the extensions itself.)
3. `selection` resolves the catalog set.
4. For each catalog, the handler takes an `Arc` of the current snapshot, or of the
   session's worktree snapshot if it has one.
5. The work runs in memory (search uses `rayon` across documents, then merges and ranks
   across catalogs).

### Write

1. Auth, `write` access check, single target catalog.
2. `catalog` gets or creates the worktree for (session, catalog): under the catalog's
   lock, `git fetch origin <branch>`, then `git worktree add --detach
   sessions/<id> origin/<branch>`.
3. The `okf::ops` function reads through `Workspace`, computes the new content, runs the
   write rules, and writes atomically.
4. The session's worktree snapshot is marked stale.

### Publish

1. Lint the worktree. Refuse if any **changed** file has errors; errors elsewhere are
   returned as warnings (as `okf.py publish` does). Refuse on unmerged paths or an
   in-progress rebase.
2. Nothing changed and nothing unpushed: return "nothing to publish".
3. `git add -A`, then commit with the key's git identity. The message is the given
   `message`, a blank line, and an `OKF-Actor: <actor>` trailer.
4. `git pull --rebase origin <branch>`, then `git push origin HEAD:<branch>`. On a
   non-fast-forward rejection, pull --rebase and push again, at most twice more.
5. On a rebase conflict: `git rebase --abort`, keep the commit in the worktree, and
   return `RebaseConflict { files }`. Never force.
6. On success: fast-forward `main/`, rebuild the catalog snapshot, and move the worktree
   to the new HEAD so the session can keep working. Return the commit hash and changed
   paths.

### Refresh

A webhook or the `refresh_interval` timer, under the catalog's lock, runs `git fetch` and
`git merge --ff-only` in `main/`, then rebuilds and swaps the snapshot. A failed
fast-forward (which should never happen, since `main/` is never edited) logs at `error`
and keeps serving the old snapshot.

### Session lifecycle

- A worktree is created on the session's first write to a catalog.
- When an MCP session is closed (`DELETE /mcp`) or idles past `session_idle_timeout`,
  each of its worktrees is removed if it has no uncommitted changes and no unpushed
  commits. Otherwise it is kept, logged at `warn`, and listed by `stranded_sessions`.
- A periodic prune removes clean, fully pushed worktrees idle for 24 hours, and runs
  `git worktree prune`.
- Session state is in memory, so a pod restart ends every session. Their worktrees
  persist on the PVC and are treated as stranded (or pruned if clean).

## Errors

One `thiserror` enum, `OkfError`:

| Variant | Meaning |
|---|---|
| `NotFound { path }` | No such concept or document |
| `Forbidden { reason }` | Key lacks access to the catalog or operation |
| `CatalogNotSelected { catalog }` | Named catalog not hosted or not allowed |
| `InvalidPath { path, reason }` | Outside the catalog, reserved, machinery, already exists |
| `FrontmatterParse { path, line, message }` | Existing or edited frontmatter does not parse |
| `SecretDetected { path, line, rule, masked }` | Write refused by the secret scan |
| `EditMismatch { path, matches }` | `old_str` found 0 or more than 1 times |
| `LintFailed { errors, warnings }` | Publish refused; findings as structured lists |
| `RebaseConflict { files }` | Publish aborted its rebase; commit kept locally |
| `Git { command, stderr }` | Any other git failure |
| `Io` | Filesystem errors |

Mapping:

- Auth failures are HTTP 401 or 403 before MCP is involved.
- Domain errors are MCP tool results with `is_error: true` and a structured body, so the
  agent can correct itself.
- Protocol-level JSON-RPC errors are only for malformed calls.

Logging uses `tracing`. Never logged: API keys, the git token, the webhook secret, or
matched secret text.

## Time

All timestamps come from `chrono::Utc::now()` through the `clock` module (formatted
`%Y-%m-%dT%H:%M:%SZ`). At startup and on each refresh, the server queries the GPS stratum 1
time server (`time_server` in config) over NTP and logs at `error` if the offset exceeds
2 seconds. `/healthz` reports the last offset. Lint's future-timestamp check keeps
`okf.py`'s 2-minute slack.

## Testing

- **Unit tests** in `#[cfg(test)]` modules for every function, against the in-memory
  `Workspace` and an injected clock: YAML parsing and rendering edge cases, byte
  preservation, slugs and repeat numbering, footnote handling, `insert_index_bullet`
  placement, `log` heading insertion, secret rules, sprawl scoring, path validation.
- **Property tests** (Hegel): render then parse returns the same values; textual edits
  change only the target key's lines; any frontmatter the server writes parses.
- **Differential parity tests against `okf.py`.** Two corpora are processed by both
  implementations with a fixed clock (`okf.py` run through a shim with a patched
  `now_utc`): a committed synthetic corpus of hand-made edge cases
  (`tests/fixtures/catalog`), and the real catalog, read from the local checkout named
  by `OKF_PARITY_CATALOG` and never copied into this repository (it is private and this
  repository is on GitHub). File-writing commands must produce byte-identical files;
  `lint`, `links`, `oldest` and `sprawl` must produce equal JSON. `okf.py`'s outputs for
  the synthetic corpus are committed as golden files, which ordinary `cargo test`
  checks, so the tests keep working after `okf.py` is retired.
- **Git integration tests** against a temporary bare repository: worktree creation,
  publish, push-race retry, rebase conflict abort, `union` merges of concurrent `log.md`
  lines, session-end cleanup of clean versus dirty worktrees.
- **End-to-end tests** of the HTTP server with rmcp's client: missing, wrong and
  read-only keys; catalog precedence (argument over header over key over server
  default); a full create, link, index, log, publish cycle against a bare remote.

## Deployment

- Multi-stage Dockerfile: Rust build stage, then a slim runtime image with `git` and CA
  certificates. Pushed to Harbor (`r.gfpd.us`), as the cluster's Kyverno policy expects.
- Manifests in Argo CD's source repository (`infrastructure/argo2025`): one-replica
  Deployment (sessions are in memory; a per-catalog lock guards `main/`), network-backed
  PVC (storage class chosen at M6), ConfigMap, Secret, Service, and an ingress-nginx
  Ingress with a cert-manager certificate.
- One Forgejo webhook per catalog, pointing at `/webhook/forgejo`.
- The Forgejo token needs push access to each hosted catalog.

## Milestones

| Milestone | Delivers |
|---|---|
| M1. Core library (delivered) | `okf::yaml`, `okf::doc`, `okf::markdown`, `okf::workspace`, `clock`; the parity test harness ([plan](../plans/2026-10-26-okf-mcp-m1-core-library.md)) |
| M2. Analysis (delivered) | `lint`, links report, `oldest`, `sprawl`, secret scan; snapshot and search; `okf::timestamp` ([plan](../plans/2026-10-27-okf-mcp-m2-analysis.md); PRs #17-#29) |
| M3. Read-only server | axum, rmcp, auth, `selection`, clone and refresh, read tools, `hash-key`. Deployable and useful on its own |
| M4. Writes | Session worktrees, content and bookkeeping write tools, `publish`, `discard`, `pending_changes`, `stranded_sessions` |
| M5. Restructuring | `move_concept`, `split_concept`, `delete_concept` |
| M6. Deployment | Dockerfile, k8s manifests, webhook, NTP check, operator docs |

Per the project's `CLAUDE.md`, each iteration adds a dated note under `changelog/`.

### What M2 shipped differently

The M2 plan's "Simplifications and deviations relative to the design" has the full list;
the ones that change this design:

- The snapshot is `okf::snapshot`, not part of `catalog` (see "In-memory snapshot"), and
  search is its own module, `okf::search`.
- The secret scan runs on every file once, inside `Snapshot::build`, so `lint` reads the
  findings rather than walking the catalog again. `tools/okf.py` is skipped, as `okf.py`
  skips itself.
- `lint` takes commit times as an input; M2 has no git code.
- Parity compares parsed JSON values, not bytes: `json.dumps` escapes non-ASCII and
  serde_json writes it raw. Key order is checked separately.
- Known divergences from `okf.py`, kept on purpose and documented in the module that has
  each: a list or mapping `status` is reported with its `repr` (`okf.py`'s lint crashes);
  a naive timestamp in `oldest` is ignored, as is a fallback `at:` value that is not a
  date (`okf.py` crashes on both); ISO week dates and fractional-second offsets are
  rejected; a link escaping the catalog root is always broken; symlinks follow M1's
  listing contract, and non-UTF-8 file names are skipped; `resolve_concept` only finds
  snapshot documents (not a file in a dot-directory); the root `AGENTS.md`, `CLAUDE.md`
  and `README.md` are in the snapshot but outside the link graph. Sprawl scores match
  `okf.py` on Python 3.12 or later, whose float `sum()` is compensated.

### Carried into M3

Items raised in M2 reviews that belong to M3's tool layer:

- **`search` tool description** (PR #24): say that the `tags` filter matches whole tags
  only, while free text matches tags by substring.
- **Commit times from git** (PR #27): key `CommitTimes` by catalog-relative paths. Git
  prints paths relative to the repository root, so strip the catalog's prefix when the
  catalog is not the repository root, or `generated-after-commit` silently stops firing.
  Documented on `CommitTimes::new`.
- **Mention matcher** (PR #23): `links_report` builds its unlinked-mention matcher on
  every call; cache one per snapshot once snapshots are long-lived.
- **`MemWorkspace` in integration tests** (PR #29): it is compiled only for unit tests or
  with the `test-util` feature, so an integration test that needs it must enable
  `test-util`.
- The deferred items in the M2 plan's "Deliberately deferred" table: the git module that
  fills `CommitTimes`, snapshot swapping and rebuilds, cross-catalog search merge, the
  per-session write lock and `core.symlinks=false` clones (M3); secret refusal on write,
  the no-frontmatter `set_block` error and `.okf-*.tmp` handling before `git add -A`
  (M4).

## Out of scope

Follow-ups once the server is in use:

- Moving the global `~/.claude/CLAUDE.md`, the hooks, the `okf-*` skills and AGENTS.md
  from `okf.py` to the MCP tools.
- Removing `okf.py` from the catalog and the catalog template. This waits until the parity
  tests pass and the server has seen real use.
- MCP resources and prompts (tools only for now).
