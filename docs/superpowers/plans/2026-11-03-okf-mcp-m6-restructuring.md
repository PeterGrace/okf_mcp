# OKF MCP Server, Milestone 6 (Restructuring): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Let agents restructure a catalog through the server: `move_concept`, `split_concept` and `delete_concept`. The work covers:
- Rust ports of `okf.py`'s `mv` and `split` and their helpers (`rewrite_links`, the listing moves), byte for byte against `okf.py`, plus a server-only `delete_concept`;
- two new replay operations, so the `index.md` and `log.md` edits these tools make are redone on newer text at publish, as M5's `ListBullet` is;
- the three MCP tools on M5's `write::apply`, with `discard`, `revert` and `pending_changes` shown to handle the deletions and renames they produce;
- publish and end-to-end tests of moves, splits and deletes racing other sessions.

**Architecture:**
- **Edit logic (`okf::ops`):**
  - `relink` holds `rewrite_links`, `okf.py`'s link rewriter, split into a per-file pure function that the op and the replay share;
  - `moves`, `split` and `delete` hold the three ops. Each is a pure function over an in-memory workspace. It returns its touched paths and notes, and records replay ops for its `index.md` and `log.md` edits.
- **Replay (`okf::ops::replay`):** `ReplayOp` gains:
  - `Unlist`, which removes the bullet listing a path;
  - `Relink`, which redirects the links into a moved document or section.

  Publish needs no new code to use them: `store::publish::merge` already replays any stored ops.
- **Write path and publish (M5, unchanged):**
  - a removed path is a deletion entry (M5's `write::new_entry`);
  - a free edit conflicts on a stale base;
  - `links_into_deleted` refuses a link into a deleted (moved-away) path that someone published meanwhile.
- **Tools (`mcp::restructure`):** a `restructure_router` block joined into `OkfServer::tools()`.

**Tech Stack:**
- Rust 2021 (toolchain 1.99.1).
- No new dependencies. Everything M1-M5 use: `serde` 1.0.229, `serde_json` 1.0.154, `regex` 1.13.1, `indexmap` 2.14.2, `schemars` 1.2.3, `rmcp` 3.8.0, `sqlx` 0.9.0, `tokio`, Hegel (`hegeltest` 0.50.3), testcontainers for Postgres.

---

## Roadmap position

Milestone 6 of 7 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones"). M1-M5 are on `main` (`299b2ea`):
- M1, core library;
- M2, analysis;
- M3, storage;
- M4, read-only server;
- M5, writes (PRs #104-#117).

M7 deploys.

## Scope

In scope: the design's "Milestones", M6 row (`move_concept`, `split_concept`, `delete_concept`), and the sections it rests on:
- "Write tools (need `write`)": the three rows;
- "Rules enforced on every write";
- "Data flow": Write step 4 (the listing changes of the three tools as replay operations) and Publish step 4 ("links into deleted or moved paths");
- "Errors";
- "Testing";
- the M5 milestone changelog's "Follow-ups" for M6.

That means:
- **`okf::ops::relink`:** `rewrite_links` (`okf.py:1343`) with `os.path.relpath`'s port (`paths::relpath`).
- **`okf::ops::replay`:** `ReplayOp::Unlist` and `ReplayOp::Relink`, their pure text edits, and their replay (Decision 1).
- **`okf::ops::moves`:** `move_concept`, `cmd_mv`'s port (`okf.py:1524`), with `MemWorkspace::rename`.
- **`okf::ops::split`:** `split_concept`, `cmd_split`'s port (`okf.py:1589`), with `find_section`, the footnote reference set (`FOOTREF_RE`) and heading re-levelling.
- **`okf::ops::delete`:** `delete_concept` (design only), with `OkfError::StillLinked` (Decision 4).
- **Parity:** the shim's `ops` command, `tests/golden/ops.json` and `live_ops_parity` cover `mv` and `split` (Decision 3).
- **Tools:** `move_concept`, `split_concept` and `delete_concept` in `restructure_router`. `discard`, `revert` and `pending_changes` are tested on a move and a delete.
- **Tests:**
  - publish database tests: replay of the new ops, the conflict on free edits, revisions and tombstones of moved, split and deleted paths, `history` of both paths;
  - end-to-end over HTTP in `tests/restructure.rs`.
- **Wrap-up:** the M6 smoke test, README, design update and milestone changelog.

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| Pool limits: writers queued on one changeset's row lock each hold a pooled connection; a publish needs a second to refresh a stale snapshot | things-to-fix (after M6) | Decision 2. Accepted in M5 (Tasks 8, 10) |
| The overlay cache's size cap | things-to-fix | Decision 2. M5 Task 7 review |
| `write::apply`'s `Busy` branch for a closed changeset (no test reaches it) | things-to-fix | Decision 2. M5 Task 10 |
| A test for `create_concept`'s `parse_checked` guard | things-to-fix | Decision 2. M5 Task 4 review |
| An `okf-mcp reindex` backfill of derived columns | When `Derived::of` changes | Decision 5. M6 does not change it |
| `history` following a rename (one history across old and new path) | not planned | Decision 16 |
| NTP check, `time_server`, TLS to Postgres, deployment | M7 | Design milestone table |
| More than one replica | never | Design "Out of scope" |

## Decisions taken while planning (2026-11-03)

Decisions 1, 2 and 5 are pgrace's or M5's. Decisions 3 and 4 were given by pgrace with their details left to Claude. The rest are Claude's, each marked **(decided by Claude)**: pgrace asked for the most conservative option wherever the design leaves a question open, decided without stopping to ask. pgrace has since confirmed Decision 7.

1. **Publish semantics: hybrid (pgrace, 2026-11-03).**
   - **New replay ops.** Index-listing changes made by move, split and delete become new `ReplayOp` variants that rebase at publish like M5's `ListBullet`:
     - `ReplayOp::Relink { redirect }` relinks the links into a moved document, or a split-out section, in an `index.md`: the listing bullet of a concept renamed within its folder, and any other link there.
     - `ReplayOp::Unlist { target, bullet }` removes the bullet listing a path from its folder's `index.md`: the old folder's listing when a concept moves to another folder, and a deleted concept's listing.

     A move to another folder is `Unlist` on the old folder's index plus M5's `ListBullet` on the new one. Replay is per path, so one operation cannot span two files. Decision 6 gives the ops' shapes.
   - **Free edits.** These are free edits, so a concurrent change to any of them is a `Conflict` at publish:
     - link rewrites in other documents;
     - the moved or split concept itself;
     - the parent of a split.

     "Other documents" means documents other than `index.md` and `log.md` (Decision 7).
   - **A link to the old path added meanwhile is refused.** The check is publish's `links_into_deleted` (`src/store/publish.rs:439`), and it needs no change:
     - `check` (`:363`) collects every entry without content into `deleted` (`:370`);
     - a move's old path is such an entry: the op removes it, and `write::apply` stores a removed path that is published as a deletion (`new_entry`, `src/write.rs:215`);
     - `split_lint` (`:419`) adds `links_into_deleted(overlay.snapshot(), changed, deleted)` to the errors (`:431`);
     - that function (`:439-470`) reports each link from a document outside the changeset (`!changed.contains(doc.path())`) into a deleted path as a `link-into-deleted` error in `LintFailed`.

     The snapshot is the overlay built on the current published state (`check` builds it from `current_base`), so it holds a document someone else published after this changeset's write. A link from a document this changeset did change is lint's ordinary `broken-link` error in a changed file, which refuses too.
   - **How a bullet replay on an `index.md` meets M5's `merged_replay` rule** (`src/write.rs:321`). An entry stays replayable only while every edit the changeset made to that path came with operations. One free edit sets its replay to `None` for good.
     - M6's ops record every edit they make to an `index.md` or `log.md` as an operation (`ListBullet`, `CreateIndex`, `Unlist`, `Relink`). So a move, split or delete never turns those files into free edits. Their operations are appended to whatever the entry already holds, for example the `ListBullet` of an earlier `index` call, and all of them replay in order.
     - If the changeset already made a free edit to that `index.md` (an `edit_document`, a `revert`), the entry has `replay: None`. A later move keeps it `None` (`merged_replay` returns `None` when the previous entry's replay is `None`), so that index conflicts on a stale base like any free edit. Its text in the changeset still holds the move's change, and only the rebase is lost.
     - Likewise a free edit after the move clears the replay the move recorded.
     - Within one op, a path is either recorded with operations or touched as a free edit, never both. `OpOutcome::record_rewrites` (Task 2) sends `index.md` and `log.md` to `record` and every other file to `touch`. The concept files the ops write are only ever touched.
2. **Scope: restructuring only (pgrace, 2026-11-03).** M5's accepted gaps stay out of M6 and in things-to-fix: the pool limits, the overlay cache cap, the `Busy` branch and the `parse_checked` test. They are listed in the deferred table.
3. **Byte parity with `okf.py` for move and split (Claude, following M5 Decision 7).**
   - The shim's `ops` command, `tests/golden/ops.json` and `live_ops_parity` gain `mv` and `split` steps (Tasks 3 and 4). For each step, the comparison covers:
     - the files the step changes, byte for byte;
     - its output, including the touched list. `okf.py`'s `print_touched` block is rebuilt from `OpOutcome::touched` by a port of `print_touched` in `tests/parity.rs` (Decision 12);
     - its notes, which come back as the tool's `notes`. These include `split`'s stderr notes: the last-section warning, the missing-description note, `new_concept_path`'s naming note and `ensure_index`'s "created" lines.
   - **The shim can run `okf.py mv` without git.** Verified on 2026-11-03 with the 52-step script below on the synthetic corpus. `cmd_mv` calls `git ls-files --error-unmatch OLD` and uses `git mv` only when that succeeds, else `os.rename` (`okf.py:1556-1561`). The shim works on a fresh copy under `tempfile.TemporaryDirectory()`, which is not a git work tree, so `git -C <copy>` exits 128 and `cmd_mv` renames.
   - The shim still stubs `okf.git` in its `ops` command, to a `CompletedProcess` with return code 1, which is the "not tracked" answer. That way an `ops` run needs no `git` binary, and it can never consult an enclosing repository if `TMPDIR` points into one. The stubbed and unstubbed runs gave byte-identical JSON. (decided by Claude)
4. **`delete_concept` (design; details decided by Claude).**
   - **Linked concepts are refused.** It refuses while any other document links to the concept, with `OkfError::StillLinked { path, links }` (Decision 8). Each link is a `LinkSource { source, line }`, one per link occurrence, in walk order and then line order.
   - **What counts as a link.** It reads the documents publish's link check reads: every markdown file in the walk except the root `AGENTS.md`, `CLAUDE.md` and `README.md` (`SnapshotDoc::in_link_graph`, `src/okf/snapshot.rs:217`).
     - Links from `index.md` files count, other than the folder's own listing. That listing is the first bullet of the folder's `index.md` that links the concept, the one `delete_concept` removes. A second bullet, or prose in that index, counts as a link.
     - **Links from `log.md` count** (decided by Claude). `log.md` is in the link graph, so publish's `links_into_deleted` and lint's `broken-link` would refuse the publish anyway; refusing at write time says so earlier, with the lines. Plain-text mentions (the links report's unlinked mentions) never count. Neither do a concept's links to itself.
   - **The listing goes through a replay op.** It removes the folder's listing bullet with `ReplayOp::Unlist { target, bullet: None }`. `None` means "whatever the bullet reads now", since the bullet is deleted either way.
   - **The concept goes through `MemWorkspace::remove`.** It removes the concept with `MemWorkspace::remove` and reports it as touched, so `write::apply` stores a deletion entry under M5's rule: a published path, or one published at the entry's base, becomes a deletion; a path the changeset created and never published leaves no entry.
   - **It never deletes an `index.md`, even an emptied one.** When the folder holds nothing but its `index.md` afterwards, it adds the note `note: /<folder>/ now holds only its index.md, which stays; delete_concept never deletes an index`.
   - **It refuses machinery and non-concepts.** `index.md`, `log.md`, `AGENTS.md`, `CLAUDE.md` and `README.md` get `InvalidPath` "is not a concept document" from `resolve_concept`. A concept-named file in machinery (`install/x.md`) gets `InvalidPath` from `rules::check_path`, which the op calls on the path `resolve_concept` returns, before it reads or changes anything. A machinery reference that names no file is `NotFound`, as for any missing concept.
5. **No `reindex` (M5 Decision 5 still holds).** M6 does not change `Derived::of` (`src/store/documents.rs:106`) or what it reads. Publish computes the derived columns of every document it writes (`write_all`, `src/store/publish.rs:476`), the moved and split-out concepts included. Verified: no task's file list includes `src/store/documents.rs` or `src/okf/snapshot.rs`.
6. **The shapes of the two replay ops.** (decided by Claude)
   - **`Relink { redirect: Redirect }`.** `Redirect { from, to, anchors }` is `rewrite_links`' `fn` as data:
     - `anchors: None` redirects every link into `from`, keeping its fragment (`mv`);
     - `anchors: Some(map)` redirects only links whose fragment, percent-decoded and lower-cased (`okf.py`'s `norm_fragment`), is a key, giving them the mapped fragment (`split`'s `amap`).

     The op and the replay call the same per-file function (`relink::rewrite_text`), so they cannot drift apart. That is M5's rule for `log_text` and `insert_index_bullet_text`.
   - **`Unlist { target, bullet: Option<String> }`.** It removes the first bullet (`* ` or `- `) linking `target`, as `cmd_mv` takes it. When `bullet` is `Some`, the replay refuses with `Refused` if the bullet listing `target` now reads otherwise. `move_concept` records `Some`, because it carries the old bullet's text to the new folder: if another session changed that bullet meanwhile (its summary, say), dropping it silently would lose that change. Refusing is the conservative option, and the agent re-reads and moves again. `delete_concept` records `None`.
   - **Replay is idempotent:** a missing listing is left alone, and a text with no link into `from` is left alone.
7. **Link rewrites in `log.md` are `Relink` replay ops too, like those in any `index.md`.** (decided by Claude; confirmed by pgrace) `rewrite_links` rewrites `log.md` (only the root's `AGENTS.md`, `CLAUDE.md` and `README.md` are skipped, `okf.py:1356`). A free edit to `log.md` would make every concurrent `log` call by another session a `Conflict` for the mover. `log.md` is bookkeeping whose entries already replay, and `Relink` replays idempotently on top of the `Log` ops (Task 2 tests the pair). This reads Decision 1's "link rewrites in other documents ... are free edits" as "other than `index.md` and `log.md`". **Confirmed by pgrace, 2026-11-03** ("keep the log.md replay").
8. **`delete_concept`'s refusal is a new error variant, `StillLinked { path, links: Vec<LinkSource> }`, HTTP 409** (decided by Claude), so an agent gets the links as data rather than inside a message. It is not internal. `kind`, `to_json` (`path`, `links`) and `AppError::status` gain arms (Task 5).
9. **Ops take `&MemWorkspace` or `&dyn Workspace`, not `&mut MemWorkspace`.** (decided by Claude) The workspace has interior mutability: `write_atomic` and M5's `remove` take `&self`, and `write::apply` hands an op `&MemWorkspace`. `move_concept` and `delete_concept` need `MemWorkspace` (`rename`, `remove`); `split_concept` needs only `&dyn Workspace`, like `create_concept`.
10. **`MemWorkspace::rename` keeps the file's bytes and execute bit** (decided by Claude), as `git mv` keeps the mode. It refuses a taken target, a target that is a directory and a target below a file, with `write_atomic`'s reasons, through one shared helper.
11. **`move_concept` and `split_concept` take `okf.py`'s exact texts, CLI wording included** (decided by Claude): "no --description given", "named with --source", "pass the anchor slug instead". M5 kept "pass --title" the same way.
12. **The op's message is `okf.py`'s stdout without `print_touched`'s block** (decided by Claude): the tool returns `touched` itself (`WriteReport::touched`). The parity harness appends the block, built from `OpOutcome::touched`, so the touched list is still compared with `okf.py`'s.
13. **The parity script leaves out two refusals** (decided by Claude):
    - a missing concept: `okf.py` prints the temporary root (`no concept 'x' in /tmp/...`), which the shim cannot strip;
    - a taken target: `okf.py` prints `X already exists`, M1's `InvalidPath` prints `X: already exists`.

    Unit tests cover both. The script does include the reserved-name refusal, which both word `moves/index.md: index.md is a reserved name, not a concept`.
14. **Divergences from `okf.py`, all where `okf.py` crashes** (decided by Claude):
    - `mv` of a concept whose frontmatter does not parse, when it has no old listing to carry, is `FrontmatterParse`; `okf.py` raises `YamlError`;
    - a `split` parent whose `status` is a list or mapping is written with Python's `str()` of it; `okf.py`'s `scalar` raises `TypeError`;
    - a missing root `index.md` under a root-level target is `NotFound`; `okf.py` raises `FileNotFoundError`.
    - an `mv` target below an existing file is `InvalidPath` "a parent component is a file", refused before anything is written; `okf.py`'s `os.makedirs` raises `FileExistsError` (added by issue #122's review).

    Each module's doc lists its divergences.
15. **The new folder's listing is inserted without a "lists already" check, as `okf.py` does** (decided by Claude). The `ListBullet` replay op it records skips an index that already links the new path. The two differ only for an index holding a link to a path that does not exist yet.
16. **`history` does not follow renames** (decided by Claude). Each path has its own history. After a move, the old path's newest revision is `delete`, and the new path's history starts with `create`; both carry the publish message. That is the existing revisions table, with no schema change.
17. **A move or delete is several entries, and `discard` may drop some of them** (decided by Claude). There is no grouping. If an agent drops part of a move, publish's lint finds what is left broken: `broken-link` errors in changed files, or `link-into-deleted`. `revert` of a moved-away path to its last revision puts it back beside the new one. Task 6 tests `revert` and a whole-changeset `discard`. Task 7 tests a partial one: discarding only a linking file's rewrite gets a `link-into-deleted` refusal. Discarding only the deletion entry breaks nothing, because the old path stays published beside the new one.
18. **A link to a split-out section, published meanwhile, is not refused** (decided by Claude). Split moves no path. Such a link is a `broken-anchor` finding in a file the publish does not change, which publish returns as a warning, as for any lint error elsewhere (`split_lint`). A new check would be new publish semantics, so M6 adds none.
19. **Moving into or out of machinery is refused by the write rules** (decided by Claude):
    - `new_concept_path` refuses a machinery or dot-directory target;
    - an old path in machinery is refused by `write::apply`'s `rules::check_path` on the touched paths. Nothing is stored, since the op runs on a scratch copy.

## Simplifications and deviations relative to the design

- **The design's "rename or remove an `index.md` bullet" (Write step 4) is `Relink` and `Unlist`**, and a move across folders is `Unlist` plus `ListBullet` (Decision 1). Task 2 names them in the design.
- **`delete_concept`'s refusal is `StillLinked`, a new `OkfError` variant** (Decision 8). Task 5 adds it to the design's "Errors".
- **`split_concept`'s `type` defaults to `Note`, `okf.py split`'s `--type` default**, and its `sources` is `okf.py`'s repeatable `--source`. Task 6 says so in the design's row.
- **Link rewrites in `log.md` replay** (Decision 7).
- **`history` stays per path** (Decision 16).

## Verified facts the code relies on

Each was checked on 2026-11-03 against the source on `main` at `299b2ea`, or by running `okf.py` itself (`python3 -I`, Python 3.14.4). `R` is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

1. **serde 1.0.229.**
   - `ReplayOp` is already an internally tagged enum (`#[serde(tag = "op", rename_all = "snake_case")]`, `src/okf/ops/replay.rs:28-30`). Struct variants with a struct-typed field (`Relink { redirect: Redirect }`) and an `Option<String>` field are supported: `serde_derive`'s internally tagged path (`R/serde_derive-1.0.229/src/de.rs:648`, `InternallyTagged`).
   - `BTreeMap<String, Option<String>>` serializes as a JSON object with `null` for `None`.
2. **schemars 1.2.3** reads `#[serde(rename = "...")]` on a field: `rename` is in the serde keywords `schemars_derive` parses (`R/schemars_derive-1.2.3/src/attr/schemars_to_serde.rs:13`). So `#[serde(rename = "type")] kind` is `type` in the input schema. Task 6's listing test asserts it.
3. **regex 1.13.1** has no lookaround. That is why `markdown.rs` drops `(?<!!)` by hand (`src/okf/markdown.rs:6-8`) and why `footnote_refs` drops `FOOTREF_RE`'s `(?!:)` by hand. Methods used: `captures_iter` (`R/regex-1.13.1/src/regex/string.rs:469`), `captures` (as `markdown.rs` already uses it).
4. **std:** `[Cow<str>]::join("\n")` (std's `Join` for `[S: Borrow<str>]`), as `insert_index_bullet_text` uses it (`src/okf/ops/replay.rs:396`).
5. **M1-M5 API used here** (on `main` at `299b2ea`):
   - **Ops:** `okf::ops::{OpContext, OpOutcome}` (`src/okf/ops/mod.rs:54`, `:63`):
     - `OpOutcome::{touch, record}` (`:87`, `:94`; `record` touches and appends an op) and `unchanged` (`:78`);
     - `load_concept` returns `LoadedConcept { path, doc, data }` (`:171-200`), refusing a document without frontmatter;
     - `refused` (`:227`) and `stamp_value` (`:219`).

     `OpOutcome` is built with a struct literal in `mcp::changesets::revert` (`src/mcp/changesets.rs:688-694`), so M6 adds no field to it.
   - **Replay** (`src/okf/ops/replay.rs`):
     - `ReplayOp` (`:30`) and `replay` (`:87`), which replays onto an empty `MemWorkspace` (`no_dirs`, `:90`);
     - the module-private `present` (`:127`) and `is_bullet` (`:252`), usable only inside `replay.rs` (so `listing_line`, which uses `is_bullet`, lives there), and the crate-private `missing_heading` (`:134`), `lists_target` (`:145`) and `new_index_text` (`:181`);
     - `insert_index_bullet_text(text, bullet, section, create_section) -> Option<String>` (`:291`), which is always `Some` when `section` is `None`.
   - **Bookkeeping** (`src/okf/ops/log_index.rs`):
     - `collapse_whitespace` (`:34`, private) is `" ".join(text.split())`;
     - `ensure_index` (`:210`, `pub`) records `CreateIndex`, recurses into the parent, then records the parent's `ListBullet` and pushes the "created" note. Its touch order matches `okf.py`'s `touched`.
   - **Notes:** `create_concept` pushes `new_concept_path`'s note as `format!("note: {note}")` (`src/okf/ops/content.rs:202-203`).
   - **Paths** (`src/okf/paths.rs`):
     - `resolve_concept` (`:190`) gives `InvalidPath` "is not a concept document" for `index.md`, `log.md` and the root readmes, and `NotFound { path: reference }` otherwise;
     - `new_concept_path` (`:227`) gives `(path, note)`. Its refusals are `InvalidPath { path: reference, .. }`, with "not a path inside the catalog", "<name> is a reserved name, not a concept" and "that's repository machinery, not a place for concepts", plus `InvalidPath { path: normalized, reason: "already exists" }` (`:255-259`). The note reads "<name> isn't short, lowercase and hyphenated";
     - `dirname` (`:101`) is `""` for a top-level file; `basename` (`:112`), `join` (`:124`), `is_concept` (`:141`), `NON_CONCEPTS` (`:23`) and `RESERVED` (`:25`).
   - **Markdown** (`src/okf/markdown.rs`):
     - `find_links` (`:99`) gives `LinkMatch { text, target, range, target_range }` (`:76-85`): byte ranges in the line, of the whole link without the preceding char and of the target;
     - `resolve_link(workspace, link, source) -> Option<ResolvedLink { path, absolute }>` (`:362`) is `None` for external and empty targets;
     - `headings(lines) -> Vec<Heading { index, level, text, slug }>` (`:211`) skips fenced code, with `text` `py_strip`ped and slugs numbered as `heading_slugs`;
     - `footnote_defs(lines) -> IndexMap<String, Vec<usize>>` (`:321`) and `body_split(text) -> (Vec<&str>, Vec<&str>)` (`:156`);
     - the `HEADING` regex (`:51`) is private.
   - **Python string helpers** (`src/okf/pystr.rs`): `py_strip` (`:42`), `py_rstrip` (`:47`), `universal_newlines -> Cow<str>` (`:67`), `py_repr_str` (`:100`), `py_str` (`:190`, Python's `str()` of a value), `py_truthy(Option<&Value>)` (`:205`) and `py_unquote -> Cow<str>` (`:249`, `urllib.parse.unquote`).
   - **YAML** (`src/okf/yaml.rs`): `scalar(text, ScalarStyle::Block)` (`:711`), `flow_value(&Value)` (`:727`), `block_item(&Value, indent) -> Vec<String>` (`:753`) and `YamlError { message, line }` (`:76`).
   - **Documents:** `Doc::from_text` (`src/okf/doc.rs:184`), `parse` (`:279`, `Result<IndexMap, YamlError>`; an empty map without frontmatter) and `parse_checked` (`:287`).
   - **Workspaces** (`src/okf/workspace.rs`):
     - the `Workspace` trait (`:21-104`);
     - `MemWorkspace::remove(&self, path) -> Result<bool, OkfError>` (`:651`);
     - `write_atomic` (`:756-785`) keeps an existing file's execute bit and refuses "is a directory" and "a parent component is a file";
     - `has_key_with_prefix` (`:490`);
     - `Listing::Markdown` (`:167-171`) skips dot-directories, `SKIP_DIRS` at any depth and `SKIP_TOP` at the root, as `okf.py`'s `walk_md` (`okf.py:599`) does, in `walk_order` (`src/okf/paths.rs:155`), which is `os.walk`'s order.
   - **Rules:** `rules::check_path` (`src/okf/rules.rs:75`) normalizes the path and refuses one outside the catalog or in machinery; `is_machinery` is at `:47`.
   - **Snapshot and lint:**
     - `SnapshotDoc::in_link_graph` (`src/okf/snapshot.rs:217`) is false only for the root `AGENTS.md`, `CLAUDE.md` and `README.md`;
     - `links_report` reads every doc in the graph (`src/okf/analysis/links.rs:122-127`), `log.md` and every `index.md` included, and lint's `broken-link` errors come from it (`src/okf/analysis/lint.rs:473`).
   - **Write path** (`src/write.rs`):
     - `apply` (`:104`): a touched path that is gone is stored as a deletion when `new_entry` (`:215`) finds a base or published revision;
     - `touched_file` (`:296`) applies `check_path` and `check_content`;
     - `merged_replay` (`:321`), with the `expect` text at `:341`.
   - **Publish** (`src/store/publish.rs`): `merge` (`:282`), `replayed_text` (`:331`, which runs `replay` for any stored ops), `check` (`:363`), `split_lint` (`:419`), `links_into_deleted` (`:439`) and `write_all` (`:476`).
   - **Server:**
     - `OkfServer::tools` (`src/mcp/mod.rs:64`) and the router-block pattern (`#[tool_router(router = writes_router, vis = "pub(crate)")]`, `src/mcp/writes.rs:213`);
     - `reply` and `tool_error`;
     - `mcp::changesets::{publish, discard, revert, pending_changes}` (`pub(crate)`, with params `PublishParams`, `DiscardParams`, `RevertParams`, `mcp::reports::CatalogParam`);
     - `pending_files` (`src/mcp/changesets.rs:760-764`) gives `delete` for an entry without content, `create` without a published file, else `update`.
   - **Errors:** `OkfError::{kind, to_json, is_internal}` (`src/error.rs`) and `AppError::status` (`src/http/mod.rs:78`) match exhaustively, with no `_` arm.
   - **Test support:**
     - `mcp::testing::{app_with, writer, TestApp}` (`src/mcp/testing.rs:42`, `:176`);
     - `tests/support/server.rs` (`TestServer::{start, key, client, client_with}`, `call`, `structured`, `files`);
     - `tests/parity.rs`: `run_op` (`:382`), `rust_ops` (`:453`), `ops_match_golden` (`:628`), `live_ops_parity` (`:843`) and `regenerate_goldens` (`:876`);
     - the shim's `OPS_DEFAULTS` and `run_op`, which calls `getattr(okf, f"cmd_{command}")(argparse.Namespace(root=work, **fields))`.
6. **`okf.py` behaviour ported here** (`~/repos/pg_knowledge_catalog/tools/okf.py`, SHA-256 `3ee65c4d...a6c03` as pinned in `tests/golden/SOURCE`, unchanged since M5):
   - **`rewrite_links`** (`:1343-1400`):
     - it walks `walk_md`, skipping the root `NON_CONCEPTS - RESERVED`;
     - it reads and writes with `newline=""`, splits on `\n` and applies `LINK_RE.sub` per line;
     - the `repl` rules are ported line for line in Task 1, with `os.path.relpath` for relative links.
   - **`cmd_mv`** (`:1524-1574`):
     - `touched = [old, new]`, then `ensure_index(new_dir)`;
     - when the folder changes, the first `* ` or `- ` line of the old folder's index that links `old` is taken and the rest written back;
     - `git mv` if the file is tracked, else `os.rename`;
     - then the carried bullet with its link set to the new basename (the fragment kept), or `* [<title or stem>](<name>) - <description, whitespace collapsed>`, goes into the new index with `insert_index_bullet(section=None)`;
     - then `rewrite_links(..., moved={new: old})`; it prints `moved /<old> -> /<new>` and the touched list.
   - **`find_section`** (`:1577`) matches heading text case-insensitively, else the slug. Its refusals read `no heading 'X'; headings are:\n  ## A\n  ...` and `N headings match 'X'; pass the anchor slug instead: a, a-1`.
   - **`cmd_split`** (`:1589-1733`): ported line for line in Task 4. Its argparse defaults (`:2324-2329`) are `--type Note`, and `--title`, `--description`, `--stub` and `--source` `None`.
   - **Helpers:**
     - `footnote_defs` (`:906`);
     - `FOOTREF_RE = \[\^([^\]]+)\](?!:)` (`:843`);
     - `heading_slugs` (`:872`);
     - `norm_fragment` (`:891`, `unquote(frag).lower()`);
     - `resolve_link` (`:921`);
     - `insert_index_bullet` (`:1410`), `ensure_index` (`:1471`) and `new_concept_path` (`:1495`);
     - `print_touched` (`:1514`), which de-duplicates in first-seen order under `touched (pass these to publish):`.
   - **Probes.** Every expected text in Tasks 1, 3 and 4 was produced by running these functions on the task's fixture, with `now_utc` frozen.
7. **The 52-step parity script** (Tasks 3 and 4 together) runs through the shim. Its result:
   - 35 changed files and 11 error steps. With Task 3's steps alone it is 47 steps, 28 files and 9 errors; with Task 4's alone, 47 steps, 25 files and 10 errors;
   - the git-stubbed and unstubbed runs print identical JSON.

## Conventions for every task

The M4 and M5 plans' "Conventions for every task" apply unchanged. These are added:
- **Never log document text.** A refused delete reports paths and line numbers only.
- **Ops are pure.** No op touches the database or the clock; `OpContext::now` is the only time an op sees.
- **Errors:** a new `OkfError` variant needs an arm in `kind`, `to_json`, `is_internal` (when internal) and `AppError::status`. The matches are exhaustive, so the compiler finds a missing one.
- **Merging main.** Tasks 3 and 4 run in parallel and both extend:
  - `src/okf/ops/mod.rs` (one `pub mod` and one `pub use` line each);
  - `tests/parity/okf_shim.py` (`OPS_DEFAULTS`);
  - `tests/parity/ops_script.json` (setup files and steps);
  - `tests/parity.rs` (one `run_op` arm each).

  Keep both sides. Whichever merges second regenerates `tests/golden/ops.json` after the merge, so the golden holds every step.
- **Write tools** return `Result<Json<T>, CallToolResult>`, require `Access::Write` on the target catalog through `write::apply`, and act on exactly one catalog.

Gates (all must pass before every push; paste the counts into the PR):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -- --test-threads=6` (Docker, for testcontainers)
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
- Task 9 adds the release smoke test.
- Cargo puts test-name filters after `--`.

There is no CI and no justfile: run the gates locally.

Project rules (`.claude/CLAUDE.md`) carried into every task:
- no `.unwrap()` in library code, and `.expect("why")` only for invariants;
- `thiserror` errors;
- a doc comment on every public item, with Arguments, Returns and Errors where they apply;
- comments explaining Rust specifics (borrows, `Cow`, closures, iterator adapters, slice patterns, `?`) to a Python expert;
- no emoji, and no unicode that imitates it;
- `rustfmt`, and clippy with `-D warnings`.

Never write a secret or a secret-shaped string. Test keys are plain strings, never `okf_` followed by 43 characters. Never copy content from the private catalog into the repo: fixtures are synthetic, and they use `example.com` for URLs.

Changelog: one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m6-task-<NN>-<slug>.md`, with Added/Changed/Notes sections like M5's.

Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.

The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

These are the names and shapes later tasks rely on. A task that must change one corrects this section and every later task that uses it.

```rust
// Task 1: src/okf/paths.rs
pub fn relpath(path: &str, start: &str) -> String;          // os.path.relpath for two catalog-relative paths
pub fn outside_link_graph(path: &str) -> bool;              // root AGENTS.md, CLAUDE.md, README.md

// Task 1: src/okf/ops/relink.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redirect {
    pub from: String, pub to: String,
    pub anchors: Option<BTreeMap<String, Option<String>>>,   // None: every link (mv); Some: by fragment (split)
}
impl Redirect { pub fn apply(&self, target: &str, fragment: Option<&str>) -> Option<(String, Option<String>)>; }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved<'a> { pub path: &'a str, pub written_at: &'a str }
pub fn rewrite_links(workspace: &dyn Workspace, redirect: &Redirect, moved: Option<Moved<'_>>)
    -> Result<Vec<String>, OkfError>;                       // the files changed, in walk order
pub(crate) fn rewrite_text(workspace: &dyn Workspace, path: &str, base: &str, text: &str,
    redirect: &Redirect) -> Option<String>;                 // None: unchanged

// Task 2: src/okf/ops/replay.rs, src/okf/ops/mod.rs, src/okf/ops/log_index.rs
ReplayOp::Unlist { target: String, bullet: Option<String> } // {"op": "unlist", ...}
ReplayOp::Relink { redirect: Redirect }                     // {"op": "relink", "redirect": {...}}
pub(crate) fn listing_line(workspace: &dyn Workspace, text: &str, index: &str, target: &str) -> Option<usize>;
pub(crate) fn unlist_text(workspace: &dyn Workspace, text: &str, index: &str, target: &str)
    -> Option<(String, String)>;                            // (text without the bullet, the bullet)
impl OpOutcome { pub fn record_rewrites(&mut self, changed: &[String], redirect: &Redirect); }
pub(crate) fn collapse_whitespace(text: &str) -> String;    // was private in log_index.rs
pub fn py_truthy_str(value: Option<&Value>) -> Option<String>; // src/okf/pystr.rs: str(v) if v else None

// Task 3: src/okf/ops/moves.rs, src/okf/workspace.rs
pub fn move_concept(workspace: &MemWorkspace, old: &str, new: &str) -> Result<OpOutcome, OkfError>;
impl MemWorkspace { pub fn rename(&self, from: &str, to: &str) -> Result<(), OkfError>; }
impl MemWorkspace { pub fn check_placement(&self, path: &str) -> Result<(), OkfError>; } // write_atomic's placement check, without writing

// Task 4: src/okf/ops/split.rs, src/okf/markdown.rs
pub const DEFAULT_SPLIT_TYPE: &str = "Note";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split<'a> {
    pub concept: &'a str, pub section: &'a str, pub to: &'a str, pub kind: &'a str,
    pub title: Option<&'a str>, pub description: Option<&'a str>, pub stub: Option<&'a str>,
    pub sources: &'a [String],
}
pub fn split_concept(workspace: &dyn Workspace, ctx: &OpContext<'_>, split: &Split<'_>)
    -> Result<OpOutcome, OkfError>;
pub fn footnote_refs(text: &str) -> BTreeSet<String>;       // set(FOOTREF_RE.findall(text))
pub(crate) fn atx_heading(line: &str) -> Option<(usize, &str)>;  // HEADING_RE groups 1 (as a level) and 2

// Task 5: src/okf/ops/delete.rs, src/error.rs, src/http/mod.rs
pub fn delete_concept(workspace: &MemWorkspace, reference: &str) -> Result<OpOutcome, OkfError>;
OkfError::StillLinked { path: String, links: Vec<LinkSource> }   // HTTP 409
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct LinkSource { pub source: String, pub line: usize }   // catalog-relative, 1-based

// Task 6: src/mcp/restructure.rs, src/mcp/mod.rs
pub struct MoveConceptParams { pub old: String, pub new: String, pub catalog: Option<String> }
pub struct SplitConceptParams {
    pub path: String, pub section: String, pub to: String,
    #[serde(rename = "type")] pub kind: Option<String>,
    pub title: Option<String>, pub description: Option<String>, pub stub: Option<String>,
    pub sources: Option<Vec<String>>, pub by: Option<String>, pub catalog: Option<String>,
}
pub struct DeleteConceptParams { pub path: String, pub catalog: Option<String> }
pub(crate) async fn move_concept(ctx: &ToolContext, params: MoveConceptParams) -> Result<WriteReport, OkfError>;
pub(crate) async fn split_concept(ctx: &ToolContext, params: SplitConceptParams) -> Result<WriteReport, OkfError>;
pub(crate) async fn delete_concept(ctx: &ToolContext, params: DeleteConceptParams) -> Result<WriteReport, OkfError>;
// OkfServer::restructure_router(), joined last in OkfServer::tools()
```

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `src/okf/paths.rs` | T1 (`relpath`, `outside_link_graph`) | Path helpers |
| `src/okf/ops/relink.rs` | T1 (create) | `Redirect`, `Moved`, `rewrite_links`, `rewrite_text` |
| `src/okf/ops/mod.rs` | T1, T3, T4, T5 (one `pub mod` and `pub use` each); T2 (`record_rewrites`) | Module tree, `OpOutcome` (keep both sides) |
| `src/okf/ops/replay.rs` | T2 (modify) | `Unlist`, `Relink`, `listing_line`, `unlist_text` |
| `src/okf/ops/log_index.rs` | T2 (`collapse_whitespace` becomes `pub(crate)`) | Shared with `mv` and `split` |
| `src/write.rs` | T2 (the `expect` text in `merged_replay`) | Write path |
| `src/okf/pystr.rs` | T2 (`py_truthy_str`) | Python string helpers |
| `src/okf/ops/moves.rs` | T3 (create) | `move_concept` |
| `src/okf/workspace.rs` | T3 (`MemWorkspace::rename`, shared placement check) | In-memory workspace |
| `src/okf/ops/split.rs` | T4 (create) | `split_concept` |
| `src/okf/markdown.rs` | T4 (`footnote_refs`, `atx_heading`; `headings` uses `atx_heading`) | Markdown structure |
| `src/okf/ops/delete.rs` | T5 (create) | `delete_concept` |
| `src/error.rs`, `src/http/mod.rs` | T5 (`StillLinked`, `LinkSource`, its status) | Errors |
| `tests/parity/okf_shim.py`, `tests/parity/ops_script.json`, `tests/parity.rs`, `tests/golden/ops.json` | T3 (`mv`, the git stub, `print_touched`), T4 (`split`) | Ops parity (keep both sides) |
| `src/mcp/restructure.rs` | T6 (create) | The three tools |
| `src/mcp/mod.rs` | T6 (`mod restructure;`, `+ Self::restructure_router()`) | `OkfServer::tools()` |
| `src/mcp/changesets.rs` | T7 (a `restructure_publish_tests` module) | Publish database tests |
| `tests/restructure.rs` | T8 (create) | End-to-end over HTTP |
| `tests/smoke.rs`, `README.md` | T9 | Smoke test, docs |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T2 (Write step 4), T5 (Errors, the delete row), T6 (the three rows), T7 (Publish step 4), T9 (M6 delivered, "What M6 shipped differently") | Design |
| `changelog/*-m6-*.md` | every task | Iteration notes |

## Task dependencies

Layered (each task needs everything it points from; the edge list below is the source of truth):

```
Layer 0:  T1 relink: rewrite_links, relpath
Layer 1:  T2 replay: Unlist, Relink (T1)
Layer 2:  T3 move_concept + mv parity (T1, T2)    T4 split_concept + split parity (T1, T2)    T5 delete_concept (T2)
Layer 3:  T6 the three tools (T3, T4, T5)         T7 publish database tests (T3, T4, T5)
Layer 4:  T8 end-to-end tests (T6)
Layer 5:  T9 smoke, README, design, changelog (all)
```

Edges, explicitly:
- T1 -> T2 (`ReplayOp::Relink` holds a `Redirect` and replays through `rewrite_text`)
- T1 -> T3; T1 -> T4 (`rewrite_links`)
- T2 -> T3; T2 -> T4 (`Unlist`, `Relink`, `record_rewrites`, `collapse_whitespace`, `py_truthy_str`)
- T2 -> T5 (`Unlist`, `listing_line`, `unlist_text`)
- T3 -> T6; T4 -> T6; T5 -> T6 (the ops the tools call)
- T3 -> T7; T4 -> T7; T5 -> T7 (the publish tests run the ops through `write::apply`, as M5's Task 10 did, not through Task 6's tools)
- T6 -> T8 (the tools over HTTP)
- every task -> T9

Notes:
- **T1 is the only task with no dependencies.**
- **T3, T4 and T5 run in parallel after T2,** and so do T6 and T7 after them.
- **T8 needs only T6.** Publish has no new code, so T7 is tests only. If T7 has to change `src/store/publish.rs`, it adds the edge T7 -> T8 here.
- **Shared files:** `src/okf/ops/mod.rs` and the parity files (T3, T4); `src/error.rs` and `src/http/mod.rs` (T5 only); the design (T2, T5, T6, T7, T9). Keep both sides.

---

### Task 1: `okf::ops::relink`: `rewrite_links` and `paths::relpath`

This task ports `okf.py`'s `rewrite_links` (`:1343`). Every later task leans on it:
- `move_concept` points every link into the old path at the new one;
- `split_concept` points links into the moved section (and its subsections) at the new concept;
- publish replays it on `index.md` and `log.md` (Task 2's `Relink`).

The function splits in two:
- `rewrite_text` is one file's rewrite, pure over its text. The replay calls it with no directory information.
- `rewrite_links` walks the catalog and writes the files `rewrite_text` changed.

`okf.py` reads each file with `newline=""` (byte for byte) and splits on `\n` only, so a `\r\n` file keeps its endings. Here that is `read_to_string`, not `read_text`.

**Files:**
- Modify: `src/okf/paths.rs` (`relpath`, `outside_link_graph`, tests)
- Create: `src/okf/ops/relink.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod relink;`, re-exports, module doc)
- Create: `changelog/<ts>-m6-task-01-relink.md`

- [ ] **Step 1: Write the failing path tests**

Add to the `tests` module of `src/okf/paths.rs`:

```rust
    #[test]
    fn relpath_matches_os_path_relpath_for_catalog_paths() {
        let cases = [
            (("infra/clock.md", "infra"), "clock.md"),
            (("ops/net/ntp.md", "infra"), "../ops/net/ntp.md"),
            (("b", "c"), "../b"),
            (("infra", "infra"), "."),
            (("x.md", ""), "x.md"),
            (("", "a/b"), "../.."),
        ];

        // `map` over an array of tuples; each `((path, start), _)` destructures
        // one case, like Python's `for (path, start), _ in cases`.
        let results = cases.map(|((path, start), _)| relpath(path, start));

        assert_eq!(results, cases.map(|(_, expected)| expected.to_string()));
    }

    #[test]
    fn only_the_root_readmes_are_outside_the_link_graph() {
        let outside = ["AGENTS.md", "CLAUDE.md", "README.md"];
        let inside = ["index.md", "log.md", "infra/README.md", "infra/x.md", "a.md"];

        assert!(outside.iter().all(|path| outside_link_graph(path)));
        assert!(!inside.iter().any(|path| outside_link_graph(path)));
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib -- paths::tests::relpath paths::tests::only_the_root`
Expected: compile errors, `cannot find function relpath` and `outside_link_graph`.

- [ ] **Step 3: Implement the path helpers**

Add to `src/okf/paths.rs`, after `join`:

```rust
/// Python's `os.path.relpath(path, start)` for two catalog-relative paths.
///
/// Python makes both paths absolute first. For two paths inside one root
/// that only drops empty and `.` components, so the result is computed on
/// the components alone.
///
/// # Arguments
/// * `path` - the target, normalized and inside the catalog (`""` is the root).
/// * `start` - the directory the result is relative to, likewise.
///
/// # Returns
/// The relative path; `.` when the two are the same.
///
/// # Examples
/// ```
/// use okf_mcp::okf::paths::relpath;
/// assert_eq!(relpath("ops/net/ntp.md", "infra"), "../ops/net/ntp.md");
/// assert_eq!(relpath("infra/clock.md", "infra"), "clock.md");
/// assert_eq!(relpath("infra", "infra"), ".");
/// ```
pub fn relpath(path: &str, start: &str) -> String {
    // A nested `fn` (not a closure) so the returned slices can borrow from
    // the argument: the signature ties their lifetime to `p`'s.
    fn parts(p: &str) -> Vec<&str> {
        p.split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect()
    }
    let (path, start) = (parts(path), parts(start));
    // How many leading components the two share (`commonprefix`).
    let common = path
        .iter()
        .zip(&start)
        .take_while(|(a, b)| a == b)
        .count();
    // `vec![x; n]` is `[x] * n`; then the rest of `path` follows.
    let mut relative = vec![".."; start.len() - common];
    relative.extend(&path[common..]);
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative.join("/")
    }
}

/// Is `path` a root `AGENTS.md`, `CLAUDE.md` or `README.md`? `okf.py`'s
/// link analysis and `rewrite_links` skip exactly these
/// (`NON_CONCEPTS - RESERVED` at the root), as does
/// [`crate::okf::snapshot::SnapshotDoc::in_link_graph`].
///
/// # Arguments
/// * `path` - catalog-relative path.
///
/// # Returns
/// `true` for those three files at the root, `false` for every other path.
pub fn outside_link_graph(path: &str) -> bool {
    !path.contains('/') && NON_CONCEPTS.contains(&path) && !RESERVED.contains(&path)
}
```

- [ ] **Step 4: Write the failing relink tests**

Create `src/okf/ops/relink.rs` holding only the test module below; Step 6 adds the code above it. Every expected text below is what `okf.py`'s `rewrite_links` writes for the same files (probed 2026-11-03).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    /// A catalog just after `moves/alpha.md` became `moves/renamed.md`.
    const MOVE_FILES: [(&str, &str); 8] = [
        ("index.md", "# Root\n\n* [Moves](moves/) - m\n"),
        ("AGENTS.md", "See [alpha](moves/alpha.md).\n"),
        (
            "log.md",
            "# Log\n\n## 2026-11-03\n\n* **Update:** Added [alpha](/moves/alpha.md).\n",
        ),
        ("moves/index.md", "# Moves\n\n* [Alpha](alpha.md) - first\n"),
        (
            "moves/renamed.md",
            "---\ntype: Note\n---\nSee [beta](beta.md), [part](#part) and \
             [web](https://example.com/x).\n\n## Part\n",
        ),
        (
            "moves/beta.md",
            "---\ntype: Note\n---\nBack to [alpha](alpha.md#part) and \
             [abs](/moves/alpha.md).\r\nUntouched line.\r\n",
        ),
        (
            "people/linker.md",
            "---\ntype: Note\n---\nSee [alpha](../moves/alpha.md#Part) and [folder](/moves/).\n",
        ),
        ("people/other.md", "---\ntype: Note\n---\nNo links here.\n"),
    ];

    fn redirect(from: &str, to: &str) -> Redirect {
        Redirect {
            from: from.to_string(),
            to: to.to_string(),
            anchors: None,
        }
    }

    fn text(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn a_move_redirects_every_style_and_keeps_fragments_and_line_endings() {
        let workspace = MemWorkspace::from_files(MOVE_FILES);
        let moved = Moved {
            path: "moves/renamed.md",
            written_at: "moves/alpha.md",
        };

        let changed = rewrite_links(
            &workspace,
            &redirect("moves/alpha.md", "moves/renamed.md"),
            Some(moved),
        )
        .unwrap();

        assert_eq!(
            changed,
            [
                "log.md",
                "moves/beta.md",
                "moves/index.md",
                "moves/renamed.md",
                "people/linker.md"
            ]
        );
        assert_eq!(
            text(&workspace, "log.md"),
            "# Log\n\n## 2026-11-03\n\n* **Update:** Added [alpha](/moves/renamed.md).\n"
        );
        // Relative stays relative, absolute stays absolute, `\r\n` stays.
        assert_eq!(
            text(&workspace, "moves/beta.md"),
            "---\ntype: Note\n---\nBack to [alpha](renamed.md#part) and \
             [abs](/moves/renamed.md).\r\nUntouched line.\r\n"
        );
        assert_eq!(
            text(&workspace, "moves/index.md"),
            "# Moves\n\n* [Alpha](renamed.md) - first\n"
        );
        // The moved file's relative links become absolute; in-page ones stay.
        assert_eq!(
            text(&workspace, "moves/renamed.md"),
            "---\ntype: Note\n---\nSee [beta](/moves/beta.md), [part](#part) and \
             [web](https://example.com/x).\n\n## Part\n"
        );
        // The fragment is kept as written (`#Part`), and the folder link is not this file's.
        assert_eq!(
            text(&workspace, "people/linker.md"),
            "---\ntype: Note\n---\nSee [alpha](../moves/renamed.md#Part) and [folder](/moves/).\n"
        );
        // The root AGENTS.md is not catalog content.
        assert_eq!(text(&workspace, "AGENTS.md"), "See [alpha](moves/alpha.md).\n");
    }

    #[test]
    fn a_split_redirect_moves_only_the_named_anchors() {
        let workspace = MemWorkspace::from_files([
            (
                "splits/parent.md",
                "---\ntype: Note\n---\n# Intro\n\nSee [bg](#background).\n\n## Background\n",
            ),
            (
                "splits/background.md",
                "---\ntype: Note\n---\nIntro [back](#intro) and [detail](#detail).\n\n# Detail\n",
            ),
            (
                "splits/user.md",
                "---\ntype: Note\n---\n[a](/splits/parent.md#background) [b](parent.md#Detail) \
                 [c](/splits/parent.md#intro) [d](/splits/parent.md)\n",
            ),
        ]);
        let anchors = BTreeMap::from([
            ("background".to_string(), None),
            ("detail".to_string(), Some("detail".to_string())),
        ]);
        let redirect = Redirect {
            from: "splits/parent.md".to_string(),
            to: "splits/background.md".to_string(),
            anchors: Some(anchors),
        };
        let moved = Moved {
            path: "splits/background.md",
            written_at: "splits/parent.md",
        };

        let changed = rewrite_links(&workspace, &redirect, Some(moved)).unwrap();

        assert_eq!(
            changed,
            ["splits/background.md", "splits/parent.md", "splits/user.md"]
        );
        assert_eq!(
            text(&workspace, "splits/background.md"),
            "---\ntype: Note\n---\nIntro [back](/splits/parent.md#intro) and \
             [detail](#detail).\n\n# Detail\n"
        );
        assert_eq!(
            text(&workspace, "splits/parent.md"),
            "---\ntype: Note\n---\n# Intro\n\nSee [bg](/splits/background.md).\n\n## Background\n"
        );
        assert_eq!(
            text(&workspace, "splits/user.md"),
            "---\ntype: Note\n---\n[a](/splits/background.md) [b](background.md#detail) \
             [c](/splits/parent.md#intro) [d](/splits/parent.md)\n"
        );
    }

    #[test]
    fn folder_links_keep_their_trailing_slash() {
        let workspace = MemWorkspace::from_files([
            ("a/index.md", "# A\n"),
            ("b/index.md", "# B\n"),
            (
                "c/x.md",
                "---\ntype: Note\n---\n[rel](../a/) [abs](/a/) [file](../a/index.md#top) \
                 [root](../index.md)\n",
            ),
            ("index.md", "# Root\n"),
        ]);

        let changed =
            rewrite_links(&workspace, &redirect("a/index.md", "b/index.md"), None).unwrap();

        assert_eq!(changed, ["c/x.md"]);
        assert_eq!(
            text(&workspace, "c/x.md"),
            "---\ntype: Note\n---\n[rel](../b/) [abs](/b/) [file](../b/index.md#top) \
             [root](../index.md)\n"
        );
    }

    #[test]
    fn rewrite_text_is_none_when_nothing_points_at_the_source() {
        let workspace = MemWorkspace::default();
        let text = "Plain [web](https://example.com/a.md) and [other](b.md) and [x](#x).\n";

        let result = rewrite_text(
            &workspace,
            "a.md",
            "a.md",
            text,
            &redirect("c.md", "d.md"),
        );

        assert_eq!(result, None);
    }

    #[test]
    fn redirect_matches_anchors_after_decoding_and_lower_casing() {
        let anchors = BTreeMap::from([("open items".to_string(), Some("todo".to_string()))]);
        let redirect = Redirect {
            from: "a.md".to_string(),
            to: "b.md".to_string(),
            anchors: Some(anchors),
        };

        let decoded = redirect.apply("a.md", Some("Open%20Items"));
        let unnamed = redirect.apply("a.md", Some("other"));
        let bare = redirect.apply("a.md", None);
        let elsewhere = redirect.apply("z.md", Some("open items"));

        assert_eq!(
            decoded,
            Some(("b.md".to_string(), Some("todo".to_string())))
        );
        assert_eq!((unnamed, bare, elsewhere), (None, None, None));
    }
}
```

- [ ] **Step 5: Run them to see them fail**

Add `pub mod relink;` to `src/okf/ops/mod.rs` (Step 7 adds the rest). Then run: `cargo test --lib -- okf::ops::relink`
Expected: compile errors, `cannot find type Redirect`, `Moved`, `cannot find function rewrite_links`, `rewrite_text`.

- [ ] **Step 6: Implement `relink.rs`**

Put this above the test module in `src/okf/ops/relink.rs`:

```rust
//! Link rewriting across a catalog: `okf.py`'s `rewrite_links` (`:1343`).
//! `okf.py`'s `mv` and `split` use it to point every link into a moved
//! document (or a moved section) at its new place; M6 ports both onto it.
//!
//! A link keeps its style: in-page (`#x`), bundle-absolute (`/dir/x.md`) or
//! relative (`x.md`). A document that has just been moved, or created from
//! another's text, gets bundle-absolute links, the catalog's preferred style.
//!
//! Files are read byte for byte (`okf.py` opens them with `newline=""`) and
//! split on `\n` only, so a `\r\n` file keeps its line endings: only link
//! targets change. `rewrite_text` (crate-internal) is one file's rewrite,
//! pure over its text, so a replay can share it with [`rewrite_links`].

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::paths::{dirname, is_concept, outside_link_graph, relpath};
use crate::okf::pystr::py_unquote;
use crate::okf::workspace::Workspace;

/// Which links a rewrite redirects, and where: `rewrite_links`' `fn`, as
/// data, so a replay op can store it (a record, so its fields are public).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redirect {
    /// The catalog-relative path whose links are redirected.
    pub from: String,
    /// Where they point afterwards.
    pub to: String,
    /// `None`: every link into `from`, its fragment kept (`mv`). `Some`:
    /// only links into `from` whose fragment, percent-decoded and
    /// lower-cased, is a key; the value is the new fragment, `None` to drop
    /// it (`split`'s `amap`).
    pub anchors: Option<BTreeMap<String, Option<String>>>,
}

impl Redirect {
    /// Where a link into `target` with `fragment` points now (`okf.py`'s
    /// `fn(t, frag)`).
    ///
    /// # Arguments
    /// * `target` - the link's resolved, catalog-relative target.
    /// * `fragment` - the text after its first `#`, if it has one.
    ///
    /// # Returns
    /// The new target and fragment, or `None` to leave the link alone.
    pub fn apply(
        &self,
        target: &str,
        fragment: Option<&str>,
    ) -> Option<(String, Option<String>)> {
        if target != self.from {
            return None;
        }
        match &self.anchors {
            None => Some((self.to.clone(), fragment.map(str::to_string))),
            Some(anchors) => {
                // `?` on an `Option`: a link without a fragment is left alone.
                // `norm_fragment`: `unquote(frag).lower()`.
                let wanted = py_unquote(fragment?).to_lowercase();
                anchors
                    .get(&wanted)
                    .map(|new_fragment| (self.to.clone(), new_fragment.clone()))
            }
        }
    }
}

/// A document whose relative and in-page links were written somewhere
/// else: a file just moved, or created from another file's text
/// (`rewrite_links`' `moved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved<'a> {
    /// Where the document is now.
    pub path: &'a str,
    /// The path its links were written against.
    pub written_at: &'a str,
}

/// Port of `rewrite_links`: rewrites the links `redirect` names in every
/// markdown file of the catalog, and writes the files that changed.
///
/// # Arguments
/// * `workspace` - the catalog, after the move or split wrote its files.
/// * `redirect` - which links to redirect, and where.
/// * `moved` - the one document whose links were written at another path.
///
/// # Returns
/// The files changed, in walk order (`os.walk`'s, as `okf.py` lists them).
///
/// # Errors
/// The workspace's read or write error (a markdown file that is not UTF-8
/// is [`OkfError::Io`], where `okf.py` raises `UnicodeDecodeError`).
pub fn rewrite_links(
    workspace: &dyn Workspace,
    redirect: &Redirect,
    moved: Option<Moved<'_>>,
) -> Result<Vec<String>, OkfError> {
    let mut changed = Vec::new();
    for path in workspace.list_markdown()? {
        if outside_link_graph(&path) {
            continue;
        }
        let base = match moved {
            Some(moved) if moved.path == path => moved.written_at,
            Some(_) | None => path.as_str(),
        };
        // Byte for byte: `rewrite_text` splits on `\n` and leaves `\r` alone.
        let text = workspace.read_to_string(&path)?;
        if let Some(updated) = rewrite_text(workspace, &path, base, &text, redirect) {
            workspace.write_atomic(&path, &updated)?;
            changed.push(path);
        }
    }
    Ok(changed)
}

/// One file's rewrite: `rewrite_links`' loop body.
///
/// # Arguments
/// * `workspace` - resolves links. A directory link written without a
///   trailing slash (`[x](../a)`) reaches its `index.md` only through
///   [`Workspace::is_dir`]. The replay passes an empty workspace, which is
///   enough only while `redirect.from` is a concept (not an `index.md`): no
///   directory link can point at a concept. `move_concept` and
///   `split_concept` only ever redirect a concept, and the replay
///   refuses any other source with an error. [`rewrite_links`] has the real workspace, so it
///   also handles an `index.md` source, as `okf.py` does.
/// * `path` - the file's catalog-relative path.
/// * `base` - the path its links were written against (`path`, unless it
///   was just moved).
/// * `text` - its content, as stored.
/// * `redirect` - which links to redirect.
///
/// # Returns
/// The new text, or `None` when no line changed.
pub(crate) fn rewrite_text(
    workspace: &dyn Workspace,
    path: &str,
    base: &str,
    text: &str,
    redirect: &Redirect,
) -> Option<String> {
    let mut changed = false;
    // `Cow` keeps every unchanged line borrowed; only rewritten lines are new strings.
    let lines: Vec<Cow<'_, str>> = text
        .split('\n')
        .map(|line| match rewrite_line(workspace, path, base, line, redirect) {
            Some(updated) if updated != line => {
                changed = true;
                Cow::Owned(updated)
            }
            Some(_) | None => Cow::Borrowed(line),
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

/// `LINK_RE.sub(repl, line)`: the line with each redirected link's target
/// replaced, or `None` if no link was redirected.
fn rewrite_line(
    workspace: &dyn Workspace,
    path: &str,
    base: &str,
    line: &str,
    redirect: &Redirect,
) -> Option<String> {
    let mut out: Option<String> = None;
    let mut copied = 0;
    for link in find_links(line) {
        let Some(target) = redirected_target(workspace, path, base, &link.target, redirect) else {
            continue;
        };
        // `get_or_insert_with` builds the output only once a link changes.
        let buffer = out.get_or_insert_with(|| String::with_capacity(line.len() + 16));
        buffer.push_str(&line[copied..link.target_range.start]);
        buffer.push_str(&target);
        copied = link.target_range.end;
    }
    // `map` runs only when something was rewritten; `mut` lets the closure
    // append the rest of the line to the buffer it now owns.
    out.map(|mut buffer| {
        buffer.push_str(&line[copied..]);
        buffer
    })
}

/// `rewrite_links`' `repl`: the new target text of one link, or `None` to
/// keep the link as written.
fn redirected_target(
    workspace: &dyn Workspace,
    path: &str,
    base: &str,
    raw: &str,
    redirect: &Redirect,
) -> Option<String> {
    // `raw.partition("#")`: the part before the first `#`, and the fragment
    // (`None` when there is no `#` at all, `Some("")` for a bare `#`).
    let (target_part, fragment) = match raw.split_once('#') {
        Some((target, fragment)) => (target, Some(fragment)),
        None => (raw, None),
    };
    let own = raw.starts_with('#');
    let (target, absolute) = if own {
        (base.to_string(), true)
    } else {
        // External and empty targets are left alone (`tgt is None`).
        let resolved = resolve_link(workspace, raw, base)?;
        (resolved.path, resolved.absolute)
    };
    let (new_target, new_fragment) = redirect
        .apply(&target, fragment)
        .unwrap_or_else(|| (target.clone(), fragment.map(str::to_string)));
    let unchanged = new_target == target && new_fragment.as_deref() == fragment;
    if unchanged && (base == path || absolute) && (!own || new_target == path) {
        return None;
    }
    // A fragment that is empty counts as none, as Python's `if nfrag`.
    let fragment = new_fragment.as_deref().filter(|fragment| !fragment.is_empty());
    let mut new = if own && new_target == path {
        match fragment {
            Some(fragment) => format!("#{fragment}"),
            None => format!("/{new_target}"),
        }
    } else if own || absolute || (base != path && is_concept(path)) {
        // `strip_suffix` gives `Some(folder/)` only when the path ends in `index.md`.
        match new_target.strip_suffix("index.md") {
            Some(folder) if target_part.ends_with('/') && new_target.ends_with("/index.md") => {
                format!("/{folder}")
            }
            Some(_) | None => format!("/{new_target}"),
        }
    } else if target_part.ends_with('/') && new_target.ends_with("index.md") {
        // `dirname(ntgt) or "."`: `relpath` reads `""` as the root.
        format!("{}/", relpath(dirname(&new_target), dirname(path)))
    } else {
        relpath(&new_target, dirname(path))
    };
    if let Some(fragment) = fragment {
        if !new.starts_with('#') {
            new.push('#');
            new.push_str(fragment);
        }
    }
    Some(new)
}
```

- [ ] **Step 7: Wire the module**

In `src/okf/ops/mod.rs`:
- add `pub mod relink;` between `pub mod log_index;` and `pub mod replay;`;
- add `pub use relink::{rewrite_links, Moved, Redirect};` after the `log_index` re-export;
- extend the module doc's first paragraph to say: "...and the server's content edits (`create_concept`, `edit_document`, `replace_body`, `set_frontmatter`), and the restructuring edits (M6): `rewrite_links` (`relink`)."

> **Corrected (issue #120), 2026-11-03.** The docs name only code that exists. The `ops` module doc lists `rewrite_links` alone; Tasks 3, 4 and 5 each add their op's name to that list (`move_concept`, `split_concept`, `delete_concept`) in the step that adds its `pub use`. The `relink` module doc no longer names `ReplayOp::Relink`; Task 2 adds it there. Review on PR #129.

- [ ] **Step 8: Run the tests**

Run: `cargo test --lib -- okf::ops::relink paths::tests`
Expected: all pass (5 relink tests, the 2 new path tests).

Run: `cargo test --doc -- relpath`
Expected: the `relpath` example passes.

- [ ] **Step 9: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-01-relink.md`:

```markdown
# M6 Task 1: `okf::ops::relink`: `rewrite_links` and `paths::relpath` (<ts>)

Task 1 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/okf/ops/relink.rs`: `Redirect` (rewrite_links' `fn` as data: every link into a
  path, or only the named anchors), `Moved`, `rewrite_links` (port of `okf.py:1343`)
  and the crate-internal per-file `rewrite_text` that the replay will share; 5 unit
  tests whose expected texts were produced by okf.py.
- `src/okf/paths.rs`: `relpath` (`os.path.relpath` for catalog paths) and
  `outside_link_graph` (the root AGENTS.md, CLAUDE.md and README.md); 2 unit tests
  and a doc example.

## Changed

- `src/okf/ops/mod.rs`: `pub mod relink;`, re-exports, module doc.

## Notes

- Files are read with `read_to_string` (okf.py's `newline=""`), so `\r\n` endings
  survive a rewrite.
- Directory detection in `resolve_link` stays lexical (M1's documented divergence);
  a redirect's source is always a file, so it never matters for which links match.
```

Run every gate. Commit:

```bash
git add src/okf/paths.rs src/okf/ops/relink.rs src/okf/ops/mod.rs changelog/
git commit -m "feat: okf::ops::relink, a port of okf.py's rewrite_links (M6.01)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: replay: `ReplayOp::Unlist` and `ReplayOp::Relink`

This task adds the two operations of Decisions 1 and 6, with their pure text edits and their replay. It also adds `OpOutcome::record_rewrites`, which Tasks 3 and 4 call to record `rewrite_links`' changes: an `index.md` or `log.md` with a `Relink`, any other file as a free edit (Decision 7).

Publish needs no change: `store::publish::replayed_text` (`src/store/publish.rs:331`) deserializes whatever ops an entry stores and calls `replay`. A replay error (here, `Unlist`'s refusal of a changed listing) fails the publish with that error and leaves the changeset open, as a missing heading already does.

**Files:**
- Modify: `src/okf/ops/replay.rs` (variants, `listing_line`, `unlist_text`, the replay arms, tests)
- Modify: `src/okf/ops/mod.rs` (`record_rewrites`, `is_bookkeeping`, a test module)
- Modify: `src/okf/ops/log_index.rs` (`collapse_whitespace` becomes `pub(crate)`)
- Modify: `src/write.rs` (the `expect` text in `merged_replay`)
- Modify: `src/okf/pystr.rs` (`py_truthy_str`, which Tasks 3 and 4 share)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Data flow", "Write", step 4)
- Create: `changelog/<ts>-m6-task-02-unlist-relink.md`

- [ ] **Step 1: Write the failing replay tests**

Add to the `tests` module of `src/okf/ops/replay.rs` (it already imports `super::*`, `MemWorkspace` through `super::*`, and `serde_json::json`):

```rust
    const INFRA: &str =
        "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n* [NTP](ntp.md) - sync\n";

    fn unlist(target: &str, bullet: Option<&str>) -> ReplayOp {
        ReplayOp::Unlist {
            target: target.to_string(),
            bullet: bullet.map(str::to_string),
        }
    }

    fn relink(from: &str, to: &str) -> ReplayOp {
        ReplayOp::Relink {
            redirect: Redirect {
                from: from.to_string(),
                to: to.to_string(),
                anchors: None,
            },
        }
    }

    #[test]
    fn unlist_text_takes_the_first_bullet_linking_the_target() {
        let workspace = MemWorkspace::default();
        let text = concat!(
            "# Infra\n\nSee [NTP](ntp.md).\n\n",
            "* [NTP](/infra/ntp.md) - sync\n* [NTP again](ntp.md) - twice\n",
        );

        let line = listing_line(&workspace, text, "infra/index.md", "infra/ntp.md");
        let taken = unlist_text(&workspace, text, "infra/index.md", "infra/ntp.md");
        let missing = unlist_text(&workspace, INFRA, "infra/index.md", "infra/other.md");

        // The prose link on line 2 is not a bullet; the absolute bullet is first.
        assert_eq!(line, Some(4));
        assert_eq!(
            taken,
            Some((
                "# Infra\n\nSee [NTP](ntp.md).\n\n* [NTP again](ntp.md) - twice\n".to_string(),
                "* [NTP](/infra/ntp.md) - sync".to_string()
            ))
        );
        assert_eq!(missing, None);
    }

    #[test]
    fn unlist_replays_idempotently_and_refuses_a_changed_listing() {
        let moved = [unlist("infra/ntp.md", Some("* [NTP](ntp.md) - sync"))];
        let edited = INFRA.replace("- sync", "- how hosts sync");

        let once = replay("infra/index.md", Some(INFRA), &moved).unwrap();
        let twice = replay("infra/index.md", Some(&once), &moved).unwrap();
        let refused = replay("infra/index.md", Some(&edited), &moved).unwrap_err();
        let deleted = replay("infra/index.md", Some(&edited), &[unlist("infra/ntp.md", None)]);

        assert_eq!(once, "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n");
        assert_eq!(twice, once);
        assert!(
            matches!(refused, OkfError::Refused { ref reason } if reason.contains("changed since")),
            "{refused:?}"
        );
        // A delete removes the listing whatever it reads now.
        assert_eq!(deleted.unwrap(), once);
    }

    #[test]
    fn relink_replays_byte_for_byte_and_idempotently() {
        let crlf = "# Infra\r\n\r\n* [Time](time.md) - the clock\r\n";
        let ops = [relink("infra/time.md", "infra/clock.md")];

        let once = replay("infra/index.md", Some(crlf), &ops).unwrap();
        let twice = replay("infra/index.md", Some(&once), &ops).unwrap();

        assert_eq!(once, "# Infra\r\n\r\n* [Time](clock.md) - the clock\r\n");
        assert_eq!(twice, once);
    }

    #[test]
    fn a_log_entry_then_a_relink_replay_in_order() {
        let ops = [
            ReplayOp::Log {
                date: "2026-11-03".to_string(),
                bullet: "* **Update:** Added [x](/infra/x.md).".to_string(),
            },
            relink("infra/x.md", "infra/y.md"),
        ];

        let replayed = replay("log.md", Some("# Log\n"), &ops).unwrap();

        assert_eq!(
            replayed,
            "# Log\n\n## 2026-11-03\n\n* **Update:** Added [x](/infra/y.md).\n"
        );
    }

    #[test]
    fn the_new_ops_serialize_with_an_op_tag() {
        let ops = vec![unlist("a/b.md", None), relink("a/b.md", "a/c.md")];

        let value = serde_json::to_value(&ops).unwrap();
        let back: Vec<ReplayOp> = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(
            value,
            json!([
                {"op": "unlist", "target": "a/b.md", "bullet": null},
                {"op": "relink", "redirect": {"from": "a/b.md", "to": "a/c.md", "anchors": null}},
            ])
        );
        assert_eq!(back, ops);
    }
```

Add a test module at the end of `src/okf/ops/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_of_bookkeeping_files_replay_and_the_others_are_free() {
        let redirect = Redirect {
            from: "infra/x.md".to_string(),
            to: "infra/y.md".to_string(),
            anchors: None,
        };
        let mut outcome = OpOutcome::default();
        outcome.touch("infra/y.md");
        let changed = [
            "hub.md".to_string(),
            "infra/index.md".to_string(),
            "infra/y.md".to_string(),
            "log.md".to_string(),
        ];

        outcome.record_rewrites(&changed, &redirect);

        assert_eq!(
            outcome.touched,
            ["infra/y.md", "hub.md", "infra/index.md", "log.md"]
        );
        let relink = ReplayOp::Relink { redirect };
        assert_eq!(
            outcome.replay,
            BTreeMap::from([
                ("infra/index.md".to_string(), vec![relink.clone()]),
                ("log.md".to_string(), vec![relink]),
            ])
        );
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib -- okf::ops::replay okf::ops::tests`
Expected: compile errors, `no variant named Unlist`, `Relink`, `cannot find function listing_line`, `unlist_text`, `no method named record_rewrites`.

- [ ] **Step 3: The variants and their edits**

In `src/okf/ops/relink.rs`, add to the module doc's first paragraph: "Publish replays its link rewrites in `index.md` and `log.md` through [`crate::okf::ops::ReplayOp::Relink`]." (Corrected (issue #120), 2026-11-03: Task 1 left this out, so the doc did not name a variant before it existed.)

In `src/okf/ops/replay.rs`:

Update the module doc's first paragraph to end: "...so an edit and its replay cannot drift apart. M6 adds [`ReplayOp::Unlist`] (a listing taken out of a folder index) and [`ReplayOp::Relink`] (links redirected by a move or split), whose edits are `unlist_text` and `relink::rewrite_text` (both crate-internal: a public doc may not link them, or `cargo doc -D warnings` fails). `Unlist` works on universal-newline text, so a CRLF `index.md` comes out with `\n` endings, while `Relink` keeps bytes as they are. Both match `okf.py`: `cmd_mv` reads and writes the old index in text mode (`okf.py:1538`), and `rewrite_links` opens files with `newline=''` (`okf.py:1359`). This is not a parity bug."

Add the import:

```rust
use crate::okf::ops::relink::{rewrite_text, Redirect};
```

and extend the `paths` import to `use crate::okf::paths::{basename, is_concept};` (for the `Relink` arm's concept check).

> **Corrected (issue #120), 2026-11-03.** The replay's empty workspace resolves every link into `from` only while `from` is a concept, so the `Relink` arm `debug_assert!`s `is_concept(&redirect.from)`. Task 1's `rewrite_text` doc states the invariant. Review on PR #129.

> **Corrected (issue #121), 2026-11-03.** The `debug_assert!` became a check that returns `OkfError::Refused` (`non_concept_relink`, below). The op is deserialized from the database at publish, so a bad stored op must be an error in both build profiles: the assert would have panicked in debug builds and been skipped in release builds, which then resolved against `no_dirs` wrongly without any error. There is no `OkfError::Internal`, and adding a variant would touch `src/error.rs` and `src/http/mod.rs`, which Task 5 also extends. `Refused` is the error publish already returns for a replay it cannot place (a missing heading), so the changeset stays open. Review on PR #130. The same review added three tests: `unlist_writes_newline_endings_like_cmd_mv`, `unlist_of_a_missing_index_is_not_found` and `relink_of_a_non_concept_is_refused_not_a_panic`. An `Unlist` with no file keeps `NotFound`, as `Log` and `ListBullet` do, and `replay`'s doc says why. Publish never replays onto a file deleted since the entry's base (`replayed_text` makes that a conflict), so a missing file means the stored ops are wrong, not that the listing is already gone.

Add two variants to `ReplayOp`, after `CreateIndex`:

```rust
    /// `move_concept` and `delete_concept`: remove the first bullet of an
    /// `index.md` that links `target`.
    Unlist {
        /// Catalog-relative path the bullet lists.
        target: String,
        /// `Some`: the bullet as it read when the edit was made. A move
        /// carries that text to the new folder, so the replay refuses a
        /// listing that reads otherwise now, rather than drop someone
        /// else's change to it. `None`: whatever it reads (a delete).
        bullet: Option<String>,
    },
    /// `move_concept` and `split_concept`: redirect the links of an
    /// `index.md` or `log.md` that point into the moved document or section.
    Relink {
        /// Which links, and where to.
        redirect: Redirect,
    },
```

In `replay`, add two arms to the `match op` (after the `ListBullet` arm):

```rust
            ReplayOp::Unlist { target, bullet } => {
                let base = universal_newlines(present(path, &text)?);
                match unlist_text(&no_dirs, &base, path, target) {
                    // Already gone: nothing to do (idempotent).
                    None => None,
                    // `is_some_and` is false for `None`: a delete removes any bullet.
                    Some((_, found)) if bullet.as_deref().is_some_and(|want| want != found) => {
                        return Err(changed_listing(path, target));
                    }
                    Some((rest, _)) => Some(rest),
                }
            }
            // Byte for byte, as the op itself rewrote the file.
            ReplayOp::Relink { redirect } => {
                // `no_dirs` cannot tell a directory link (`[x](../a)`) from a file
                // link, which only matters when `from` is an `index.md`. Moves and
                // splits only redirect concepts (see `rewrite_text`'s doc). The op
                // comes from the database, so a bad one is an error in every build
                // profile, never a panic or a silently wrong rewrite.
                if !is_concept(&redirect.from) {
                    return Err(non_concept_relink(path, &redirect.from));
                }
                rewrite_text(&no_dirs, path, path, present(path, &text)?, redirect)
            }
```

Extend `replay`'s doc: in the idempotence paragraph add "an `Unlist` whose listing is gone, a `Relink` with no link left into its source"; in "# Errors" add "[`OkfError::Refused`] when a move's `Unlist` finds its listing changed since the move, or when a `Relink` redirects a non-concept", and say that `Unlist`'s `NotFound` for a missing file is deliberate (see the Corrected note above).

Add after `lists_target`:

```rust
/// The line of `text` (an `index.md` at `index`) that lists `target`: the
/// first bullet (`* ` or `- `) with a link resolving to it, as `cmd_mv`
/// takes it.
///
/// # Arguments
/// * `workspace` - resolves directory links (replay passes an empty one).
/// * `text` - the index, with universal newlines.
/// * `index` - its catalog-relative path, to resolve relative links.
/// * `target` - the catalog-relative path listed.
///
/// # Returns
/// The 0-based line index, or `None` when no bullet links `target`.
pub(crate) fn listing_line(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> Option<usize> {
    text.split('\n').position(|line| {
        is_bullet(line)
            && find_links(line).iter().any(|link| {
                resolve_link(workspace, &link.target, index)
                    .is_some_and(|resolved| resolved.path == target)
            })
    })
}

/// `cmd_mv`'s edit of the old folder's index: the text without the line
/// [`listing_line`] finds.
///
/// # Arguments
/// As [`listing_line`].
///
/// # Returns
/// The text without that line and the line itself, or `None` when no
/// bullet links `target`.
pub(crate) fn unlist_text(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> Option<(String, String)> {
    let at = listing_line(workspace, text, index, target)?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `Vec::remove(at)` takes the line out, like Python's `lines.pop(at)`.
    let bullet = lines.remove(at).to_string();
    Some((lines.join("\n"), bullet))
}

/// The refusal of a stored `Relink` whose source is not a concept: the
/// replay's empty workspace cannot resolve directory links into it.
fn non_concept_relink(path: &str, from: &str) -> OkfError {
    OkfError::Refused {
        reason: format!(
            "{path}: a stored relink redirects /{from}, which is not a concept; \
             only moves and splits of concepts record one, so discard this path \
             and redo the operation"
        ),
    }
}

/// The refusal of a move's `Unlist` whose listing changed since the move.
fn changed_listing(index: &str, target: &str) -> OkfError {
    OkfError::Refused {
        reason: format!(
            "{index}: the listing of /{target} changed since this changeset moved it; \
             discard every path of the move together (pending_changes lists them: \
             both concept paths, both folder indexes and the files whose links it \
             rewrote), or the whole changeset, then move the concept again"
        ),
    }
}
```

- [ ] **Step 4: `record_rewrites`, and the shared helper**

In `src/okf/ops/mod.rs`, the import of `paths` becomes `use crate::okf::paths::{basename, resolve_concept};`. `Redirect` and `ReplayOp` are already in scope through the module's own `pub use` lines (Task 1's `pub use relink::{..., Redirect}`, M5's `pub use replay::{..., ReplayOp}`); importing them again would be a duplicate-name error.

Add to `impl OpOutcome`, after `record`:

```rust
    /// Records the files `rewrite_links` changed: an `index.md` or `log.md`
    /// with a [`ReplayOp::Relink`], so publish can redo the rewrite on newer
    /// text (plan Decision 7); any other file as a free edit.
    ///
    /// # Arguments
    /// * `changed` - the paths [`relink::rewrite_links`] returned.
    /// * `redirect` - the redirect it applied.
    pub fn record_rewrites(&mut self, changed: &[String], redirect: &Redirect) {
        for path in changed {
            if is_bookkeeping(path) {
                // `clone`: each recorded op owns its own copy of the redirect.
                let op = ReplayOp::Relink {
                    redirect: redirect.clone(),
                };
                self.record(path, op);
            } else {
                self.touch(path);
            }
        }
    }
```

And a free function after `refused`:

```rust
/// Does publish replay edits to this file (`index.md`, `log.md`)?
fn is_bookkeeping(path: &str) -> bool {
    matches!(basename(path), "index.md" | "log.md")
}
```

In `src/okf/ops/log_index.rs`, make `collapse_whitespace` `pub(crate)` (Tasks 3 and 4 list summaries with it) and add to its doc: "Shared with `move_concept` and `split_concept`, whose listings collapse the description the same way."

In `src/okf/pystr.rs`, add after `py_truthy` (Tasks 3 and 4 both read `d.get("title") or ...`; it lands here so neither parallel task adds it):

> **Corrected (issue #121), 2026-11-03.** `pystr` is a `pub(crate)` module, so a `pub fn` with no caller fails `clippy -D warnings` (`dead_code`). `py_truthy_str` carries `#[cfg_attr(not(test), expect(dead_code, ...))]` until its first caller. Tasks 3 and 4 each delete that attribute (their first step that imports it says so): once a caller exists the expectation is unfulfilled, which `-D warnings` also refuses. Both deleting the same lines merges cleanly.

```rust
/// `str(value) if value else None` for a looked-up frontmatter value: how
/// `okf.py` reads `d.get("title") or <fallback>` when it formats the result.
///
/// # Arguments
/// * `value` - the looked-up value, `None` when the key is missing.
///
/// # Returns
/// [`py_str`] of the value when it is truthy ([`py_truthy`]), else `None`.
// `expect(dead_code)` silences the unused warning until Tasks 3 and 4 of the
// M6 plan call this; once one does, the expectation is unfulfilled, which is
// itself a warning, so that task must delete this attribute. `cfg_attr`
// applies it outside test builds only, since the unit test below uses it.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "first callers: move_concept and split_concept (M6)")
)]
pub fn py_truthy_str(value: Option<&Value>) -> Option<String> {
    if py_truthy(value) {
        value.map(py_str)
    } else {
        None
    }
}
```

and to its tests:

```rust
    #[test]
    fn py_truthy_str_is_str_of_a_truthy_value() {
        let empty = Value::Str(String::new());
        let text = Value::Str("x".to_string());
        let list = Value::Seq(vec![Value::Str("a".to_string())]);

        let results = [
            py_truthy_str(None),
            py_truthy_str(Some(&empty)),
            py_truthy_str(Some(&Value::Seq(Vec::new()))),
            py_truthy_str(Some(&text)),
            py_truthy_str(Some(&list)),
        ];

        assert_eq!(
            results,
            [None, None, None, Some("x".to_string()), Some("['a']".to_string())]
        );
    }
```

In `src/write.rs`, `merged_replay`'s `expect` text becomes `"a ReplayOp (strings, a bool and a string map) always serializes"`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- okf::ops okf::pystr`
Expected: every test passes, including the 5 new replay tests (8 with the three PR #130 added), `py_truthy_str_is_str_of_a_truthy_value` and `rewrites_of_bookkeeping_files_replay_and_the_others_are_free`.

- [ ] **Step 6: Update the design**

In "Data flow", "Write", step 4, replace "The operations are `Log { date, bullet }`, `ListBullet { target, bullet, section, create_section }` and `CreateIndex { folder }` (`okf::ops::ReplayOp`)." with:

"The operations are `Log { date, bullet }`, `ListBullet { target, bullet, section, create_section }`, `CreateIndex { folder }`, `Unlist { target, bullet }` (remove the bullet listing a path; with `bullet`, refuse a listing that changed since) and `Relink { redirect }` (redirect the links into a moved document or section, in an `index.md` or `log.md`) (`okf::ops::ReplayOp`). A move to another folder is `Unlist` on the old index and `ListBullet` on the new one."

- [ ] **Step 7: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-02-unlist-relink.md`:

```markdown
# M6 Task 2: replay: `ReplayOp::Unlist` and `ReplayOp::Relink` (<ts>)

Task 2 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `ReplayOp::Unlist { target, bullet }`: removes the first bullet of an index that
  links a path; with `bullet`, the replay refuses a listing that changed since (a
  move carries that text to the new folder).
- `ReplayOp::Relink { redirect }`: re-runs a move's or split's link rewrite on an
  `index.md` or `log.md`, byte for byte, through `relink::rewrite_text`.
- `replay::{listing_line, unlist_text}` (crate-internal) and
  `OpOutcome::record_rewrites`; 6 unit tests.

## Changed

- `log_index::collapse_whitespace` is `pub(crate)`; `pystr::py_truthy_str` (Python's
  `str(v) if v else None`) is new, with a unit test.
- `write::merged_replay`'s `expect` text names the new field types.
- Design, "Data flow", "Write" step 4: the five replay operations.

## Notes

- Publish needs no code: `replayed_text` replays whatever ops an entry stores.
- Link rewrites in `log.md` replay too (plan Decision 7): `is_bookkeeping` in
  `src/okf/ops/mod.rs` is the one place to change that.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/pystr.rs src/write.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: ReplayOp::Unlist and ReplayOp::Relink (M6.02)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `okf::ops::moves`: `move_concept`, and `mv` parity

This task ports `cmd_mv` (`okf.py:1524`):
1. **Resolve and check.** It resolves the old concept and checks the new path (`new_concept_path`).
2. **The target folder.** It makes sure the new folder has an index, listed in its parent (`ensure_index`).
3. **Take the old listing.** When the folder changes, it takes the old folder's listing bullet out (`Unlist`).
4. **Rename.** It renames the file, keeping its bytes and execute bit (`MemWorkspace::rename`).
5. **List it in the new folder.** It lists the concept there (`ListBullet`), carrying the old bullet with its link set to the new name, or building one from the frontmatter.
6. **Rewrite links.** It rewrites every link into the old path (`rewrite_links`, recorded with `record_rewrites`).

The touched list is `okf.py`'s, in its order:
- `[old, new]`;
- `ensure_index`'s files;
- the old index;
- the new index;
- `rewrite_links`' changes.

`write::apply` stores `old`, which the op removed, as a deletion entry.

It also extends the parity shim and script with `mv` steps, stubs `okf.git` in the shim (Decision 3), and ports `print_touched` into `tests/parity.rs` (Decision 12).

**Files:**
- Create: `src/okf/ops/moves.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod moves;`, `pub use moves::move_concept;`)
- Modify: `src/okf/workspace.rs` (`MemWorkspace::rename`, `placement_conflict` shared with `write_atomic`, a test)
- Modify: `tests/parity/okf_shim.py`, `tests/parity/ops_script.json`, `tests/parity.rs`
- Modify: `tests/golden/ops.json` (regenerated)
- Create: `changelog/<ts>-m6-task-03-move-concept.md`

- [ ] **Step 1: Write the failing workspace test**

Add to the `tests` module of `src/okf/workspace.rs` (its `stored(content, executable)` helper is at the top of that module):

```rust
    #[test]
    fn rename_keeps_bytes_and_execute_bit_and_refuses_a_bad_target() {
        let workspace = MemWorkspace::from_stored([
            ("a/x.md".to_string(), stored(b"x", true)),
            ("b.md".to_string(), stored(b"b", false)),
        ])
        .unwrap();

        workspace.rename("a/x.md", "c/y.md").unwrap();
        let taken = workspace.rename("c/y.md", "b.md").unwrap_err();
        let below_a_file = workspace.rename("c/y.md", "b.md/z.md").unwrap_err();
        let missing = workspace.rename("a/x.md", "d.md").unwrap_err();

        assert_eq!(workspace.stored("c/y.md"), Some(stored(b"x", true)));
        // Directories exist only as prefixes, so the emptied `a` is gone.
        assert!(!workspace.is_file("a/x.md") && !workspace.is_dir("a"));
        assert!(matches!(
            taken,
            OkfError::InvalidPath { ref reason, .. } if reason == "already exists"
        ));
        assert!(matches!(
            below_a_file,
            OkfError::InvalidPath { ref reason, .. } if reason == "a parent component is a file"
        ));
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib -- workspace::tests::rename`
Expected: compile error, `no method named rename found for struct MemWorkspace`.

- [ ] **Step 3: Implement `rename`, sharing `write_atomic`'s placement check**

In `src/okf/workspace.rs`, add after `has_key_with_prefix`:

```rust
/// Why no file can be placed at `rel`: it is a directory (a prefix of other
/// files), or one of its parent components is a file.
///
/// # Returns
/// `write_atomic`'s reason, or `None` when the path is free to hold a file.
fn placement_conflict(files: &BTreeMap<String, StoredFile>, rel: &str) -> Option<&'static str> {
    if has_key_with_prefix(files, &format!("{rel}/")) {
        Some("is a directory")
    } else {
        // Any proper prefix of the path that is a file blocks it.
        rel.match_indices('/')
            .any(|(i, _)| files.contains_key(&rel[..i]))
            .then_some("a parent component is a file")
    }
}
```

In `impl Workspace for MemWorkspace`, `write_atomic`'s check becomes:

```rust
        let rel = checked_file(path)?;
        let mut files = self.files.write().expect("MemWorkspace lock poisoned");
        // `&files` derefs from the lock guard to the map it guards.
        if let Some(reason) = placement_conflict(&files, &rel) {
            return Err(OkfError::InvalidPath {
                path: path.to_string(),
                reason: reason.to_string(),
            });
        }
```

(the `dir_prefix` and `conflict` locals are gone; the rest of `write_atomic` is unchanged).

Add to `impl MemWorkspace`, after `remove`:

```rust
    /// Moves a file to a new path, keeping its bytes and execute bit, as
    /// `git mv` keeps a file's mode (`move_concept`).
    ///
    /// Not part of [`Workspace`], for the reason [`MemWorkspace::remove`]
    /// gives.
    ///
    /// # Arguments
    /// * `from` - the file's catalog-relative path.
    /// * `to` - its new path.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if either path escapes the catalog or is
    /// the root, or `to` already exists, is a directory, or sits below a
    /// file; [`OkfError::NotFound`] if there is no file at `from`.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), OkfError> {
        let (from_rel, to_rel) = (checked_file(from)?, checked_file(to)?);
        let mut files = self.files.write().expect("MemWorkspace lock poisoned");
        // `or_else` only runs the placement check when the path is not taken.
        let taken = files.contains_key(&to_rel).then_some("already exists");
        if let Some(reason) = taken.or_else(|| placement_conflict(&files, &to_rel)) {
            return Err(OkfError::InvalidPath {
                path: to.to_string(),
                reason: reason.to_string(),
            });
        }
        let file = files.remove(&from_rel).ok_or_else(|| OkfError::NotFound {
            path: from.to_string(),
        })?;
        files.insert(to_rel, file);
        Ok(())
    }
```

Run: `cargo test --lib -- workspace`
Expected: every workspace test passes, the new one and the existing `write_atomic` refusals included.

- [ ] **Step 4: Write the failing op tests**

Create `src/okf/ops/moves.rs` with this test module (Step 6 adds the code above it). The expected texts are what `okf.py`'s `cmd_mv` writes for the same files (probed 2026-11-03):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::replay::replay;
    use std::collections::BTreeMap;

    const INFRA_INDEX: &str =
        "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n* [NTP](ntp.md) - sync\n";
    const ROOT_INDEX: &str = "# Root\n\n# Sections\n\n* [Infra](infra/) - systems\n";

    const FILES: [(&str, &str); 7] = [
        ("index.md", ROOT_INDEX),
        ("infra/index.md", INFRA_INDEX),
        (
            "infra/time.md",
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](ntp.md).\n",
        ),
        (
            "infra/ntp.md",
            "---\ntype: System\ntitle: NTP\ndescription: How   hosts sync.\n---\n\
             Uses [time](/infra/time.md#ports).\n",
        ),
        (
            "infra/unlisted.md",
            "---\ntype: Note\ntitle: Unlisted\ndescription: Not   listed.\n---\nAlone.\n",
        ),
        ("people/index.md", "# People\n"),
        ("log.md", "# Log\n"),
    ];

    fn text(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn a_move_within_a_folder_relinks_its_listing_and_every_link() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "infra/time.md", "infra/clock.md").unwrap();

        assert_eq!(outcome.message, "moved /infra/time.md -> /infra/clock.md");
        assert_eq!(
            outcome.touched,
            ["infra/time.md", "infra/clock.md", "infra/index.md", "infra/ntp.md"]
        );
        assert!(outcome.notes.is_empty());
        assert!(!workspace.is_file("infra/time.md"));
        assert_eq!(
            text(&workspace, "infra/clock.md"),
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](/infra/ntp.md).\n"
        );
        assert_eq!(
            text(&workspace, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n* [Time](clock.md) - the clock\n* [NTP](ntp.md) - sync\n"
        );
        assert_eq!(
            text(&workspace, "infra/ntp.md"),
            "---\ntype: System\ntitle: NTP\ndescription: How   hosts sync.\n---\n\
             Uses [time](/infra/clock.md#ports).\n"
        );
        let relink = ReplayOp::Relink {
            redirect: Redirect {
                from: "infra/time.md".to_string(),
                to: "infra/clock.md".to_string(),
                anchors: None,
            },
        };
        assert_eq!(
            outcome.replay,
            BTreeMap::from([("infra/index.md".to_string(), vec![relink])])
        );
    }

    #[test]
    fn a_move_to_a_new_folder_carries_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "/infra/ntp", "ops/net/ntp.md").unwrap();

        assert_eq!(outcome.message, "moved /infra/ntp.md -> /ops/net/ntp.md");
        assert_eq!(
            outcome.touched,
            [
                "infra/ntp.md",
                "ops/net/ntp.md",
                "ops/net/index.md",
                "ops/index.md",
                "index.md",
                "infra/index.md",
                "infra/time.md"
            ]
        );
        assert_eq!(
            outcome.notes,
            [
                "created /ops/index.md; replace its TODO lines (and the parent's listing)",
                "created /ops/net/index.md; replace its TODO lines (and the parent's listing)"
            ]
        );
        assert_eq!(
            text(&workspace, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n"
        );
        assert_eq!(
            text(&workspace, "ops/net/index.md"),
            "# Net\n\nTODO: say what this folder holds.\n\n# Concepts\n\n* [NTP](ntp.md) - sync\n"
        );
        // A relative link stays relative, through `relpath`.
        assert_eq!(
            text(&workspace, "infra/time.md"),
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](../ops/net/ntp.md).\n"
        );
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::Unlist {
                target: "infra/ntp.md".to_string(),
                bullet: Some("* [NTP](ntp.md) - sync".to_string()),
            }]
        );
        assert_eq!(
            outcome.replay["ops/net/index.md"],
            [
                ReplayOp::CreateIndex {
                    folder: "ops/net".to_string()
                },
                ReplayOp::ListBullet {
                    target: "ops/net/ntp.md".to_string(),
                    bullet: "* [NTP](ntp.md) - sync".to_string(),
                    section: None,
                    create_section: false,
                },
            ]
        );
        // The moved concept and the file whose link changed are free edits.
        assert!(!outcome.replay.contains_key("ops/net/ntp.md"));
        assert!(!outcome.replay.contains_key("infra/time.md"));
    }

    #[test]
    fn an_unlisted_concept_is_listed_from_its_frontmatter() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "infra/unlisted.md", "people/Unlisted.md").unwrap();

        assert_eq!(
            outcome.touched,
            ["infra/unlisted.md", "people/Unlisted.md", "people/index.md"]
        );
        assert_eq!(
            outcome.notes,
            ["note: Unlisted.md isn't short, lowercase and hyphenated"]
        );
        assert_eq!(
            text(&workspace, "people/index.md"),
            "# People\n\n# Concepts\n\n* [Unlisted](Unlisted.md) - Not listed.\n"
        );
        assert_eq!(text(&workspace, "infra/index.md"), INFRA_INDEX);
    }

    #[test]
    fn every_bookkeeping_edit_replays_onto_the_text_it_was_made_on() {
        let workspace = MemWorkspace::from_files(FILES);
        let original: BTreeMap<&str, &str> = FILES.into_iter().collect();

        let outcome = move_concept(&workspace, "infra/ntp.md", "ops/net/ntp.md").unwrap();

        assert_eq!(outcome.replay.len(), 4, "{:?}", outcome.replay.keys());
        for (path, ops) in &outcome.replay {
            // `copied`: the map holds `&str`s; `get` gives `Option<&&str>`.
            let before = original.get(path.as_str()).copied();
            let replayed = replay(path, before, ops).unwrap();
            assert_eq!(replayed, text(&workspace, path), "{path}");
        }
    }

    #[test]
    fn a_taken_reserved_or_missing_path_is_refused_before_anything_is_written() {
        let workspace = MemWorkspace::from_files(FILES);

        let taken = move_concept(&workspace, "infra/time.md", "infra/ntp.md").unwrap_err();
        let reserved = move_concept(&workspace, "infra/time.md", "infra/index.md").unwrap_err();
        let missing = move_concept(&workspace, "infra/none.md", "infra/x.md").unwrap_err();

        assert_eq!(taken.to_string(), "infra/ntp.md: already exists");
        assert_eq!(
            reserved.to_string(),
            "infra/index.md: index.md is a reserved name, not a concept"
        );
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert_eq!(text(&workspace, "infra/index.md"), INFRA_INDEX);
        assert!(workspace.is_file("infra/time.md"));
    }
}
```

- [ ] **Step 5: Run them to see them fail**

Add `pub mod moves;` to `src/okf/ops/mod.rs`. Run: `cargo test --lib -- okf::ops::moves`
Expected: compile error, `cannot find function move_concept`.

- [ ] **Step 6: Implement `move_concept`**

> **Corrected (issue #122, review of PR #131), 2026-11-03.** `new_concept_path` does not check placement, so a `new` below an existing file (`infra/time.md/x.md`) used to fail first in `ensure_index`'s write, naming an index path the caller never gave. `move_concept` now calls `workspace.check_placement(&new)?` right after `new_concept_path`, so it refuses with `InvalidPath { path: new, reason: "a parent component is a file" }` before anything is written. `MemWorkspace::check_placement` (Interfaces block) wraps `placement_conflict`. The module doc lists this as a third divergence (`okf.py`'s `os.makedirs` raises `FileExistsError`). Three more unit tests cover it and the two documented divergences (`FrontmatterParse`, `NotFound` for a missing root `index.md`), and the workspace tests cover `rename`'s directory-target refusal and `check_placement`. `src/okf/ops/moves.rs` is the code to read.

> **Corrected (issue #121), 2026-11-03.** `move_concept` is `py_truthy_str`'s first caller (with Task 4's `split_concept`). In `src/okf/pystr.rs`, delete the `#[cfg_attr(not(test), expect(dead_code, ...))]` attribute and the four-line comment above it on `py_truthy_str` (Task 2 added them until a caller existed). With this caller the expectation is unfulfilled, and `clippy -D warnings` fails until they go. If Task 3 or 4 has already merged and deleted them, there is nothing to do.

Put this above the test module in `src/okf/ops/moves.rs`:

```rust
//! `move_concept`: port of `okf.py`'s `cmd_mv` (`:1524`). A concept moves
//! to a new path: its folder listing goes with it, and every link into the
//! old path is rewritten ([`rewrite_links`]).
//!
//! What publish replays (plan Decision 1): the old folder's [`ReplayOp::Unlist`],
//! the new folder's `ensure_index` and [`ReplayOp::ListBullet`], and the
//! [`ReplayOp::Relink`] of every `index.md` and `log.md` whose links
//! changed. The moved concept and every other file whose links changed are
//! free edits. The old path is removed, which `write::apply` stores as a
//! deletion.
//!
//! Divergences from `okf.py` (each where `okf.py` crashes):
//! - an unlisted concept whose frontmatter does not parse is
//!   [`OkfError::FrontmatterParse`] when its new listing is built (`okf.py`
//!   raises `YamlError`);
//! - a move into the root of a catalog without a root `index.md` is
//!   [`OkfError::NotFound`] (`okf.py`: `FileNotFoundError`).
//!
//! `okf.py` runs `git mv` for a tracked file and `os.rename` otherwise;
//! [`MemWorkspace::rename`] keeps the bytes and execute bit, as both do.

use crate::error::OkfError;
use crate::okf::doc::Doc;
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::ops::log_index::collapse_whitespace;
use crate::okf::ops::relink::{rewrite_links, Moved, Redirect};
use crate::okf::ops::replay::{insert_index_bullet_text, unlist_text, ReplayOp};
use crate::okf::ops::{ensure_index, OpOutcome};
use crate::okf::paths::{basename, dirname, join, new_concept_path, resolve_concept};
use crate::okf::pystr::py_truthy_str;
use crate::okf::workspace::{MemWorkspace, Workspace};

/// Port of `cmd_mv`: moves a concept and rewrites every link to it.
///
/// # Arguments
/// * `workspace` - the catalog (the write path's scratch copy of an overlay).
/// * `old` - the concept, `/dir/x.md`, `dir/x` or `/dir/x`.
/// * `new` - its new path, with or without `.md` and a leading `/`.
///
/// # Returns
/// The outcome: touched paths in `okf.py`'s order (the old path, now
/// removed, first), the replay ops of the index and log edits, the message
/// `moved /<old> -> /<new>`, and the notes (a non-kebab-case name, folder
/// indexes created).
///
/// # Errors
/// - As [`resolve_concept`] for `old`.
/// - As [`new_concept_path`] for `new` (outside the catalog, reserved,
///   machinery, taken).
/// - [`OkfError::FrontmatterParse`] for an unlisted concept whose
///   frontmatter does not parse.
/// - The workspace's read or write error.
///
/// The path checks come before anything is written. Run the op on a
/// scratch copy, as the write path does, and drop the copy on error.
pub fn move_concept(
    workspace: &MemWorkspace,
    old: &str,
    new: &str,
) -> Result<OpOutcome, OkfError> {
    let old = resolve_concept(workspace, old)?;
    let (new, note) = new_concept_path(workspace, new)?;
    let mut outcome = OpOutcome::default();
    // `Option` is iterable (zero or one item), so `extend` adds the note if any.
    outcome
        .notes
        .extend(note.map(|note| format!("note: {note}")));
    let (old_dir, new_dir) = (dirname(&old), dirname(&new));
    outcome.touch(&old);
    outcome.touch(&new);
    ensure_index(workspace, new_dir, &mut outcome)?;

    // The old folder's listing travels with the concept, summary included.
    let old_index = join(old_dir, "index.md");
    let mut carried = None;
    if old_dir != new_dir && workspace.exists(&old_index) {
        let text = workspace.read_text(&old_index)?;
        if let Some((rest, bullet)) = unlist_text(workspace, &text, &old_index, &old) {
            workspace.write_atomic(&old_index, &rest)?;
            let op = ReplayOp::Unlist {
                target: old.clone(),
                bullet: Some(bullet.clone()),
            };
            outcome.record(&old_index, op);
            carried = Some(bullet);
        }
    }

    workspace.rename(&old, &new)?;

    if old_dir != new_dir {
        let name = basename(&new);
        let bullet = match carried {
            Some(bullet) => relinked_bullet(workspace, &bullet, &old_index, &old, name),
            None => frontmatter_bullet(workspace, &new)?,
        };
        let new_index = join(new_dir, "index.md");
        let text = workspace.read_text(&new_index)?;
        // `okf.py` inserts without checking that the index lists it already
        // (plan Decision 15); the replay op skips an index that does.
        let listed = insert_index_bullet_text(&text, &bullet, None, false)
            .expect("without a section a bullet always finds a place");
        workspace.write_atomic(&new_index, &listed)?;
        let op = ReplayOp::ListBullet {
            target: new.clone(),
            bullet,
            section: None,
            create_section: false,
        };
        outcome.record(&new_index, op);
    }

    let redirect = Redirect {
        from: old.clone(),
        to: new.clone(),
        anchors: None,
    };
    let moved = Moved {
        path: &new,
        written_at: &old,
    };
    let changed = rewrite_links(workspace, &redirect, Some(moved))?;
    outcome.record_rewrites(&changed, &redirect);
    outcome.message = format!("moved /{old} -> /{new}");
    Ok(outcome)
}

/// The old listing with its link into `old` pointing at `name` instead, the
/// fragment kept: `cmd_mv`'s `LINK_RE.sub` over the carried bullet.
///
/// As in `okf.py`, the first `(<target>` in the link's text is replaced, so
/// a link whose text itself holds `(<target>` changes there.
fn relinked_bullet(
    workspace: &dyn Workspace,
    bullet: &str,
    index: &str,
    old: &str,
    name: &str,
) -> String {
    let mut out = String::with_capacity(bullet.len());
    let mut copied = 0;
    for link in find_links(bullet) {
        // `is_none_or`: skip a link that does not resolve, or resolves elsewhere.
        if resolve_link(workspace, &link.target, index).is_none_or(|resolved| resolved.path != old)
        {
            continue;
        }
        let fragment = link
            .target
            .split_once('#')
            .map_or(String::new(), |(_, fragment)| format!("#{fragment}"));
        // `clone`: a `Range` is not `Copy`, and slicing would move it.
        let whole = &bullet[link.range.clone()];
        out.push_str(&bullet[copied..link.range.start]);
        out.push_str(&whole.replacen(
            &format!("({}", link.target),
            &format!("({name}{fragment}"),
            1,
        ));
        copied = link.range.end;
    }
    out.push_str(&bullet[copied..]);
    out
}

/// A listing built from the moved concept's frontmatter, for a concept its
/// old folder did not list: `* [<title, else the file stem>](<name>) -
/// <description, whitespace collapsed>`.
///
/// # Errors
/// [`OkfError::FrontmatterParse`] when the frontmatter does not parse; the
/// workspace's read error.
fn frontmatter_bullet(workspace: &dyn Workspace, new: &str) -> Result<String, OkfError> {
    let data = Doc::from_text(new, &workspace.read_to_string(new)?).parse_checked()?;
    let name = basename(new);
    // `data.get("title") or basename[:-3]`, formatted with `str()`.
    let title = py_truthy_str(data.get("title"))
        .unwrap_or_else(|| name.strip_suffix(".md").unwrap_or(name).to_string());
    let description = py_truthy_str(data.get("description")).unwrap_or_default();
    Ok(format!(
        "* [{title}]({name}) - {}",
        collapse_whitespace(&description)
    ))
}
```

In `src/okf/ops/mod.rs`, add `pub use moves::move_concept;` after the `log_index` re-export, and append `move_concept` to the module doc's list of restructuring edits ("...the restructuring edits (M6): `rewrite_links` (`relink`), `move_concept`..."). Keep both sides when Task 4 adds `split_concept` to the same list.

- [ ] **Step 7: Run the tests**

Run: `cargo test --lib -- okf::ops::moves workspace`
Expected: the 5 move tests and every workspace test pass.

- [ ] **Step 8: Parity: the shim**

In `tests/parity/okf_shim.py`:
- add `import subprocess` after `import shutil`;
- add `"mv": {},` to `OPS_DEFAULTS` after `"ensure_index": {},`;
- add a typed stub before `ops()`, and install it right after `setattr(okf, "now_utc", lambda: fixed)`:

> **Corrected (issue #122), 2026-11-03.** The stub is a named, annotated function rather than a 100-character lambda, so it carries types (mypy cannot annotate a lambda) and fits the line length.

```python
def untracked_git(
    _root: str, *args: str, check: bool = False
) -> "subprocess.CompletedProcess[str]":
    """okf.py's `git` for a copy no repository tracks: every call fails (code 1)."""
    return subprocess.CompletedProcess(list(args), 1, "", "")
```

```python
    # cmd_mv runs `git mv` for a tracked file, else os.rename. The copy is
    # never tracked, so answer `git ls-files --error-unmatch` with "no"
    # (return code 1) without running git: the run needs no git binary and
    # never sees an enclosing repository.
    setattr(okf, "git", untracked_git)
```

and add to the docstring's `ops` line: "mv runs with git stubbed out (os.rename)".

- [ ] **Step 9: Parity: the script**

In `tests/parity/ops_script.json`, add to `"setup"`:

```json
    "moves/index.md": "# Moves\n\n# Concepts\n\n* [Alpha](alpha.md) - first letter\n* [Beta](beta.md) - second\n\nSee also [alpha again](alpha.md).\n",
    "moves/alpha.md": "---\ntype: Note\ntitle: Alpha\n---\nSee [beta](beta.md), [own part](#part) and [hub](/analysis/hub.md).\n\n## Part\n\nText.\n",
    "moves/beta.md": "---\ntype: Note\ntitle: Beta\ndescription: The   second letter.\n---\nBack to [alpha](alpha.md#part) and [abs](/moves/alpha.md).\r\n",
    "moves/gamma.md": "---\ntype: Note\ntitle: Gamma\ndescription: Third.\n---\nUnlisted; [beta](/moves/beta.md).\n",
    "people/linker.md": "---\ntype: Note\ntitle: Linker\n---\nSee [alpha](../moves/alpha.md#part), [beta](/moves/beta.md) and [folder](/moves/).\n"
```

and append to `"steps"`:

```json
    {"cmd": "log", "text": "Linked [alpha](/moves/alpha.md).", "kind": "Update"},
    {"cmd": "mv", "old": "moves/alpha.md", "new": "moves/renamed.md"},
    {"cmd": "mv", "old": "/moves/beta", "new": "/elsewhere/new-folder/beta.md"},
    {"cmd": "mv", "old": "moves/gamma.md", "new": "infra/Gamma_Ray.md"},
    {"cmd": "mv", "old": "moves/renamed.md", "new": "moves/index.md"}
```

They cover:
- a rename within a folder, with a log line, a relative, an absolute and an in-page link, and a `\r\n` line;
- a move into two new folders: carried listing, two `ensure_index` notes, the root index's `# Sections`;
- an unlisted concept listed from its frontmatter, with the naming note;
- the reserved-name refusal.

A missing concept and a taken target stay out (Decision 13).

- [ ] **Step 10: Parity: the Rust side**

In `tests/parity.rs`:
- add `move_concept` to the `okf_mcp::okf::ops` import;
- in the module doc's ops paragraph, after "byte for byte.", add: "For `mv` and `split` the output also carries `okf.py`'s touched list, rebuilt from `OpOutcome::touched` by [`print_touched`] (the op's message leaves it out, since the tool returns `touched` itself)";
- add before `run_op`:

```rust
/// `okf.py`'s `print_touched` (`:1514`): the paths under its heading, one
/// per line. `OpOutcome::touched` is already de-duplicated in first-seen
/// order, as `print_touched` de-duplicates.
fn print_touched(touched: &[String]) -> String {
    let mut out = "touched (pass these to publish):".to_string();
    for path in touched {
        out.push_str("\n  ");
        out.push_str(path);
    }
    out
}

/// An `mv` or `split` outcome as `okf.py` prints it: the message, then the
/// touched list.
fn with_touched(mut outcome: OpOutcome) -> OpOutcome {
    outcome.message = format!("{}\n{}", outcome.message, print_touched(&outcome.touched));
    outcome
}
```

- add an arm to `run_op`'s `match`, before `other => panic!`:

```rust
        "mv" => move_concept(workspace, text("old"), text("new")).map(with_touched),
```

- [ ] **Step 11: Regenerate the golden and run the parity tests**

Run: `UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens`

Expected:
- 1 passed;
- `git diff --stat tests/golden` shows only `ops.json`;
- it has 47 steps, 9 of them errors. The new one, step 47, is `moves/index.md: index.md is a reserved name, not a concept`;
- step 44's output is `moved /moves/alpha.md -> /moves/renamed.md`, then the touched list `moves/alpha.md`, `moves/renamed.md`, `log.md`, `moves/beta.md`, `moves/index.md`, `people/linker.md`;
- step 45 has two notes, `/elsewhere` first, then `/elsewhere/new-folder`;
- 28 changed files, among them `moves/alpha.md` with `null` (removed), and `log.md`, whose 2026-10-27 line now links `/moves/renamed.md`.

If Task 4 merged first, the expected counts are 52 steps, 11 errors and 35 files (verified fact 7).

Run: `cargo test --test parity` then `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
Expected: every test passes, including `ops_match_golden` and `live_ops_parity`.

- [ ] **Step 12: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-03-move-concept.md`:

```markdown
# M6 Task 3: `okf::ops::moves`: `move_concept`, and `mv` parity (<ts>)

Task 3 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/okf/ops/moves.rs`: `move_concept`, a port of okf.py's `cmd_mv`: the old
  listing is taken (`Unlist`, with the bullet it read), the file renamed, the
  listing added in the new folder (`ensure_index`, `ListBullet`), and every link
  rewritten (`Relink` on index.md and log.md, free edits elsewhere); 5 unit tests.
- `MemWorkspace::rename`, keeping bytes and the execute bit; 1 unit test.
- Parity: 5 setup files and 5 steps (a log line and four moves, one refused); the
  shim stubs `okf.git` (the copy is never tracked, so `cmd_mv` renames); the Rust
  side appends okf.py's `print_touched` block; `ops.json` regenerated.

## Changed

- `MemWorkspace::write_atomic` shares its placement check (`placement_conflict`)
  with `rename`.

## Notes

- The op's message is okf.py's first line; the tool returns `touched` itself.
- okf.py runs the shim's `mv` without git either way: verified while planning, the
  stubbed and unstubbed runs printed identical JSON.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/workspace.rs tests/parity/okf_shim.py tests/parity/ops_script.json tests/parity.rs tests/golden/ops.json changelog/
git commit -m "feat: move_concept, a port of okf.py's mv, with byte parity (M6.03)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `okf::ops::split`: `split_concept`, and `split` parity

This task ports `cmd_split` (`okf.py:1589`) and `find_section` (`:1577`). A section of a concept, with its subsections, becomes a new concept:
- **The new concept.** Its frontmatter has `type`, `title` (default: the heading), `description`, the parent's `status`, `generated` now, and the sources the section cites. Its body is an intro line linking back, the section's content with headings re-levelled to start at `#`, then the footnote definitions it cites.
- **The parent** keeps the heading with a stub paragraph linking to the new concept. Footnote definitions stay where something still cites them.
- **Links** to the section and its subsections are rewritten across the catalog (a `Redirect` with `anchors`).
- **The listing.** The new concept is listed in its folder's `index.md`.

The parent and the new concept are free edits. `ensure_index`'s files and the listing record replay ops, and so do `index.md` and `log.md` files whose links changed (`record_rewrites`).

The task also adds the two markdown helpers the port needs:
- `footnote_refs`: `FOOTREF_RE`, whose `(?!:)` lookahead is done by hand;
- `atx_heading`: `HEADING_RE`'s groups, which `headings` now shares.

It extends the parity shim and script with `split` steps.

**Files:**
- Create: `src/okf/ops/split.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod split;`, `pub use split::{split_concept, Split, DEFAULT_SPLIT_TYPE};`)
- Modify: `src/okf/markdown.rs` (`FOOTNOTE_REF`, `footnote_refs`, `atx_heading`; `headings` uses `atx_heading`; tests)
- Modify: `tests/parity/okf_shim.py`, `tests/parity/ops_script.json`, `tests/parity.rs`
- Modify: `tests/golden/ops.json` (regenerated)
- Create: `changelog/<ts>-m6-task-04-split-concept.md`

- [ ] **Step 1: Write the failing markdown tests**

Add to the `tests` module of `src/okf/markdown.rs`:

```rust
    #[test]
    fn footnote_refs_skip_definitions() {
        let text = "A[^a] and [^b c]. [^d]: def\n[^a]: again [^e]";

        let refs = footnote_refs(text);

        let expected = BTreeSet::from(["a".to_string(), "b c".to_string(), "e".to_string()]);
        assert_eq!(refs, expected);
    }

    #[test]
    fn atx_heading_keeps_the_text_as_written() {
        let cases = [
            atx_heading("### Detail ##"),
            atx_heading("#"),
            atx_heading("#nospace"),
            atx_heading("####### seven"),
        ];

        assert_eq!(cases, [Some((3, "Detail")), Some((1, "")), None, None]);
    }
```

Run: `cargo test --lib -- markdown::tests::footnote_refs markdown::tests::atx_heading`
Expected: compile errors, `cannot find function footnote_refs`, `atx_heading`.

- [ ] **Step 2: Implement the markdown helpers**

In `src/okf/markdown.rs`, add beside `FOOTNOTE_DEF`:

```rust
/// A footnote reference `[^label]` (`okf.py`'s `FOOTREF_RE` without its
/// `(?!:)` lookahead, which [`footnote_refs`] applies by hand).
static FOOTNOTE_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\^([^\]]+)\]").expect("FOOTNOTE_REF regex is valid"));
```

Add after `footnote_defs`:

```rust
/// The footnote labels `text` cites: `set(FOOTREF_RE.findall(text))`,
/// every `[^label]` that is not followed by `:` (which makes it a
/// definition).
///
/// # Arguments
/// * `text` - any markdown (several lines joined with `\n` is fine).
///
/// # Returns
/// The labels, sorted (a `BTreeSet`, so callers iterate deterministically).
///
/// # Examples
/// ```
/// use okf_mcp::okf::markdown::footnote_refs;
/// let refs = footnote_refs("Cited.[^a]\n\n[^a]: The definition.");
/// assert_eq!(refs.into_iter().collect::<Vec<_>>(), ["a"]);
/// ```
pub fn footnote_refs(text: &str) -> BTreeSet<String> {
    // Python's `(?!:)` rejects a match followed by `:`, and Python then tries
    // every later start inside it. Each of those ends at the same `]` (a
    // label holds no `]`), so each is rejected too: skipping the whole match,
    // as `captures_iter` does, finds the same labels.
    FOOTNOTE_REF
        .captures_iter(text)
        .filter(|caps| {
            let end = caps.get(0).map_or(text.len(), |whole| whole.end());
            !text[end..].starts_with(':')
        })
        .map(|caps| caps[1].to_string())
        .collect()
}

/// An ATX heading line's level and its text as written: `HEADING_RE`'s
/// groups 1 and 2, the text not stripped (`cmd_split` re-levels with it).
///
/// # Arguments
/// * `line` - one line, without its `\n`.
///
/// # Returns
/// `(number of #, text)` (`""` for a bare `#`), or `None` for any other line.
/// It does not know about fences: [`headings`] skips fenced code itself.
pub(crate) fn atx_heading(line: &str) -> Option<(usize, &str)> {
    // `?` on an `Option`: a line that is not a heading returns `None` here.
    let caps = HEADING.captures(line)?;
    // Group 1 (the `#`s) is mandatory; group 2 is absent for a bare `#`.
    Some((caps[1].len(), caps.get(2).map_or("", |text| text.as_str())))
}
```

In `headings`, replace the `if let Some(caps) = HEADING.captures(line) { ... }` block with:

```rust
        if let Some((level, text)) = atx_heading(line) {
            found.push(Heading {
                index,
                level,
                text: py_strip(text).to_string(),
                slug: String::new(),
            });
        }
```

Run: `cargo test --lib -- markdown` then `cargo test --test parity -- markdown_matches_golden`
Expected: every markdown test passes, the two new ones and the golden included.

- [ ] **Step 3: Write the failing op tests**

Create `src/okf/ops/split.rs` with this test module (Step 5 adds the code above it). The expected texts are what `okf.py`'s `cmd_split` writes for the same files, with `now_utc` frozen at 2026-11-03T12:00:00Z and `--by tester` (probed 2026-11-03):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::replay::replay;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    const PARENT: &str = concat!(
        "---\ntype: Note\ntitle: Parent\nstatus: draft\nsources:\n",
        "  - id: s1\n    resource: r1\n    title: One\n",
        "  - id: s2\n    resource: https://example.com/two\n---\n",
        "# Intro\n\nOpening.[^n0]\n\n## Background\n\nWhy it exists.[^s1]\n\n",
        "### Detail\n\nFine print, see [intro](#intro).\n\n## Other\n\nUnrelated.[^n2]\n\n",
        "## Last bit\n\nThe end.[^s1]\n\n[^n0]: Zero.\n[^s1]: From s1.\n[^n2]: Two.\n",
    );
    const SPLITS_INDEX: &str = "# Splits\n\n# Concepts\n\n* [Parent](parent.md) - the parent\n";
    const USER: &str = concat!(
        "---\ntype: Note\ntitle: User\n---\nSee [bg](/splits/parent.md#background), ",
        "[detail](parent.md#Detail), [other](/splits/parent.md#other) and ",
        "[whole](/splits/parent.md).\n",
    );
    const FILES: [(&str, &str); 4] = [
        ("splits/index.md", SPLITS_INDEX),
        ("splits/parent.md", PARENT),
        ("splits/user.md", USER),
        ("index.md", "# Root\n"),
    ];

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    /// `okf.py split`'s arguments with its defaults, for one section.
    fn split<'a>(section: &'a str, to: &'a str) -> Split<'a> {
        Split {
            concept: "splits/parent.md",
            section,
            to,
            kind: DEFAULT_SPLIT_TYPE,
            title: None,
            description: None,
            stub: None,
            sources: &[],
        }
    }

    fn text(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn a_section_with_a_subsection_and_a_cited_footnote_becomes_a_concept() {
        let workspace = MemWorkspace::from_files(FILES);
        let args = Split {
            description: Some("Why the parent exists."),
            ..split("Background", "splits/background.md")
        };

        let outcome = split_concept(&workspace, &ctx(), &args).unwrap();

        assert_eq!(
            outcome.message,
            concat!(
                "split 'Background' (5 lines) out of /splits/parent.md into /splits/background.md\n",
                "sources copied: s1 (cited by the section's footnotes)\n",
                "footnotes carried: s1",
            )
        );
        assert_eq!(
            outcome.touched,
            ["splits/parent.md", "splits/background.md", "splits/user.md", "splits/index.md"]
        );
        assert!(outcome.notes.is_empty(), "{:?}", outcome.notes);
        assert_eq!(
            text(&workspace, "splits/background.md"),
            concat!(
                "---\ntype: Note\ntitle: Background\ndescription: Why the parent exists.\n",
                "status: draft\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n",
                "sources:\n  - id: s1\n    resource: r1\n    title: One\n---\n\n",
                "Split out of [Parent](/splits/parent.md); see it for the surrounding context.\n\n",
                "Why it exists.[^s1]\n\n# Detail\n\n",
                "Fine print, see [intro](/splits/parent.md#intro).\n\n[^s1]: From s1.\n",
            )
        );
        // `[^s1]` stays in the parent: "Last bit" still cites it.
        assert_eq!(
            text(&workspace, "splits/parent.md"),
            concat!(
                "---\ntype: Note\ntitle: Parent\nstatus: draft\nsources:\n",
                "  - id: s1\n    resource: r1\n    title: One\n",
                "  - id: s2\n    resource: https://example.com/two\n---\n",
                "# Intro\n\nOpening.[^n0]\n\n## Background\n\n",
                "[Background](/splits/background.md): Why the parent exists.\n\n",
                "## Other\n\nUnrelated.[^n2]\n\n## Last bit\n\nThe end.[^s1]\n\n",
                "[^n0]: Zero.\n[^s1]: From s1.\n[^n2]: Two.\n",
            )
        );
        assert_eq!(
            text(&workspace, "splits/user.md"),
            concat!(
                "---\ntype: Note\ntitle: User\n---\nSee [bg](/splits/background.md), ",
                "[detail](background.md#detail), [other](/splits/parent.md#other) and ",
                "[whole](/splits/parent.md).\n",
            )
        );
        assert_eq!(
            text(&workspace, "splits/index.md"),
            concat!(
                "# Splits\n\n# Concepts\n\n* [Parent](parent.md) - the parent\n",
                "* [Background](background.md) - Why the parent exists.\n",
            )
        );
        assert_eq!(outcome.replay.len(), 1);
        assert_eq!(
            outcome.replay["splits/index.md"],
            [ReplayOp::ListBullet {
                target: "splits/background.md".to_string(),
                bullet: "* [Background](background.md) - Why the parent exists.".to_string(),
                section: None,
                create_section: false,
            }]
        );
    }

    #[test]
    fn the_last_section_into_a_new_folder_with_a_named_source() {
        let workspace = MemWorkspace::from_files(FILES);
        let named = ["s2".to_string()];
        let args = Split {
            sources: &named,
            ..split("last-bit", "/splits/deeper/last.md")
        };

        let outcome = split_concept(&workspace, &ctx(), &args).unwrap();

        assert_eq!(
            outcome.message,
            concat!(
                "split 'Last bit' (1 lines) out of /splits/parent.md into /splits/deeper/last.md\n",
                "sources copied: s2 (named with --source)\n",
                "footnotes carried: s1",
            )
        );
        assert_eq!(
            outcome.notes,
            [
                "note: 'Last bit' is the last section, so it runs to the end of the document; \
                 check that its closing paragraphs really belong to it and move any that \
                 don't back",
                "note: no --description given; write a real one in the new concept and its \
                 index listing",
                "created /splits/deeper/index.md; replace its TODO lines (and the parent's \
                 listing)",
            ]
        );
        assert_eq!(
            outcome.touched,
            [
                "splits/parent.md",
                "splits/deeper/last.md",
                "splits/deeper/index.md",
                "splits/index.md"
            ]
        );
        assert_eq!(
            text(&workspace, "splits/deeper/last.md"),
            concat!(
                "---\ntype: Note\ntitle: Last bit\ndescription: Last bit, split out of Parent.\n",
                "status: draft\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n",
                "sources:\n  - id: s2\n    resource: https://example.com/two\n---\n\n",
                "Split out of [Parent](/splits/parent.md); see it for the surrounding context.\n\n",
                "The end.[^s1]\n\n[^s1]: From s1.\n",
            )
        );
        assert_eq!(
            text(&workspace, "splits/deeper/index.md"),
            concat!(
                "# Deeper\n\nTODO: say what this folder holds.\n\n# Concepts\n\n",
                "* [Last bit](last.md) - Last bit, split out of Parent.\n",
            )
        );
        assert!(text(&workspace, "splits/parent.md")
            .contains("## Last bit\n\nSee [Last bit](/splits/deeper/last.md).\n\n[^n0]: Zero.\n"));
    }

    #[test]
    fn a_stub_a_title_and_a_type_and_every_source_when_none_is_cited() {
        let workspace = MemWorkspace::from_files(FILES);
        let args = Split {
            kind: "Topic",
            title: Some("Other [things]"),
            stub: Some("Moved away."),
            ..split("Other", "splits/other.md")
        };

        let outcome = split_concept(&workspace, &ctx(), &args).unwrap();

        assert!(outcome.message.contains(
            "sources copied: s1, s2 (all of the parent's (none was cited by footnote; prune them))"
        ));
        assert!(outcome.message.ends_with("footnotes carried: n2"));
        let other = text(&workspace, "splits/other.md");
        assert!(
            other.starts_with("---\ntype: Topic\ntitle: Other [things]\n"),
            "{other}"
        );
        assert!(other.ends_with("Unrelated.[^n2]\n\n[^n2]: Two.\n"), "{other}");
        let parent = text(&workspace, "splits/parent.md");
        assert!(
            parent.contains("## Other\n\nMoved away. See [Other \\[things\\]](/splits/other.md).\n"),
            "{parent}"
        );
        // `[^n2]` moved with the only section citing it.
        assert!(parent.ends_with("[^n0]: Zero.\n[^s1]: From s1.\n"), "{parent}");
        assert!(text(&workspace, "splits/index.md")
            .ends_with("* [Other [things]](other.md) - Other, split out of Parent.\n"));
        assert_eq!(
            text(&workspace, "splits/user.md"),
            USER.replace("/splits/parent.md#other", "/splits/other.md")
        );
    }

    #[test]
    fn a_repeated_heading_is_chosen_by_its_slug() {
        let parent = "---\ntype: Note\ntitle: P\n---\n## Notes\n\nx\n\n## Notes\n\ny\n\n## Empty\n\n## End\n\nz\n";
        let workspace = MemWorkspace::from_files([("a/index.md", "# A\n"), ("a/p.md", parent)]);
        let args = Split {
            concept: "a/p.md",
            ..split("notes-1", "a/n.md")
        };

        let outcome = split_concept(&workspace, &ctx(), &args).unwrap();

        assert_eq!(
            outcome.message,
            concat!(
                "split 'Notes' (1 lines) out of /a/p.md into /a/n.md\n",
                "sources copied: none (all of the parent's (none was cited by footnote; prune them))",
            )
        );
        assert_eq!(
            text(&workspace, "a/p.md"),
            "---\ntype: Note\ntitle: P\n---\n## Notes\n\nx\n\n## Notes\n\nSee [Notes](/a/n.md).\n\n## Empty\n\n## End\n\nz\n"
        );
        assert_eq!(
            text(&workspace, "a/n.md"),
            concat!(
                "---\ntype: Note\ntitle: Notes\ndescription: Notes, split out of P.\n",
                "generated: { by: tester, at: 2026-11-03T12:00:00Z }\n---\n\n",
                "Split out of [P](/a/p.md); see it for the surrounding context.\n\ny\n",
            )
        );
    }

    #[test]
    fn refusals_word_okf_py_s_messages_and_write_nothing() {
        let parent = "---\ntype: Note\ntitle: P\n---\n## Notes\n\nx\n\n## Notes\n\ny\n\n## Empty\n\n## End\n\nz\n";
        let workspace = MemWorkspace::from_files([
            ("a/index.md", "# A\n"),
            ("a/p.md", parent),
            ("splits/parent.md", PARENT),
        ]);
        // A nested `fn`: its result borrows `section`, which a closure
        // could not express.
        fn in_a(section: &str) -> Split<'_> {
            Split {
                concept: "a/p.md",
                ..split(section, "a/n.md")
            }
        }
        let unknown = ["zz".to_string(), "s1".to_string()];

        let repeated = split_concept(&workspace, &ctx(), &in_a("Notes")).unwrap_err();
        let empty = split_concept(&workspace, &ctx(), &in_a("Empty")).unwrap_err();
        let missing = split_concept(&workspace, &ctx(), &in_a("Nope")).unwrap_err();
        let source = split_concept(
            &workspace,
            &ctx(),
            &Split {
                sources: &unknown,
                ..split("Other", "splits/o.md")
            },
        )
        .unwrap_err();

        assert_eq!(
            repeated.to_string(),
            "2 headings match 'Notes'; pass the anchor slug instead: notes, notes-1"
        );
        assert_eq!(empty.to_string(), "section 'Empty' has no content to move");
        assert_eq!(
            missing.to_string(),
            "no heading 'Nope'; headings are:\n  ## Notes\n  ## Notes\n  ## Empty\n  ## End"
        );
        assert_eq!(
            source.to_string(),
            "splits/parent.md has no source with id(s): zz"
        );
        assert_eq!(text(&workspace, "a/p.md"), parent);
        assert!(!workspace.is_file("a/n.md") && !workspace.is_file("splits/o.md"));
    }

    #[test]
    fn every_bookkeeping_edit_replays_onto_the_text_it_was_made_on() {
        let workspace = MemWorkspace::from_files(FILES);
        let original: std::collections::BTreeMap<&str, &str> = FILES.into_iter().collect();

        let outcome =
            split_concept(&workspace, &ctx(), &split("last-bit", "splits/deeper/last.md")).unwrap();

        assert_eq!(outcome.replay.len(), 2, "{:?}", outcome.replay.keys());
        for (path, ops) in &outcome.replay {
            let before = original.get(path.as_str()).copied();
            assert_eq!(replay(path, before, ops).unwrap(), text(&workspace, path), "{path}");
        }
    }
}
```

- [ ] **Step 4: Run them to see them fail**

Add `pub mod split;` to `src/okf/ops/mod.rs`. Run: `cargo test --lib -- okf::ops::split`
Expected: compile errors, `cannot find type Split`, `cannot find function split_concept`, `DEFAULT_SPLIT_TYPE`.

- [ ] **Step 5: Implement `split_concept`**

> **Corrected (issue #121), 2026-11-03.** `split_concept` is `py_truthy_str`'s first caller (with Task 3's `move_concept`). In `src/okf/pystr.rs`, delete the `#[cfg_attr(not(test), expect(dead_code, ...))]` attribute and the four-line comment above it on `py_truthy_str` (Task 2 added them until a caller existed). With this caller the expectation is unfulfilled, and `clippy -D warnings` fails until they go. If Task 3 or 4 has already merged and deleted them, there is nothing to do.

Put this above the test module in `src/okf/ops/split.rs`:

```rust
//! `split_concept`: port of `okf.py`'s `cmd_split` (`:1589`) and
//! `find_section` (`:1577`). A section of a concept, with its subsections,
//! becomes a new concept; the parent keeps the heading and a stub linking to
//! it; links to the section and its subsections follow it
//! ([`rewrite_links`] with a [`Redirect`] holding the moved anchors); the
//! new concept is listed in its folder's `index.md`.
//!
//! What publish replays (plan Decision 1): `ensure_index`'s files, the
//! [`ReplayOp::ListBullet`] of the new concept, and the [`ReplayOp::Relink`]
//! of every `index.md` and `log.md` whose links changed. The parent, the
//! new concept and every other file whose links changed are free edits.
//!
//! Divergences from `okf.py` (each where `okf.py` crashes):
//! - a parent `status` that is a list or mapping is written as Python's
//!   `str()` of it, where `okf.py`'s `scalar` raises `TypeError`;
//! - a new concept at the root of a catalog without a root `index.md` is
//!   [`OkfError::NotFound`] (`okf.py`: `FileNotFoundError`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;

use indexmap::IndexMap;

use crate::error::OkfError;
use crate::okf::doc::Doc;
use crate::okf::markdown::{
    atx_heading, body_split, footnote_defs, footnote_refs, headings, Heading,
};
use crate::okf::ops::log_index::collapse_whitespace;
use crate::okf::ops::relink::{rewrite_links, Moved, Redirect};
use crate::okf::ops::replay::{insert_index_bullet_text, ReplayOp};
use crate::okf::ops::{
    ensure_index, load_concept, refused, stamp_value, LoadedConcept, OpContext, OpOutcome,
};
use crate::okf::paths::{basename, dirname, join, new_concept_path};
use crate::okf::pystr::{py_repr_str, py_rstrip, py_str, py_strip, py_truthy_str};
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{block_item, flow_value, scalar, ScalarStyle, Value};

/// `okf.py split`'s `--type` default.
pub const DEFAULT_SPLIT_TYPE: &str = "Note";

/// `split_concept`'s arguments: `okf.py split`'s (a record, so its fields
/// are public).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split<'a> {
    /// The concept to split, `/dir/x.md`, `dir/x` or `/dir/x`.
    pub concept: &'a str,
    /// The heading text (case-insensitive) or its anchor slug.
    pub section: &'a str,
    /// The new concept's path, with or without `.md` and a leading `/`.
    pub to: &'a str,
    /// The new concept's `type` (`--type`, default [`DEFAULT_SPLIT_TYPE`]).
    pub kind: &'a str,
    /// Its title; `None` or empty for the heading, backticks dropped.
    pub title: Option<&'a str>,
    /// Its description, also the listing's summary and the default stub.
    pub description: Option<&'a str>,
    /// The paragraph left in the parent; a link to the new concept is
    /// appended unless it has one.
    pub stub: Option<&'a str>,
    /// Parent source ids to copy (`--source`); empty to copy the ones the
    /// section's footnotes cite, else all of them.
    pub sources: &'a [String],
}

/// The note `okf.py` prints without `--description`.
const NO_DESCRIPTION: &str =
    "note: no --description given; write a real one in the new concept and its index listing";

/// The new concept's descriptive keys.
struct Header<'a> {
    kind: &'a str,
    title: &'a str,
    description: &'a str,
    status: Option<String>,
}

/// Port of `cmd_split`: carves a section out of a concept into a new one.
///
/// # Arguments
/// * `workspace` - the catalog (the write path's scratch copy of an overlay).
/// * `ctx` - the "now" and actor for the new concept's `generated` stamp.
/// * `split` - what to split, and where to.
///
/// # Returns
/// The outcome: touched paths in `okf.py`'s order (parent, new concept,
/// `ensure_index`'s files, rewritten files, the folder index), the replay
/// ops of the index and log edits, `okf.py`'s two or three printed lines as
/// the message, and its notes.
///
/// # Errors
/// - As [`load_concept`] for the parent, as [`new_concept_path`] for `to`.
/// - [`OkfError::Refused`] with `okf.py`'s words: no such heading (listing
///   the headings), several matches (listing their slugs), a section with
///   no content, an unknown `--source` id, or generated frontmatter that
///   would not parse.
/// - The workspace's read or write error.
///
/// Every refusal comes before anything is written. Run the op on a scratch
/// copy, as the write path does, and drop the copy on an I/O error.
pub fn split_concept(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    split: &Split<'_>,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path: parent, data, ..
    } = load_concept(workspace, split.concept)?;
    let (new, note) = new_concept_path(workspace, split.to)?;
    let mut outcome = OpOutcome::default();
    outcome
        .notes
        .extend(note.map(|note| format!("note: {note}")));
    // Text mode, as `okf.py` reads the parent here: universal newlines.
    let text = workspace.read_text(&parent)?;
    let (head, body) = body_split(&text);
    let sections = headings(&body);
    let section = find_section(&sections, split.section)?;
    let (start, level) = (section.index, section.level);
    let heading = section.text.as_str();
    let end = sections
        .iter()
        .find(|later| later.index > start && later.level <= level)
        .map_or(body.len(), |later| later.index);
    // The lines strictly between the heading and the next one at its level.
    let moved = start + 1..end;
    let defs = footnote_defs(&body);
    let def_lines: HashSet<usize> = defs.values().flatten().copied().collect();
    let moved_lines = section_lines(&body, &moved, &def_lines);
    if moved_lines.is_empty() {
        return Err(refused(format!(
            "section {} has no content to move",
            py_repr_str(heading)
        )));
    }
    if end == body.len() {
        outcome.notes.push(format!(
            "note: '{heading}' is the last section, so it runs to the end of the document; \
             check that its closing paragraphs really belong to it and move any that don't back"
        ));
    }
    let moved_text = join_lines(moved_lines.iter().map(|&i| body[i]));
    let rest_text = join_lines(
        body.iter()
            .enumerate()
            .filter(|&(i, _)| !moved.contains(&i) && !def_lines.contains(&i))
            .map(|(_, line)| *line),
    );
    let used_moved = footnote_refs(&moved_text);
    let used_rest = footnote_refs(&rest_text);
    // `def_text`: each cited label's definition lines (empty if undefined).
    let cited_texts: Vec<String> = used_moved
        .iter()
        .map(|label| join_lines(defs.get(label).into_iter().flatten().map(|&i| body[i])))
        .collect();
    let (chosen, how) = choose_sources(
        &parent,
        source_entries(&data),
        split.sources,
        &used_moved,
        &cited_texts,
    )?;

    // The new concept.
    let heading_lines: HashSet<usize> = sections.iter().map(|h| h.index).collect();
    let new_body: Vec<String> = moved_lines
        .iter()
        .map(|&i| relevel(body[i], heading_lines.contains(&i), level))
        .collect();
    let title = split
        .title
        .filter(|title| !title.is_empty())
        .map_or_else(|| heading.replace('`', ""), str::to_string);
    let name = basename(&parent);
    let parent_title = py_truthy_str(data.get("title"))
        .unwrap_or_else(|| name.strip_suffix(".md").unwrap_or(name).to_string());
    let given = split.description.filter(|description| !description.is_empty());
    let description = match given {
        Some(description) => description.to_string(),
        None => {
            outcome.notes.push(NO_DESCRIPTION.to_string());
            format!("{heading}, split out of {parent_title}.")
        }
    };
    let header = Header {
        kind: split.kind,
        title: &title,
        description: &description,
        status: py_truthy_str(data.get("status")),
    };
    let intro =
        format!("Split out of [{parent_title}](/{parent}); see it for the surrounding context.");
    let foot = carried_footnotes(&defs, &used_moved, &body);
    let frontmatter = frontmatter_lines(&header, ctx, &chosen);
    let mut lines: Vec<&str> = frontmatter.iter().map(String::as_str).collect();
    lines.extend(["", intro.as_str(), ""]);
    lines.extend(new_body.iter().map(String::as_str));
    if !foot.is_empty() {
        lines.push("");
        lines.extend(&foot);
    }
    let content = format!("{}\n", lines.join("\n"));
    if let Err(error) = Doc::from_text(&new, &content).parse() {
        return Err(refused(format!(
            "generated frontmatter would not parse ({}); nothing written",
            error.message
        )));
    }

    // The parent: the heading stays, its content becomes the stub.
    let escaped = title.replace('[', "\\[").replace(']', "\\]");
    let link = format!("[{escaped}](/{new})");
    let mut stub = match (split.stub.filter(|stub| !stub.is_empty()), given) {
        (Some(stub), _) => stub.to_string(),
        (None, Some(_)) => format!("{link}: {description}"),
        (None, None) => format!("See {link}."),
    };
    if !stub.contains(&format!("(/{new})")) {
        stub = format!("{} See {link}.", py_rstrip(&stub));
    }
    let (dropped, kept_defs) = parent_footnotes(&defs, &used_moved, &used_rest, &moved, &body);
    let mut kept: Vec<&str> = Vec::with_capacity(body.len() + 3);
    for (i, &line) in body.iter().enumerate() {
        if i == start {
            kept.extend([line, "", stub.as_str(), ""]);
        } else if !moved.contains(&i) && !dropped.contains(&i) {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| py_strip(line).is_empty()) {
        kept.pop();
    }
    if !kept_defs.is_empty() {
        kept.push("");
        kept.extend(kept_defs);
    }
    // `head` moves into the parent's lines: it is not needed afterwards.
    let mut parent_lines = head;
    parent_lines.extend(kept);
    workspace.write_atomic(&parent, &format!("{}\n", parent_lines.join("\n")))?;
    workspace.write_atomic(&new, &content)?;
    outcome.touch(&parent);
    outcome.touch(&new);

    let folder = dirname(&new);
    ensure_index(workspace, folder, &mut outcome)?;
    let redirect = Redirect {
        from: parent.clone(),
        to: new.clone(),
        anchors: Some(moved_anchors(&sections, section, end, &content)),
    };
    let moved_file = Moved {
        path: &new,
        written_at: &parent,
    };
    let changed = rewrite_links(workspace, &redirect, Some(moved_file))?;
    outcome.record_rewrites(&changed, &redirect);
    let index_path = join(folder, "index.md");
    // `okf.py` lists the title as it is (no bracket escaping) and inserts
    // without checking the index lists it already (plan Decision 15).
    let bullet = format!(
        "* [{title}]({}) - {}",
        basename(&new),
        collapse_whitespace(&description)
    );
    let listed = insert_index_bullet_text(&workspace.read_text(&index_path)?, &bullet, None, false)
        .expect("without a section a bullet always finds a place");
    workspace.write_atomic(&index_path, &listed)?;
    let op = ReplayOp::ListBullet {
        target: new.clone(),
        bullet,
        section: None,
        create_section: false,
    };
    outcome.record(&index_path, op);

    let ids = chosen
        .iter()
        .map(|source| field(source, "id").map_or_else(|| "None".to_string(), py_str))
        .collect::<Vec<_>>()
        .join(", ");
    let ids = if ids.is_empty() { "none".to_string() } else { ids };
    let mut message = vec![
        format!(
            "split '{heading}' ({} lines) out of /{parent} into /{new}",
            moved_lines.len()
        ),
        format!("sources copied: {ids} ({how})"),
    ];
    if !foot.is_empty() {
        let carried: Vec<&str> = used_moved
            .iter()
            .filter(|label| defs.contains_key(*label))
            .map(String::as_str)
            .collect();
        message.push(format!("footnotes carried: {}", carried.join(", ")));
    }
    outcome.message = message.join("\n");
    Ok(outcome)
}

/// Port of `find_section`: the heading `want` names, by its text
/// (case-insensitive) or, failing that, by its anchor slug.
///
/// # Errors
/// [`OkfError::Refused`]: no match (the message lists every heading), or
/// several (the message lists their slugs).
fn find_section<'h>(sections: &'h [Heading], want: &str) -> Result<&'h Heading, OkfError> {
    // `want.strip().lstrip("#").strip()`, then lower-cased.
    let wanted = py_strip(py_strip(want).trim_start_matches('#')).to_lowercase();
    let by_text: Vec<&Heading> = sections
        .iter()
        .filter(|heading| heading.text.to_lowercase() == wanted)
        .collect();
    let hits = if by_text.is_empty() {
        let slug = wanted.trim_start_matches('#');
        sections.iter().filter(|heading| heading.slug == slug).collect()
    } else {
        by_text
    };
    // A slice pattern: `[]` is empty, `[hit]` exactly one, `many` the rest.
    match hits.as_slice() {
        [] => {
            let listed: Vec<String> = sections
                .iter()
                .map(|heading| format!("{} {}", "#".repeat(heading.level), heading.text))
                .collect();
            Err(refused(format!(
                "no heading {}; headings are:\n  {}",
                py_repr_str(want),
                listed.join("\n  ")
            )))
        }
        [hit] => Ok(*hit),
        many => {
            let slugs: Vec<&str> = many.iter().map(|heading| heading.slug.as_str()).collect();
            Err(refused(format!(
                "{} headings match {}; pass the anchor slug instead: {}",
                many.len(),
                py_repr_str(want),
                slugs.join(", ")
            )))
        }
    }
}

/// The section's content lines: `moved` without footnote definitions,
/// without blank lines at either end (`section_idx`).
fn section_lines(body: &[&str], moved: &Range<usize>, def_lines: &HashSet<usize>) -> Vec<usize> {
    let mut lines: Vec<usize> = moved.clone().filter(|i| !def_lines.contains(i)).collect();
    while lines.last().is_some_and(|&i| py_strip(body[i]).is_empty()) {
        lines.pop();
    }
    let leading = lines
        .iter()
        .take_while(|&&i| py_strip(body[i]).is_empty())
        .count();
    // `drain(..n)` removes the first `n` items (`del lines[:n]`).
    lines.drain(..leading);
    lines
}

/// Lines joined with `\n` (`"\n".join(...)`).
fn join_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    lines.collect::<Vec<_>>().join("\n")
}

/// A moved line, its heading re-levelled so the section's subsections
/// start at `#`.
fn relevel(line: &str, is_heading: bool, level: usize) -> String {
    match atx_heading(line) {
        Some((marks, text)) if is_heading => {
            // `max(1, len(marks) - level)`; `saturating_sub` stops at 0.
            format!("{} {text}", "#".repeat(marks.saturating_sub(level).max(1)))
        }
        Some(_) | None => line.to_string(),
    }
}

/// The parent's `sources` entries that are mappings (`srcs`).
fn source_entries(data: &IndexMap<String, Value>) -> Vec<&Value> {
    match data.get("sources") {
        Some(Value::Seq(items)) => items
            .iter()
            .filter(|item| matches!(item, Value::Map(_)))
            .collect(),
        Some(Value::Null | Value::Str(_) | Value::Map(_)) | None => Vec::new(),
    }
}

/// One field of a source entry (`x.get(key)`).
fn field<'v>(source: &'v Value, key: &str) -> Option<&'v Value> {
    match source {
        Value::Map(map) => map.get(key),
        Value::Null | Value::Str(_) | Value::Seq(_) => None,
    }
}

/// A source entry's `id`, when it is a string. A `fn`, not a closure, so
/// the returned `&str` can borrow from the argument (a closure cannot tie
/// its result's lifetime to its parameter's).
fn source_id(source: &Value) -> Option<&str> {
    field(source, "id").and_then(Value::as_str)
}

/// The sources the new concept gets, and how they were chosen: the named
/// ones; else those the section's footnotes cite (a cited label equal to the
/// id, or a cited definition containing its id, title or resource); else all.
///
/// # Errors
/// [`OkfError::Refused`] for a named id the parent does not have.
fn choose_sources<'v>(
    parent: &str,
    sources: Vec<&'v Value>,
    named: &[String],
    used: &BTreeSet<String>,
    cited_texts: &[String],
) -> Result<(Vec<&'v Value>, &'static str), OkfError> {
    if !named.is_empty() {
        let known: BTreeSet<&str> = sources.iter().filter_map(|source| source_id(source)).collect();
        // A `BTreeSet` sorts and de-duplicates, as `sorted(set(...))` does.
        let unknown: BTreeSet<&str> = named
            .iter()
            .map(String::as_str)
            .filter(|id| !known.contains(id))
            .collect();
        if !unknown.is_empty() {
            let unknown: Vec<&str> = unknown.into_iter().collect();
            return Err(refused(format!(
                "{parent} has no source with id(s): {}",
                unknown.join(", ")
            )));
        }
        let chosen = sources
            .into_iter()
            .filter(|source| source_id(source).is_some_and(|id| named.iter().any(|n| n == id)))
            .collect();
        return Ok((chosen, "named with --source"));
    }
    let cited: Vec<&Value> = sources
        .iter()
        .copied()
        .filter(|source| {
            let id = py_truthy_str(field(source, "id")).unwrap_or_default();
            let keys: Vec<String> = ["id", "title", "resource"]
                .iter()
                .filter_map(|key| py_truthy_str(field(source, key)))
                .collect();
            (!id.is_empty() && used.contains(&id))
                || keys
                    .iter()
                    .any(|key| cited_texts.iter().any(|text| text.contains(key.as_str())))
        })
        .collect();
    if cited.is_empty() {
        Ok((
            sources,
            "all of the parent's (none was cited by footnote; prune them)",
        ))
    } else {
        Ok((cited, "cited by the section's footnotes"))
    }
}

/// The definition lines of the footnotes the section cites, in the order
/// they are defined in the parent (`foot`).
fn carried_footnotes<'b>(
    defs: &IndexMap<String, Vec<usize>>,
    used: &BTreeSet<String>,
    body: &[&'b str],
) -> Vec<&'b str> {
    let mut labels: Vec<&String> = used.iter().collect();
    // A stable sort by first definition line; an undefined label adds no lines.
    labels.sort_by_key(|label| {
        defs.get(*label)
            .and_then(|lines| lines.iter().min().copied())
            .unwrap_or(usize::MAX)
    });
    labels
        .into_iter()
        .flat_map(|label| defs.get(label).into_iter().flatten().map(|&i| body[i]))
        .collect()
}

/// The new concept's frontmatter, delimiters included (`fm`).
fn frontmatter_lines(header: &Header<'_>, ctx: &OpContext<'_>, sources: &[&Value]) -> Vec<String> {
    let mut lines = vec![
        "---".to_string(),
        format!("type: {}", scalar(header.kind, ScalarStyle::Block)),
        format!("title: {}", scalar(header.title, ScalarStyle::Block)),
        format!("description: {}", scalar(header.description, ScalarStyle::Block)),
    ];
    if let Some(status) = &header.status {
        lines.push(format!("status: {}", scalar(status, ScalarStyle::Block)));
    }
    lines.push(format!("generated: {}", flow_value(&stamp_value(ctx))));
    if !sources.is_empty() {
        lines.push("sources:".to_string());
        for source in sources {
            lines.extend(block_item(source, 2));
        }
    }
    lines.push("---".to_string());
    lines
}

/// Which footnote definitions leave the parent, and which are re-added at
/// its end. A label only the section cites goes with it; a definition that
/// sat inside the section but is still cited elsewhere stays in the parent,
/// moved to its end. Orphans stay put.
fn parent_footnotes<'b>(
    defs: &IndexMap<String, Vec<usize>>,
    used_moved: &BTreeSet<String>,
    used_rest: &BTreeSet<String>,
    moved: &Range<usize>,
    body: &[&'b str],
) -> (HashSet<usize>, Vec<&'b str>) {
    let mut dropped = HashSet::new();
    let mut kept = Vec::new();
    for (label, lines) in defs {
        if used_moved.contains(label) && !used_rest.contains(label) {
            dropped.extend(lines.iter().copied());
        } else if lines.iter().all(|i| moved.contains(i)) {
            kept.extend(lines.iter().map(|&i| body[i]));
            dropped.extend(lines.iter().copied());
        }
    }
    (dropped, kept)
}

/// `amap`: the section's own anchor (a link to it loses its fragment) and
/// each moved subsection's anchor in the new concept, paired by position.
fn moved_anchors(
    sections: &[Heading],
    section: &Heading,
    end: usize,
    content: &str,
) -> BTreeMap<String, Option<String>> {
    let (_, new_body) = body_split(content);
    let new_headings = headings(&new_body);
    let mut anchors = BTreeMap::from([(section.slug.clone(), None)]);
    let moved = sections
        .iter()
        .filter(|heading| section.index < heading.index && heading.index < end);
    // `zip` stops at the shorter side, as Python's does.
    for (old, new) in moved.zip(&new_headings) {
        anchors.insert(old.slug.clone(), Some(new.slug.clone()));
    }
    anchors
}
```

In `src/okf/ops/mod.rs`, add `pub use split::{split_concept, Split, DEFAULT_SPLIT_TYPE};`, and append `split_concept` to the module doc's list of restructuring edits. Keep both sides when Task 3 adds `move_concept` to the same list.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- okf::ops::split markdown`
Expected: the 6 split tests and every markdown test pass.

- [ ] **Step 7: Parity: shim, script and the Rust side**

In `tests/parity/okf_shim.py`, add to `OPS_DEFAULTS` (after `"mv"` if Task 3 merged, else after `"ensure_index"`):

```python
    "split": {"title": None, "type": "Note", "description": None, "stub": None,
              "source": None, "by": "fixture"},
```

In `tests/parity/ops_script.json`, add to `"setup"`:

```json
    "splits/index.md": "# Splits\n\n# Concepts\n\n* [Parent](parent.md) - the parent\n",
    "splits/parent.md": "---\ntype: Note\ntitle: Parent\nstatus: draft\nsources:\n  - id: s1\n    resource: r1\n    title: One\n  - id: s2\n    resource: https://example.com/two\n---\n# Intro\n\nOpening.[^n0]\n\n## Background\n\nWhy it exists.[^s1]\n\n### Detail\n\nFine print, see [intro](#intro).\n\n## Other\n\nUnrelated.[^n2]\n\n## Last bit\n\nThe end.[^s1]\n\n[^n0]: Zero.\n[^s1]: From s1.\n[^n2]: Two.\n",
    "splits/user.md": "---\ntype: Note\ntitle: User\n---\nSee [bg](/splits/parent.md#background), [detail](parent.md#Detail), [other](/splits/parent.md#other) and [whole](/splits/parent.md).\n"
```

and append to `"steps"` (after Task 3's, if merged):

```json
    {"cmd": "split", "concept": "splits/parent.md", "section": "Background", "to": "splits/background.md", "description": "Why the parent exists."},
    {"cmd": "split", "concept": "splits/parent.md", "section": "last-bit", "to": "/splits/deeper/last.md", "source": ["s2"]},
    {"cmd": "split", "concept": "splits/parent.md", "section": "Nope", "to": "splits/nope.md"},
    {"cmd": "split", "concept": "splits/parent.md", "section": "Other", "to": "splits/other.md", "source": ["zz"]},
    {"cmd": "split", "concept": "splits/parent.md", "section": "Other", "to": "splits/other.md", "stub": "Moved away.", "title": "Other [things]", "type": "Topic"}
```

In `tests/parity.rs`:
- add `split_concept, Split, DEFAULT_SPLIT_TYPE` to the `okf_mcp::okf::ops` import;
- if Task 3 has not merged, add its `print_touched` and `with_touched` helpers (Task 3, Step 10) too;
- add an arm to `run_op`, before `other => panic!`:

```rust
        "split" => {
            // `source` is okf.py's repeatable `--source`: null or a list of ids.
            let sources: Vec<String> = step
                .get("source")
                .filter(|ids| !ids.is_null())
                .map(|ids| serde_json::from_value(ids.clone()).expect("a list of source ids"))
                .unwrap_or_default();
            let split = Split {
                concept: text("concept"),
                section: text("section"),
                to: text("to"),
                kind: optional("type").unwrap_or(DEFAULT_SPLIT_TYPE),
                title: optional("title"),
                description: optional("description"),
                stub: optional("stub"),
                sources: &sources,
            };
            split_concept(workspace, &ctx, &split).map(with_touched)
        }
```

- [ ] **Step 8: Regenerate the golden and run the parity tests**

Run: `UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens`

Expected (with Task 3 merged; without it, 47 steps, 10 errors and 25 files):
- 1 passed;
- `git diff --stat tests/golden` shows only `ops.json`;
- 52 steps, 11 of them errors. The new ones are step 50 (`no heading 'Nope'; headings are:` then `# Intro`, `## Background`, `## Other`, `## Last bit`) and step 51 (`splits/parent.md has no source with id(s): zz`);
- step 49 has three notes: the last-section note, the description note, `created /splits/deeper/index.md`;
- 35 changed files, among them `splits/parent.md`, which ends `[^n0]: Zero.\n`.

Run: `cargo test --test parity` then `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
Expected: every test passes, including `ops_match_golden` and `live_ops_parity`.

- [ ] **Step 9: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-04-split-concept.md`:

```markdown
# M6 Task 4: `okf::ops::split`: `split_concept`, and `split` parity (<ts>)

Task 4 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/okf/ops/split.rs`: `split_concept`, `Split` and `DEFAULT_SPLIT_TYPE`, a port
  of okf.py's `cmd_split` and `find_section`: sources chosen as okf.py chooses them,
  headings re-levelled, footnotes carried or kept, the parent's stub, links to the
  section and its subsections redirected (`Redirect` with anchors), the new concept
  listed (`ListBullet`); 6 unit tests whose expected texts were produced by okf.py.
- `markdown::footnote_refs` (FOOTREF_RE, its lookahead done by hand) and the
  crate-internal `atx_heading`; 2 unit tests and a doc example.
- Parity: 3 setup files and 5 split steps (two refused); `ops.json` regenerated.

## Changed

- `markdown::headings` reads each heading through `atx_heading`.

## Notes

- A list or mapping `status` is written as Python's `str()` of it, where okf.py's
  `scalar` raises; the module doc lists this divergence.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/markdown.rs tests/parity/okf_shim.py tests/parity/ops_script.json tests/parity.rs tests/golden/ops.json changelog/
git commit -m "feat: split_concept, a port of okf.py's split, with byte parity (M6.04)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `okf::ops::delete`: `delete_concept` and `OkfError::StillLinked`

`okf.py` has no delete command, so this follows the design's row and Decision 4.

**What it does:**
- It resolves the concept and refuses non-concepts and machinery.
- It refuses while any other document links to it, with `StillLinked`, which lists each link. Not counted:
  - the folder's own listing bullet;
  - the concept's links to itself;
  - the root readmes.
- Otherwise it removes the concept and its listing (`Unlist { bullet: None }`).

**What `write::apply` stores:**
- the removed concept, as a deletion entry under M5's rule;
- the listing edit, which replays at publish.

Publish's `links_into_deleted` (Decision 1) refuses a link added by someone else after this check ran.

**Files:**
- Create: `src/okf/ops/delete.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod delete;`, `pub use delete::delete_concept;`)
- Modify: `src/error.rs` (`StillLinked`, `LinkSource`, the `kind` and `to_json` arms, a test)
- Modify: `src/http/mod.rs` (`AppError::status`: 409)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Errors"; the `delete_concept` row)
- Create: `changelog/<ts>-m6-task-05-delete-concept.md`

- [ ] **Step 1: Write the failing error test**

Add to the `tests` module of `src/error.rs`:

```rust
    #[test]
    fn still_linked_lists_its_links_and_is_a_conflict_not_internal() {
        let error = OkfError::StillLinked {
            path: "infra/x.md".to_string(),
            links: vec![LinkSource {
                source: "hub.md".to_string(),
                line: 5,
            }],
        };

        let body = error.to_json();

        assert_eq!(error.kind(), "StillLinked");
        assert!(!error.is_internal());
        assert_eq!(body["path"], "infra/x.md");
        assert_eq!(body["links"], serde_json::json!([{"source": "hub.md", "line": 5}]));
        assert_eq!(
            body["message"],
            "infra/x.md: still linked from 1 place(s); retarget or remove those links \
             first (move_concept retargets them)"
        );
    }
```

Run: `cargo test --lib -- error::tests::still_linked`
Expected: compile error, `no variant named StillLinked`.

- [ ] **Step 2: Add the variant**

In `src/error.rs`, add after `Conflict` in `OkfError`:

```rust
    /// `delete_concept` was refused: other documents still link to the
    /// concept.
    #[error(
        "{path}: still linked from {} place(s); retarget or remove those links first \
         (move_concept retargets them)",
        .links.len()
    )]
    StillLinked {
        /// The concept's catalog-relative path.
        path: String,
        /// Each link into it, in walk order, then line order.
        links: Vec<LinkSource>,
    },
```

Add after `ConflictFile`:

```rust
/// One link of an [`OkfError::StillLinked`] (a record, so its fields are
/// public).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct LinkSource {
    /// The linking document's catalog-relative path.
    pub source: String,
    /// The 1-based line of the link (frontmatter lines count, as in lint).
    pub line: usize,
}
```

In `kind`, add `OkfError::StillLinked { .. } => "StillLinked",`. In `to_json`, add an arm before `OkfError::Conflict { files } => ...`:

```rust
            OkfError::StillLinked { path, links } => {
                body["path"] = path.as_str().into();
                body["links"] = json_list(links);
            }
```

`is_internal` needs nothing (it is not internal). In `src/http/mod.rs`, `AppError::status`: `OkfError::Refused { .. } | OkfError::Conflict { .. } | OkfError::StillLinked { .. } => StatusCode::CONFLICT,`.

Run: `cargo test --lib -- error http`
Expected: every test passes.

- [ ] **Step 3: Write the failing op tests**

Create `src/okf/ops/delete.rs` with this test module (Step 5 adds the code above it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::replay::replay;

    const INFRA_INDEX: &str = concat!(
        "# Infra\n\n# Concepts\n\n* [X](x.md) - x\n* [Y](y.md) - y\n",
        "* [X again](/infra/x.md) - listed twice\n* [W](w.md) - w\n",
    );
    const FILES: [(&str, &str); 11] = [
        (
            "index.md",
            "# Root\n\n# Sections\n\n* [Infra](infra/) - systems\n\nSee the [hub](/hub.md).\n",
        ),
        ("AGENTS.md", "Read [x](infra/x.md).\n"),
        (
            "hub.md",
            "---\ntype: Note\ntitle: Hub\n---\nSee [x](/infra/x.md#ports) and [y](infra/y.md).\n",
        ),
        (
            "log.md",
            "# Log\n\n## 2026-11-01\n\n* **Creation:** Added [x](/infra/x.md).\n",
        ),
        ("infra/index.md", INFRA_INDEX),
        (
            "infra/x.md",
            "---\ntype: Note\ntitle: X\n---\n# X\n\nSee [itself](#x) and [self](/infra/x.md).\n",
        ),
        ("infra/y.md", "---\ntype: Note\ntitle: Y\n---\n# Y\n"),
        ("infra/w.md", "---\ntype: Note\ntitle: W\n---\n# W\n"),
        ("infra/z.md", "---\ntype: Note\ntitle: Z\n---\n# Z\n"),
        ("solo/index.md", "# Solo\n\n* [Only](only.md) - the only one\n"),
        ("solo/only.md", "---\ntype: Note\ntitle: Only\n---\n# Only\n"),
    ];

    fn link(source: &str, line: usize) -> LinkSource {
        LinkSource {
            source: source.to_string(),
            line,
        }
    }

    #[test]
    fn a_linked_concept_is_refused_with_every_link_but_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let error = delete_concept(&workspace, "/infra/x").unwrap_err();

        // Not counted: the root AGENTS.md, the first bullet in infra/index.md
        // (the listing delete removes), x.md's links to itself.
        assert!(matches!(
            error,
            OkfError::StillLinked { ref path, ref links } if path == "infra/x.md" && links == &[
                link("hub.md", 5),
                link("log.md", 5),
                link("infra/index.md", 7),
            ]
        ), "{error:?}");
        assert!(workspace.is_file("infra/x.md"));
    }

    #[test]
    fn a_relative_link_counts_too() {
        let workspace = MemWorkspace::from_files(FILES);

        let error = delete_concept(&workspace, "infra/y.md").unwrap_err();

        assert!(matches!(
            error,
            OkfError::StillLinked { ref links, .. } if links == &[link("hub.md", 5)]
        ));
    }

    #[test]
    fn an_unlinked_concept_goes_with_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "infra/w.md").unwrap();

        assert_eq!(outcome.message, "deleted /infra/w.md");
        assert_eq!(outcome.touched, ["infra/w.md", "infra/index.md"]);
        assert!(outcome.notes.is_empty());
        assert!(!workspace.is_file("infra/w.md"));
        let listed = workspace.read_to_string("infra/index.md").unwrap();
        assert_eq!(listed, INFRA_INDEX.replace("* [W](w.md) - w\n", ""));
        let ops = &outcome.replay["infra/index.md"];
        assert_eq!(
            ops,
            &[ReplayOp::Unlist {
                target: "infra/w.md".to_string(),
                bullet: None
            }]
        );
        assert_eq!(replay("infra/index.md", Some(INFRA_INDEX), ops).unwrap(), listed);
    }

    #[test]
    fn an_unlisted_concept_touches_only_itself() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "infra/z.md").unwrap();

        assert_eq!(outcome.touched, ["infra/z.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(
            workspace.read_to_string("infra/index.md").unwrap(),
            INFRA_INDEX
        );
    }

    #[test]
    fn an_emptied_folder_keeps_its_index_and_says_so() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "solo/only.md").unwrap();

        assert_eq!(
            outcome.notes,
            ["note: /solo/ now holds only its index.md, which stays; delete_concept never \
              deletes an index"]
        );
        assert_eq!(
            workspace.read_to_string("solo/index.md").unwrap(),
            "# Solo\n"
        );
    }

    #[test]
    fn non_concepts_machinery_and_missing_paths_are_refused() {
        let mut files = FILES.to_vec();
        files.push(("install/setup.md", "---\ntype: Note\n---\n"));
        let workspace = MemWorkspace::from_files(files);

        let refusals = ["infra/index.md", "log.md", "AGENTS.md", "install/setup.md"]
            .map(|path| delete_concept(&workspace, path).unwrap_err());
        let missing = delete_concept(&workspace, "infra/none").unwrap_err();

        for error in &refusals {
            assert_eq!(error.kind(), "InvalidPath", "{error}");
        }
        assert!(refusals[3].to_string().contains("machinery"), "{}", refusals[3]);
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }
}
```

- [ ] **Step 4: Run them to see them fail**

Add `pub mod delete;` to `src/okf/ops/mod.rs`. Run: `cargo test --lib -- okf::ops::delete`
Expected: compile error, `cannot find function delete_concept`.

- [ ] **Step 5: Implement `delete_concept`**

Put this above the test module in `src/okf/ops/delete.rs`:

```rust
//! `delete_concept` (design "Write tools"; `okf.py` has no counterpart).
//! It removes a concept and its folder listing, and refuses while any other
//! document links to it ([`OkfError::StillLinked`], listing those links).
//!
//! What counts as a link (plan Decision 4) is what publish's link check
//! reads: every markdown file in the walk except the root `AGENTS.md`,
//! `CLAUDE.md` and `README.md`, so `index.md` and `log.md` files count. The
//! folder's own listing (the first bullet of its `index.md` linking the
//! concept, which this op removes) and the concept's links to itself do not.
//!
//! The listing goes through a [`ReplayOp::Unlist`] (any bullet, since it is
//! deleted either way), so publish replays it onto newer text. The concept
//! is removed with [`MemWorkspace::remove`], and `write::apply` stores a
//! deletion. An emptied folder keeps its `index.md`; a note says so.

use crate::error::{LinkSource, OkfError};
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::ops::replay::{listing_line, unlist_text, ReplayOp};
use crate::okf::ops::OpOutcome;
use crate::okf::paths::{dirname, join, outside_link_graph, resolve_concept};
use crate::okf::rules::check_path;
use crate::okf::workspace::{MemWorkspace, Workspace};

/// Deletes a concept and its listing in its folder's `index.md`.
///
/// # Arguments
/// * `workspace` - the catalog (the write path's scratch copy of an overlay).
/// * `reference` - the concept, `/dir/x.md`, `dir/x` or `/dir/x`.
///
/// # Returns
/// The outcome: the concept (removed) and, if it was listed, the folder
/// index with its `Unlist` op; the message `deleted /<path>`; a note when
/// the folder is left holding only its `index.md`.
///
/// # Errors
/// - As [`resolve_concept`] ([`OkfError::NotFound`]; [`OkfError::InvalidPath`]
///   for `index.md`, `log.md` and the root readmes).
/// - [`OkfError::InvalidPath`] for a concept in machinery ([`check_path`]).
/// - [`OkfError::StillLinked`] while another document links to it.
/// - The workspace's read or write error.
pub fn delete_concept(workspace: &MemWorkspace, reference: &str) -> Result<OpOutcome, OkfError> {
    let path = check_path(&resolve_concept(workspace, reference)?)?;
    let folder = dirname(&path);
    let index = join(folder, "index.md");
    let index_text = if workspace.is_file(&index) {
        Some(workspace.read_text(&index)?)
    } else {
        None
    };
    // `as_deref` views `Option<String>` as `Option<&str>`; `and_then` chains
    // the lookup, like `text and listing_line(...)` in Python.
    let listing = index_text
        .as_deref()
        .and_then(|text| listing_line(workspace, text, &index, &path));
    let links = links_into(workspace, &path, &index, listing)?;
    if !links.is_empty() {
        return Err(OkfError::StillLinked { path, links });
    }
    let unlisted = index_text
        .as_deref()
        .and_then(|text| unlist_text(workspace, text, &index, &path));
    workspace.remove(&path)?;
    let mut outcome = OpOutcome::default();
    outcome.touch(&path);
    if let Some((rest, _)) = unlisted {
        workspace.write_atomic(&index, &rest)?;
        let op = ReplayOp::Unlist {
            target: path.clone(),
            bullet: None,
        };
        outcome.record(&index, op);
    }
    if !folder.is_empty() && only_the_index_is_left(workspace, folder, &index)? {
        outcome.notes.push(format!(
            "note: /{folder}/ now holds only its index.md, which stays; delete_concept never \
             deletes an index"
        ));
    }
    outcome.message = format!("deleted /{path}");
    Ok(outcome)
}

/// Every link into `path` from another document, except the folder
/// listing's line (`listing`, a 0-based line of `index`).
///
/// # Errors
/// The workspace's read error.
fn links_into(
    workspace: &dyn Workspace,
    path: &str,
    index: &str,
    listing: Option<usize>,
) -> Result<Vec<LinkSource>, OkfError> {
    let mut links = Vec::new();
    for source in workspace.list_markdown()? {
        if outside_link_graph(&source) || source == path {
            continue;
        }
        // Universal newlines, as the snapshot (and so lint) numbers lines.
        let text = workspace.read_text(&source)?;
        for (number, line) in text.split('\n').enumerate() {
            if source == index && listing == Some(number) {
                continue;
            }
            // In-page links (`#x`) point into the source itself.
            let hits = find_links(line)
                .iter()
                .filter(|link| !link.target.starts_with('#'))
                .filter(|link| {
                    resolve_link(workspace, &link.target, &source)
                        .is_some_and(|resolved| resolved.path == path)
                })
                .count();
            // `repeat_with(..).take(n)`: one entry per link on the line.
            links.extend(
                std::iter::repeat_with(|| LinkSource {
                    source: source.clone(),
                    line: number + 1,
                })
                .take(hits),
            );
        }
    }
    Ok(links)
}

/// Does `folder` hold its `index.md` and nothing else?
///
/// # Errors
/// The workspace's listing error (none for a `MemWorkspace`).
fn only_the_index_is_left(
    workspace: &dyn Workspace,
    folder: &str,
    index: &str,
) -> Result<bool, OkfError> {
    let prefix = format!("{folder}/");
    // The `is_file` check first: a folder whose last file was the concept,
    // and which never had an index, is simply gone.
    Ok(workspace.is_file(index)
        && workspace
            .list_files()?
            .iter()
            .filter(|file| file.starts_with(&prefix))
            .all(|file| file == index))
}
```

In `src/okf/ops/mod.rs`, add `pub use delete::delete_concept;`, and append `delete_concept` to the module doc's list of restructuring edits. Tasks 3, 4 and 5 run in parallel and all extend that list: keep every side, so that it ends up reading "...`rewrite_links` (`relink`), `move_concept`, `split_concept` and `delete_concept`."

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- okf::ops::delete error`
Expected: the 6 delete tests and every error test pass.

- [ ] **Step 7: Update the design**

In "Errors", add a row after `Conflict`:

"| `StillLinked { path, links }` | `delete_concept` refused: other documents link to the concept; `links` lists each (source, line). Not counted: its folder's own listing bullet, its links to itself, the root `AGENTS.md`, `CLAUDE.md` and `README.md` (as publish's link check); `log.md` and other `index.md` files count; HTTP 409 |"

In "Write tools", the `delete_concept` row's Behavior becomes:

"Removes the concept and its listing in its folder's `index.md` (a replayable `Unlist`); the concept becomes a deletion entry. Refuses with `StillLinked` while any other document links to it (`log.md` and other `index.md` files included, the folder's own listing bullet not), listing those links. Refuses `index.md`, `log.md`, the root readmes and machinery. An emptied folder keeps its `index.md`, and a note says so"

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-05-delete-concept.md`:

```markdown
# M6 Task 5: `okf::ops::delete`: `delete_concept` and `OkfError::StillLinked` (<ts>)

Task 5 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/okf/ops/delete.rs`: `delete_concept`: refuses non-concepts, machinery and a
  concept other documents link to (`StillLinked`, one entry per link); removes the
  concept and its listing (`Unlist`, any bullet); a note for an emptied folder;
  6 unit tests.
- `OkfError::StillLinked { path, links }` and `LinkSource { source, line }`, HTTP 409;
  1 unit test.

## Changed

- Design: the "Errors" table and the `delete_concept` row.

## Notes

- Links from `log.md` count (plan Decision 4): publish's `links_into_deleted` and
  lint's `broken-link` read `log.md` too, so the publish would be refused anyway.
- The check runs on the changeset's view at write time; publish's
  `links_into_deleted` catches a link someone else published afterwards.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/error.rs src/http/mod.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: delete_concept, refused while linked (M6.05)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: the restructuring tools: `move_concept`, `split_concept`, `delete_concept`

The three tools are thin wrappers on `write::apply`, as M5's content tools are. They go in their own module and router block, `restructure_router`, joined last in `OkfServer::tools()`.

The tests show what each stores:
- **move:** a deletion entry for the old path, a creation for the new one, free edits for the linking files, and a replayable `index.md`;
- **delete refused while linked:** a `StillLinked` tool error, with nothing stored;
- **the changeset tools:** `pending_changes`, `revert` and `discard` handle a move like any other entries (Decision 17).

**Files:**
- Create: `src/mcp/restructure.rs`
- Modify: `src/mcp/mod.rs` (`mod restructure;`, `+ Self::restructure_router()`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` (the `move_concept` and `split_concept` rows)
- Create: `changelog/<ts>-m6-task-06-restructure-tools.md`

- [ ] **Step 1: Write the failing tests**

Create `src/mcp/restructure.rs` with this test module (Step 3 adds the code above it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::changesets::{discard, pending_changes, revert, DiscardParams, RevertParams};
    use crate::mcp::reports::CatalogParam;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::mcp::writes::{edit_document, EditDocumentParams};
    use crate::store::changesets::{self, EntryRow};

    const TIME: &str = "---\ntype: System\ntitle: Time\ndescription: The clock.\n---\n\
                        # Time\n\n## Ports\n\nUDP 123.\n";
    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                       See [time](/infra/time.md#ports).\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    (
                        "index.md",
                        "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
                    ),
                    ("infra/index.md", "# Infra\n\n* [Time](time.md) - the clock\n"),
                    ("infra/time.md", TIME),
                    ("hub.md", HUB),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    fn move_time(new: &str) -> MoveConceptParams {
        MoveConceptParams {
            old: "/infra/time".to_string(),
            new: new.to_string(),
            catalog: None,
        }
    }

    /// The changeset's entry for `path`.
    async fn entry(test: &TestApp, changeset: i64, path: &str) -> EntryRow {
        changesets::entries(test.app.pool(), changeset)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    /// The text of `path` as this connection sees it (its overlay).
    async fn seen(ctx: &ToolContext, path: &str) -> Option<String> {
        let state = ctx.app().catalog("pg").unwrap();
        let view = ctx.view(&state).await.unwrap();
        view.snapshot().get(path).map(|doc| doc.text().to_string())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_stores_a_deletion_a_creation_and_a_replayable_listing() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = move_concept(&ctx, move_time("/infra/clock.md")).await.unwrap();

        assert_eq!(
            report.touched,
            ["/infra/time.md", "/infra/clock.md", "/hub.md", "/infra/index.md"]
        );
        assert_eq!(report.message, "moved /infra/time.md -> /infra/clock.md");
        let old = entry(&test, report.changeset, "infra/time.md").await;
        assert_eq!((old.content, old.base_revision), (None, Some(1)));
        let new = entry(&test, report.changeset, "infra/clock.md").await;
        assert_eq!((new.base_revision, new.replay), (None, None));
        assert_eq!(entry(&test, report.changeset, "hub.md").await.replay, None);
        let listing = entry(&test, report.changeset, "infra/index.md").await;
        assert_eq!(listing.replay.expect("a relink replays")[0]["op"], "relink");
        assert_eq!(seen(&ctx, "infra/time.md").await, None);
        assert!(seen(&ctx, "hub.md")
            .await
            .unwrap()
            .contains("[time](/infra/clock.md#ports)"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_split_lists_the_new_concept_and_returns_okf_py_s_notes() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = split_concept(
            &ctx,
            SplitConceptParams {
                path: "/infra/time".to_string(),
                section: "Ports".to_string(),
                to: "/infra/ports.md".to_string(),
                kind: None,
                title: None,
                description: Some("UDP ports.".to_string()),
                stub: None,
                sources: None,
                by: None,
                catalog: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            report.touched,
            ["/infra/time.md", "/infra/ports.md", "/hub.md", "/infra/index.md"]
        );
        assert_eq!(report.notes.len(), 1, "{:?}", report.notes);
        assert!(report.notes[0].starts_with("note: 'Ports' is the last section"));
        let ports = seen(&ctx, "infra/ports.md").await.unwrap();
        // `type` defaulted to Note, and `by` to the key's actor.
        assert!(ports.starts_with("---\ntype: Note\ntitle: Ports\n"), "{ports}");
        assert!(ports.contains("by: writer-a"), "{ports}");
        assert!(seen(&ctx, "hub.md")
            .await
            .unwrap()
            .contains("[time](/infra/ports.md)"));
        let listing = entry(&test, report.changeset, "infra/index.md").await;
        assert_eq!(listing.replay.expect("a listing replays")[0]["op"], "list_bullet");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_is_refused_while_linked_then_goes_through() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let delete = || DeleteConceptParams {
            path: "/infra/time".to_string(),
            catalog: None,
        };

        let refused = delete_concept(&ctx, delete()).await.unwrap_err();
        let state = ctx.app().catalog("pg").unwrap();
        let nothing_stored = !ctx.view(&state).await.unwrap().pending();
        edit_document(
            &ctx,
            EditDocumentParams {
                path: "/hub.md".to_string(),
                old_str: "See [time](/infra/time.md#ports).".to_string(),
                new_str: "See the time server.".to_string(),
                replace_all: false,
                catalog: None,
            },
        )
        .await
        .unwrap();
        let deleted = delete_concept(&ctx, delete()).await.unwrap();

        let body = refused.to_json();
        assert_eq!(body["error"], "StillLinked");
        assert_eq!(
            body["links"],
            serde_json::json!([{"source": "hub.md", "line": 6}])
        );
        assert!(nothing_stored, "a refused delete stores nothing");
        assert_eq!(deleted.touched, ["/infra/time.md", "/infra/index.md"]);
        let listing = entry(&test, deleted.changeset, "infra/index.md").await;
        assert_eq!(listing.replay.expect("an unlist replays")[0]["op"], "unlist");
        assert_eq!(
            seen(&ctx, "infra/index.md").await.as_deref(),
            Some("# Infra\n\n")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_is_listed_reverted_and_discarded_like_any_other_edit() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let pending = || CatalogParam { catalog: None };
        move_concept(&ctx, move_time("/infra/clock.md")).await.unwrap();

        let listed = pending_changes(&ctx, pending()).await.unwrap();
        revert(
            &ctx,
            RevertParams {
                path: "/infra/time.md".to_string(),
                revision: 1,
                catalog: None,
            },
        )
        .await
        .unwrap();
        let reverted = pending_changes(&ctx, pending()).await.unwrap();
        let discarded = discard(
            &ctx,
            DiscardParams {
                paths: None,
                catalog: None,
            },
        )
        .await
        .unwrap();

        let changes: Vec<(&str, &str, bool)> = listed
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.change, file.replayable))
            .collect();
        assert_eq!(
            changes,
            [
                ("/hub.md", "update", false),
                ("/infra/clock.md", "create", false),
                ("/infra/index.md", "update", true),
                ("/infra/time.md", "delete", false),
            ]
        );
        // The old path is back, byte for byte the published text.
        let time = reverted
            .files
            .iter()
            .find(|file| file.path == "/infra/time.md")
            .unwrap();
        assert_eq!(
            (time.change, time.diff.as_str()),
            ("update", "No change from the published text\n")
        );
        assert_eq!((discarded.removed, discarded.closed), (4, true));
        let state = ctx.app().catalog("pg").unwrap();
        assert!(!ctx.view(&state).await.unwrap().pending());
    }

    #[test]
    fn the_three_tools_are_listed_with_their_schemas() {
        let tools = OkfServer::restructure_router().list_all();
        // `&*` borrows the name (a `Cow<str>`) as a plain `&str`.
        let names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
        let split = tools
            .iter()
            .find(|tool| tool.name == "split_concept")
            .unwrap();
        let properties = split.input_schema["properties"].as_object().unwrap();
        let required = split.input_schema["required"].as_array().unwrap();

        assert_eq!(names.len(), 3, "{names:?}");
        // And the server's router has them (`OkfServer::tools` joins this block).
        let all = OkfServer::tools().list_all();
        assert!(all.iter().any(|tool| tool.name == "delete_concept"));
        for name in ["move_concept", "split_concept", "delete_concept"] {
            assert!(names.contains(&name), "{name} is registered");
        }
        assert!(properties.contains_key("type") && !properties.contains_key("kind"));
        assert!(properties.contains_key("sources"));
        assert_eq!(
            required.iter().filter_map(|name| name.as_str()).collect::<Vec<_>>(),
            ["path", "section", "to"]
        );
        for tool in &tools {
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{}", tool.name);
            let inputs = tool.input_schema["properties"].as_object().unwrap();
            assert!(
                !inputs.keys().any(|key| key == "at" || key == "now"),
                "{}",
                tool.name
            );
        }
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Add `mod restructure;` to `src/mcp/mod.rs` (after `mod reports;`). Run: `cargo test --lib -- mcp::restructure`
Expected: compile errors, `cannot find type MoveConceptParams` and the rest.

- [ ] **Step 3: Implement the tools**

Put this above the test module in `src/mcp/restructure.rs`:

```rust
//! The restructuring tools (design "Write tools", M6): `move_concept` and
//! `split_concept`, ports of `okf.py`'s `mv` and `split`, and
//! `delete_concept`. Each goes through [`crate::write::apply`], so it acts
//! on this connection's changeset in one catalog and the write rules apply.
//! A block of its own (`restructure_router`), joined in `OkfServer::tools`.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::ops::{self, Split, DEFAULT_SPLIT_TYPE};
use crate::write::{apply, WriteReport};

/// `move_concept`'s arguments. Doc comments are the parameters'
/// descriptions in the tool's input schema.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct MoveConceptParams {
    /// The concept to move, `.md` optional, e.g. `/infra/ntp`.
    pub old: String,
    /// Its new path, `.md` optional, e.g. `/playbooks/ntp.md`; it must not exist.
    pub new: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `split_concept`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SplitConceptParams {
    /// The concept to split, `.md` optional, e.g. `/infra/ntp`.
    pub path: String,
    /// The section: its heading text (case-insensitive) or its anchor slug, e.g. `Ports` or `ports-1`.
    pub section: String,
    /// The new concept's path, `.md` optional; it must not exist.
    pub to: String,
    /// The new concept's `type` (default: Note).
    // `rename`: the JSON name is `type`, a Rust keyword, so the field is `kind`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// The new concept's title (default: the heading, backticks dropped).
    pub title: Option<String>,
    /// A one-sentence description; also the index listing's summary and the default stub. Without it, a placeholder is written and a note says so.
    pub description: Option<String>,
    /// The paragraph left under the heading in the parent; a link to the new concept is appended unless it has one.
    pub stub: Option<String>,
    /// Ids of the parent's sources to copy (default: those the section's footnotes cite, else all of them).
    pub sources: Option<Vec<String>>,
    /// Who wrote it, for the `generated` stamp (default: the key's actor).
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `delete_concept`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DeleteConceptParams {
    /// The concept, `.md` optional, e.g. `/infra/ntp`.
    pub path: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// Moves a concept, its listing and every link to it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the old and new paths.
///
/// # Returns
/// The write's report: the old path (a deletion), the new one, and every
/// file whose links or listing changed.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`] or [`OkfError::InvalidPath`]
/// (taken, reserved, machinery).
pub(crate) async fn move_concept(
    ctx: &ToolContext,
    params: MoveConceptParams,
) -> Result<WriteReport, OkfError> {
    let MoveConceptParams { old, new, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::move_concept(ws, &old, &new)
    })
    .await
}

/// Splits a section out of a concept into a new one.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, the section and the new concept.
///
/// # Returns
/// The write's report, with `okf.py split`'s notes.
///
/// # Errors
/// As [`apply`]; the op's refusals ([`OkfError::Refused`] for a heading it
/// cannot find or that is ambiguous, an empty section or an unknown source).
pub(crate) async fn split_concept(
    ctx: &ToolContext,
    params: SplitConceptParams,
) -> Result<WriteReport, OkfError> {
    // Destructuring moves each field into its own variable, so the closure
    // can take the ones it needs while `catalog` and `by` stay here.
    let SplitConceptParams {
        path,
        section,
        to,
        kind,
        title,
        description,
        stub,
        sources,
        by,
        catalog,
    } = params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        // `unwrap_or_default` on an `Option<Vec<_>>`: an empty list when absent.
        let sources = sources.unwrap_or_default();
        let split = Split {
            concept: &path,
            section: &section,
            to: &to,
            kind: kind.as_deref().unwrap_or(DEFAULT_SPLIT_TYPE),
            title: title.as_deref(),
            description: description.as_deref(),
            stub: stub.as_deref(),
            sources: &sources,
        };
        ops::split_concept(ws, op_ctx, &split)
    })
    .await
}

/// Deletes a concept and its listing, refused while it is linked.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept.
///
/// # Returns
/// The write's report: the concept (a deletion) and its folder index.
///
/// # Errors
/// As [`apply`]; [`OkfError::StillLinked`] while other documents link to
/// it; [`OkfError::InvalidPath`] for a non-concept or machinery.
pub(crate) async fn delete_concept(
    ctx: &ToolContext,
    params: DeleteConceptParams,
) -> Result<WriteReport, OkfError> {
    let DeleteConceptParams { path, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::delete_concept(ws, &path)
    })
    .await
}

// The restructuring tools; `OkfServer::tools` joins `restructure_router()` with the rest.
#[tool_router(router = restructure_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `move_concept` tool.
    #[tool(
        description = "Move or rename a concept in this connection's changeset (port of `okf.py \
                       mv`): its folder listing moves with it (creating folder indexes as \
                       needed) and every link to it is rewritten, keeping each link's style and \
                       anchor. `old` and `new` are concept paths, .md optional. Publish replays \
                       the index.md and log.md edits onto newer text; any other file it changed \
                       conflicts if someone changed it meanwhile. Write access."
    )]
    async fn move_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<MoveConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(move_concept(&ctx, params).await)
    }

    /// The `split_concept` tool.
    #[tool(
        description = "Carve a section (and its subsections) out of a concept into a new concept, \
                       in this connection's changeset (port of `okf.py split`). The new concept \
                       gets type, title, description, the parent's status, generated, and the \
                       sources the section's footnotes cite (or `sources`); the parent keeps the \
                       heading with a stub linking to it; links to the section follow it; it is \
                       listed in its folder's index.md. Returns okf.py's notes. Write access."
    )]
    async fn split_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SplitConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(split_concept(&ctx, params).await)
    }

    /// The `delete_concept` tool.
    #[tool(
        description = "Delete a concept and its listing in its folder's index.md, in this \
                       connection's changeset. Refused with StillLinked, listing each link \
                       (source, line), while any other document links to it, log.md and other \
                       index.md files included; move_concept or edit_document fixes them first. \
                       index.md files are never deleted. Write access."
    )]
    async fn delete_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<DeleteConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(delete_concept(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, `OkfServer::tools()` gains `+ Self::restructure_router()` after `+ Self::changes_router()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- mcp:: --test-threads=6`
Expected: the 5 new tests pass, and so does every other `mcp` test. No existing test counts `OkfServer::tools()` (checked: `history.rs:202`, `documents.rs:586`, `catalogs.rs:156` and `changesets.rs:1147` look tools up by name), so none needs a change.

- [ ] **Step 5: Update the design**

In "Write tools", the rows become:

"| `move_concept` | `old`, `new` | Port of `mv`: the concept's listing moves to the new folder (`ensure_index` for a new one) and every link to it is rewritten, keeping its style and anchor. Returns touched files and notes. The old path becomes a deletion entry. Listing and link edits in `index.md` and `log.md` replay at publish (`Unlist`, `ListBullet`, `Relink`); the concept and other linking files are free edits |"

"| `split_concept` | `path`, `section`, `to`, `type?` (default `Note`), `title?`, `description?`, `stub?`, `sources?` (`okf.py`'s `--source` ids), `by?` | Port of `split`; returns touched files and `okf.py`'s notes. The parent and the new concept are free edits; the new listing and link edits in `index.md` and `log.md` replay |"

- [ ] **Step 6: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-06-restructure-tools.md`:

```markdown
# M6 Task 6: the restructuring tools (<ts>)

Task 6 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/mcp/restructure.rs`: `move_concept`, `split_concept` and `delete_concept`,
  each on `write::apply`, in `restructure_router`; 5 tests (what a move stores; a
  split's listing and notes; a delete refused while linked, then allowed; a move
  through `pending_changes`, `revert` and `discard`; the tool listing).

## Changed

- `OkfServer::tools()` joins `restructure_router`.
- Design: the `move_concept` and `split_concept` rows.

## Notes

- `split_concept`'s `type` is the Rust field `kind` (`#[serde(rename = "type")]`).
- A move is several entries; `discard` of some leaves the rest, and publish's lint
  finds anything left broken (plan Decision 17).
```

Run every gate. Commit:

```bash
git add src/mcp/restructure.rs src/mcp/mod.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: move_concept, split_concept and delete_concept tools (M6.06)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: publish: moves, splits and deletes against other sessions

Publish has no new code (Decision 1): `replayed_text` replays the new ops, a stale free edit is already a `Conflict`, and `links_into_deleted` already reads deletion entries. This task proves it with database tests in a new module, `restructure_publish_tests`, in `src/mcp/changesets.rs` (beside M5's `publish_tests`, whose helpers it mirrors). They cover:
- **a move:** `delete` and `create` revisions, a tombstone, and a history per path (Decision 16);
- **replays onto a concurrent change:** a move's `Relink` onto a concurrent `index` of the same folder and onto a concurrent `log`; a delete's `Unlist` onto a concurrent listing;
- **refusals:** the `Conflict` on a concurrent edit of a linking file, `link-into-deleted` for a link to the old path published meanwhile or left by a partial `discard` of the move (Decision 17), and a cross-folder move's `Unlist` refusing a listing changed meanwhile;
- **a split:** the parent, the new concept and the listing.

The ops run through `write::apply` directly, as M5's Task 10 tests did, so this task does not wait for Task 6.

If one of these tests shows publish needs a code change, make it here (in `src/store/publish.rs`), say so in a dated `Corrected` note, and add the edge T7 -> T8 to the dependency graph.

**Files:**
- Modify: `src/mcp/changesets.rs` (a `#[cfg(test)] mod restructure_publish_tests` at the end)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Publish", step 4)
- Create: `changelog/<ts>-m6-task-07-restructure-publish-tests.md`

- [ ] **Step 1: Write the tests**

Append to `src/mcp/changesets.rs`:

```rust
#[cfg(test)]
mod restructure_publish_tests {
    use super::*;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::mcp::writes::{
        create_concept, edit_document, replace_body, CreateConceptParams, EditDocumentParams,
        ReplaceBodyParams,
    };
    use crate::okf::ops::{self, LogKind, Split, DEFAULT_SPLIT_TYPE};
    use crate::store::changesets::{self, ChangesetState};
    use crate::store::history;

    const X: &str = "---\ntype: Note\ntitle: X\ndescription: X.\n---\n# X\n";
    const Y: &str = "---\ntype: Note\ntitle: Y\ndescription: Y.\n---\n# Y\n";
    const BIG: &str = "---\ntype: Note\ntitle: Big\ndescription: Big.\n---\n# Big\n\n\
                       ## Part\n\nPart text.\n\n## Rest\n\nRest text.\n";
    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                       See [X](/infra/x.md) and [the part](/infra/big.md#part).\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[
            (
                "index.md",
                "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
            ),
            (
                "infra/index.md",
                "# Infra\n\n# Concepts\n\n* [X](x.md) - x\n* [Y](y.md) - y\n* [Big](big.md) - big\n",
            ),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("infra/big.md", BIG),
            ("hub.md", HUB),
            (
                "log.md",
                "# Log\n\n## 2026-11-01\n\n* **Update:** Added [X](/infra/x.md).\n",
            ),
        ];
        app_with(&[("pg", files)], Config::default()).await
    }

    /// A `move_concept` write, through the write path as Task 6's tool makes it.
    async fn moved(ctx: &ToolContext, old: &str, new: &str) -> WriteReport {
        let (old, new) = (old.to_string(), new.to_string());
        apply(ctx, None, None, move |ws, _| ops::move_concept(ws, &old, &new))
            .await
            .unwrap()
    }

    /// A `delete_concept` write.
    async fn deleted(ctx: &ToolContext, path: &str) -> WriteReport {
        let path = path.to_string();
        apply(ctx, None, None, move |ws, _| ops::delete_concept(ws, &path))
            .await
            .unwrap()
    }

    /// A `log` write.
    async fn logged(ctx: &ToolContext, text: &str) -> WriteReport {
        let text = text.to_string();
        apply(ctx, None, None, move |ws, op| ops::log(ws, op, LogKind::Update, &text))
            .await
            .unwrap()
    }

    /// A new concept `Q` at `path` with `body`; listed in its folder when `listed`.
    async fn created(ctx: &ToolContext, path: &str, body: &str, listed: bool) -> WriteReport {
        let frontmatter = serde_json::json!({"type": "Note", "title": "Q", "description": "Q."});
        let params = CreateConceptParams {
            path: path.to_string(),
            frontmatter: frontmatter.as_object().cloned().unwrap_or_default(),
            body: body.to_string(),
            // `bool::then`: `Some(..)` when true, like `x if cond else None`.
            index_summary: listed.then(|| "q".to_string()),
            index_section: None,
            by: None,
            catalog: None,
        };
        create_concept(ctx, params).await.unwrap()
    }

    async fn publish_as(ctx: &ToolContext, message: &str) -> Result<Published, OkfError> {
        let params = PublishParams {
            message: message.to_string(),
            catalog: None,
        };
        publish(ctx, params).await
    }

    fn published_text(test: &TestApp, path: &str) -> String {
        let loaded = test.app.catalog("pg").unwrap().current();
        loaded
            .snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    /// The publish's revisions as sorted `(path, op, revision)`.
    fn revisions(published: &Published) -> Vec<(&str, &str, i32)> {
        let mut rows: Vec<(&str, &str, i32)> = published
            .report
            .revisions
            .iter()
            .map(|row| (row.path.as_str(), row.op, row.revision))
            .collect();
        rows.sort_unstable();
        rows
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_move_deletes_the_old_path_and_creates_the_new_one() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        moved(&ctx, "infra/x.md", "infra/z.md").await;

        let published = publish_as(&ctx, "move x").await.unwrap();

        assert_eq!(
            revisions(&published),
            [
                ("/hub.md", "update", 2),
                ("/infra/index.md", "update", 2),
                ("/infra/x.md", "delete", 2),
                ("/infra/z.md", "create", 1),
                ("/log.md", "update", 2),
            ]
        );
        assert!(published.report.replayed.is_empty());
        let old = history::revisions(test.app.pool(), state.id(), "infra/x.md", 5)
            .await
            .unwrap();
        let new = history::revisions(test.app.pool(), state.id(), "infra/z.md", 5)
            .await
            .unwrap();
        // Each path keeps its own history (plan Decision 16).
        assert_eq!((old.len(), old[0].op.as_str()), (2, "delete"));
        assert_eq!(old[0].message, "move x");
        assert_eq!((new.len(), new[0].op.as_str()), (1, "create"));
        let tombstone: bool = sqlx::query_scalar(
            "SELECT deleted_at IS NOT NULL FROM documents WHERE path = 'infra/x.md'",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();
        assert!(tombstone);
        assert_eq!(published_text(&test, "infra/z.md"), X);
        assert!(published_text(&test, "hub.md").contains("[X](/infra/z.md)"));
        assert!(published_text(&test, "infra/index.md").contains("* [X](z.md) - x"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_replays_its_listing_onto_a_concurrent_index_of_the_folder() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        moved(&mover, "infra/y.md", "infra/w.md").await;
        created(&other, "/infra/q.md", "# Q\n", true).await;
        publish_as(&other, "add q").await.unwrap();

        let published = publish_as(&mover, "rename y").await.unwrap();

        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        let index = published_text(&test, "infra/index.md");
        assert!(index.contains("* [Y](w.md) - y"), "{index}");
        assert!(index.contains("* [Q](q.md) - q"), "{index}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_relinks_log_md_onto_a_concurrent_log() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        moved(&mover, "infra/x.md", "infra/z.md").await;
        logged(&other, "from b").await;
        publish_as(&other, "log b").await.unwrap();

        let published = publish_as(&mover, "move x").await.unwrap();

        assert!(published.report.replayed.contains(&"/log.md".to_string()));
        let log = published_text(&test, "log.md");
        assert!(log.contains("* **Update:** from b"), "{log}");
        assert!(log.contains("Added [X](/infra/z.md)."), "{log}");
        assert!(!log.contains("/infra/x.md"), "{log}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_conflicts_with_a_concurrent_edit_of_a_linking_file() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = moved(&mover, "infra/x.md", "infra/z.md").await;
        let body = ReplaceBodyParams {
            path: "/hub.md".to_string(),
            body: "Nothing linked.\n".to_string(),
            catalog: None,
        };
        replace_body(&other, body).await.unwrap();
        publish_as(&other, "edit hub").await.unwrap();

        let error = publish_as(&mover, "move x").await.unwrap_err();

        assert!(
            matches!(
                error,
                OkfError::Conflict { ref files } if files == &[ConflictFile {
                    path: "/hub.md".to_string(),
                    base_revision: Some(1),
                    current_revision: Some(2),
                }]
            ),
            "{error:?}"
        );
        let row = changesets::by_id(test.app.pool(), report.changeset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_to_the_old_path_published_meanwhile_refuses_the_move() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        moved(&mover, "infra/y.md", "infra/w.md").await;
        created(&other, "/b.md", "See [y](/infra/y.md).\n", false).await;
        publish_as(&other, "add b").await.unwrap();

        let error = publish_as(&mover, "rename y").await.unwrap_err();

        // `links_into_deleted` (store/publish.rs): b.md is not in the mover's
        // changeset and links to the path it deletes.
        let OkfError::LintFailed { errors, .. } = error else {
            panic!("expected LintFailed, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|finding| finding.file == "b.md" && finding.rule == "link-into-deleted"),
            "{errors:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_partial_discard_of_a_move_leaves_a_link_publish_refuses() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        moved(&ctx, "infra/x.md", "infra/z.md").await;
        // Drop only hub.md's rewrite: the published hub.md still links the old path.
        let params = DiscardParams {
            paths: Some(vec!["/hub.md".to_string()]),
            catalog: None,
        };
        discard(&ctx, params).await.unwrap();

        let error = publish_as(&ctx, "rename x").await.unwrap_err();

        let OkfError::LintFailed { errors, .. } = error else {
            panic!("expected LintFailed, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|finding| finding.file == "hub.md" && finding.rule == "link-into-deleted"),
            "{errors:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_listing_changed_since_a_move_to_another_folder_refuses_the_publish() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = moved(&mover, "infra/y.md", "ops/y.md").await;
        let edit = EditDocumentParams {
            path: "/infra/index.md".to_string(),
            old_str: "* [Y](y.md) - y".to_string(),
            new_str: "* [Y](y.md) - why".to_string(),
            replace_all: false,
            catalog: None,
        };
        edit_document(&other, edit).await.unwrap();
        publish_as(&other, "reword y").await.unwrap();

        let error = publish_as(&mover, "move y").await.unwrap_err();

        assert!(
            matches!(error, OkfError::Refused { ref reason } if reason.contains("changed since")),
            "{error:?}"
        );
        let row = changesets::by_id(test.app.pool(), report.changeset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_delete_leaves_a_tombstone_and_unlists_onto_newer_text() {
        let test = pg().await;
        let deleter = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = deleted(&deleter, "infra/y.md").await;
        created(&other, "/infra/q.md", "# Q\n", true).await;
        publish_as(&other, "add q").await.unwrap();

        let published = publish_as(&deleter, "drop y").await.unwrap();

        assert_eq!(report.touched, ["/infra/y.md", "/infra/index.md"]);
        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        assert_eq!(
            revisions(&published),
            [("/infra/index.md", "update", 3), ("/infra/y.md", "delete", 2)]
        );
        let index = published_text(&test, "infra/index.md");
        assert!(!index.contains("[Y](y.md)") && index.contains("[Q](q.md)"), "{index}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_split_writes_the_parent_the_new_concept_and_its_listing() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        apply(&ctx, None, None, |ws, op| {
            let split = Split {
                concept: "infra/big.md",
                section: "Part",
                to: "infra/part.md",
                kind: DEFAULT_SPLIT_TYPE,
                title: None,
                description: Some("The part."),
                stub: None,
                sources: &[],
            };
            ops::split_concept(ws, op, &split)
        })
        .await
        .unwrap();

        let published = publish_as(&ctx, "split big").await.unwrap();

        assert_eq!(
            revisions(&published),
            [
                ("/hub.md", "update", 2),
                ("/infra/big.md", "update", 2),
                ("/infra/index.md", "update", 2),
                ("/infra/part.md", "create", 1),
            ]
        );
        assert!(published_text(&test, "infra/part.md").contains("Part text."));
        assert!(published_text(&test, "infra/big.md")
            .contains("## Part\n\n[Part](/infra/part.md): The part.\n"));
        assert!(published_text(&test, "hub.md").contains("[the part](/infra/part.md)"));
        assert!(published_text(&test, "infra/index.md").contains("* [Part](part.md) - The part."));
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --lib -- restructure_publish_tests --test-threads=6`
Expected: 8 passed. The tests target code Tasks 2-5 already merged; if one fails, the bug is in that code or in publish. Fix it in this PR and record a dated `Corrected` note in the task whose code changed.

- [ ] **Step 3: Update the design**

In "Data flow", "Publish", step 4, after the "links into deleted or moved paths" bullet's last sentence ("This catches a link another session published after this changeset's `delete_concept` check ran."), add:

"A move's old path is a deletion entry, so the same check covers moves. A split moves no path: a link to the split-out section published meanwhile is a `broken-anchor` finding in a file this publish does not change, returned as a warning. A move to another folder replays its old listing's removal (`Unlist`) only while that bullet still reads as it did; if it changed meanwhile, publish is refused (`Refused`), so the change is not dropped."

- [ ] **Step 4: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-07-restructure-publish-tests.md`:

```markdown
# M6 Task 7: publish: moves, splits and deletes against other sessions (<ts>)

Task 7 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `src/mcp/changesets.rs`, `restructure_publish_tests`: 9 database tests:
  - a move's revisions, tombstone and per-path histories;
  - its `Relink` replayed onto a concurrent `index` and onto a concurrent `log`;
  - the `Conflict` on a concurrent edit of a linking file;
  - `link-into-deleted` for a link to the old path published meanwhile;
  - `link-into-deleted` for the link a partial `discard` of the move leaves;
  - a cross-folder move refused when its old listing changed meanwhile;
  - a delete's tombstone and `Unlist` replayed onto a concurrent listing;
  - a split's three revisions.

## Changed

- Design, "Publish" step 4: moves, splits and the changed-listing refusal.

## Notes

- Publish needed no code: `replayed_text` replays any stored ops, and
  `links_into_deleted` reads deletion entries.
```

Run every gate. Commit:

```bash
git add src/mcp/changesets.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "test: publish of moves, splits and deletes against other sessions (M6.07)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: end-to-end restructuring over HTTP (`tests/restructure.rs`)

The design's "Testing" asks for end-to-end tests of the server with rmcp's client. This task covers the restructuring tools as an agent sees them, in four tests:
1. a move, a publish and an export;
2. a concurrent edit of a linking file, which conflicts;
3. a concurrent `index` of the same folder, which replays;
4. a delete refused while linked, then allowed once the link is gone.

It uses M5's harness (`tests/support/server.rs`) as `tests/writes.rs` does.

**Files:**
- Create: `tests/restructure.rs`
- Create: `changelog/<ts>-m6-task-08-restructure-end-to-end.md`

- [ ] **Step 1: Write the tests**

Create `tests/restructure.rs`:

```rust
//! End-to-end tests of the restructuring tools (design "Testing"), over real
//! HTTP with rmcp's client:
//! - a move, published and exported;
//! - a move against a concurrent edit of a linking file (a conflict);
//! - a move against a concurrent `index` of the same folder (a replay);
//! - a delete refused while linked, then allowed.

mod support;

use okf_mcp::archive::{read_tarball, Files};
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};
use support::server::{call, files, structured, Client, TestServer};

const TIME: &str = "---\ntype: System\ntitle: Time server\ndescription: The GPS clock.\n---\n\
                    # Time server\n\n## Ports\n\nUDP 123.\n";
const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                   See the [time server](/infra/time.md#ports).\n";

/// One catalog, `pg`: a root index, a folder with one concept, a hub linking
/// it, a log.
fn catalog() -> Vec<(&'static str, Files)> {
    vec![(
        "pg",
        files(&[
            (
                "index.md",
                "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
            ),
            (
                "infra/index.md",
                "# Infra\n\n* [Time server](time.md) - the GPS clock\n",
            ),
            ("infra/time.md", TIME),
            ("hub.md", HUB),
            ("log.md", "# Log\n"),
        ]),
    )]
}

/// A write key with `pg` as its default.
async fn writer(server: &TestServer, id: &str) -> String {
    server.key(id, Access::Write, &[], Some("pg")).await
}

/// Calls a tool that must succeed and returns its JSON.
async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    structured(&call(client, tool, arguments).await).clone()
}

/// The `(path, op)` pairs of a publish's revisions, sorted.
fn revisions(published: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = published["revisions"]
        .as_array()
        .expect("a list of revisions")
        .iter()
        .map(|row| {
            let path = row["path"].as_str().expect("a path").to_string();
            (path, row["op"].as_str().expect("an op").to_string())
        })
        .collect();
    pairs.sort_unstable();
    pairs
}

/// The move every test but the last makes.
fn move_time() -> Value {
    json!({"old": "/infra/time", "new": "/infra/clock.md"})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_move_is_published_and_exported() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let client = server.client(&key, None).await.unwrap();

    let moved = ok(&client, "move_concept", move_time()).await;
    let seen = ok(&client, "get_concept", json!({"path": "/infra/clock.md"})).await;
    let published = ok(&client, "publish", json!({"message": "rename the time server"})).await;
    let new_history = ok(&client, "history", json!({"path": "/infra/clock.md"})).await;
    let old_history = ok(&client, "history", json!({"path": "/infra/time.md"})).await;
    let response = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let tarball = response.bytes().await.unwrap();
    client.cancel().await.unwrap();

    assert_eq!(
        moved["touched"],
        json!(["/infra/time.md", "/infra/clock.md", "/hub.md", "/infra/index.md"])
    );
    assert_eq!(seen["title"], "Time server");
    let expected = [
        ("/hub.md", "update"),
        ("/infra/clock.md", "create"),
        ("/infra/index.md", "update"),
        ("/infra/time.md", "delete"),
    ]
    .map(|(path, op)| (path.to_string(), op.to_string()));
    assert_eq!(revisions(&published), expected);
    assert_eq!(new_history["revisions"][0]["op"], "create");
    assert_eq!(old_history["revisions"][0]["op"], "delete");
    assert_eq!(status, reqwest::StatusCode::OK);
    let exported = read_tarball(&tarball[..], u64::MAX).unwrap();
    let text = |path: &str| String::from_utf8(exported[path].content.clone()).unwrap();
    assert!(!exported.contains_key("infra/time.md"));
    assert_eq!(text("infra/clock.md"), TIME);
    assert!(text("hub.md").contains("[time server](/infra/clock.md#ports)"));
    assert!(text("infra/index.md").contains("[Time server](clock.md)"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_edit_of_a_linking_file_conflicts() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let mover_key = writer(&server, "writer-a").await;
    let editor_key = writer(&server, "writer-b").await;
    let mover = server.client(&mover_key, None).await.unwrap();
    let editor = server.client(&editor_key, None).await.unwrap();
    ok(&mover, "move_concept", move_time()).await;
    let body = json!({"path": "/hub.md", "body": "See the [time server](/infra/time.md).\n"});
    ok(&editor, "replace_body", body).await;
    ok(&editor, "publish", json!({"message": "reword the hub"})).await;

    let refused = call(&mover, "publish", json!({"message": "rename"})).await;
    let pending = ok(&mover, "pending_changes", json!({})).await;
    mover.cancel().await.unwrap();
    editor.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "Conflict");
    assert_eq!(
        body["files"],
        json!([{"path": "/hub.md", "base_revision": 1, "current_revision": 2}])
    );
    // The move is still pending, its hub entry marked stale.
    let hub = pending["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "/hub.md")
        .expect("the hub is still pending");
    assert_eq!(hub["stale"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_index_of_the_same_folder_replays() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let mover_key = writer(&server, "writer-a").await;
    let lister_key = writer(&server, "writer-b").await;
    let mover = server.client(&mover_key, None).await.unwrap();
    let lister = server.client(&lister_key, None).await.unwrap();
    ok(&mover, "move_concept", move_time()).await;
    let concept = json!({
        "path": "/infra/ntp.md",
        "frontmatter": {"type": "System", "title": "NTP", "description": "How hosts sync."},
        "body": "# NTP\n",
        "index_summary": "how hosts sync",
    });
    ok(&lister, "create_concept", concept).await;
    ok(&lister, "publish", json!({"message": "add NTP"})).await;

    let published = ok(&mover, "publish", json!({"message": "rename"})).await;
    let index = ok(&mover, "read_document", json!({"path": "/infra/index.md"})).await;
    mover.cancel().await.unwrap();
    lister.cancel().await.unwrap();

    assert_eq!(published["published"], true);
    assert_eq!(published["replayed"], json!(["/infra/index.md"]));
    let text = index["text"].as_str().unwrap();
    assert!(text.contains("* [Time server](clock.md) - the GPS clock"), "{text}");
    assert!(text.contains("* [NTP](ntp.md) - how hosts sync"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_is_refused_while_linked_then_published() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let client = server.client(&key, None).await.unwrap();
    let delete = || json!({"path": "/infra/time.md"});

    let refused = call(&client, "delete_concept", delete()).await;
    let unlink = json!({
        "path": "/hub.md",
        "old_str": "See the [time server](/infra/time.md#ports).",
        "new_str": "See the time server.",
    });
    ok(&client, "edit_document", unlink).await;
    let deleted = ok(&client, "delete_concept", delete()).await;
    let published = ok(&client, "publish", json!({"message": "retire the time server"})).await;
    let index = ok(&client, "read_document", json!({"path": "/infra/index.md"})).await;
    client.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "StillLinked");
    assert_eq!(body["path"], "infra/time.md");
    assert_eq!(body["links"], json!([{"source": "hub.md", "line": 6}]));
    assert_eq!(deleted["touched"], json!(["/infra/time.md", "/infra/index.md"]));
    let expected = [
        ("/hub.md", "update"),
        ("/infra/index.md", "update"),
        ("/infra/time.md", "delete"),
    ]
    .map(|(path, op)| (path.to_string(), op.to_string()));
    assert_eq!(revisions(&published), expected);
    assert_eq!(index["text"], "# Infra\n\n");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --test restructure -- --test-threads=6`
Expected: 4 passed. (`read_document` answers with a `text` field, as `tests/writes.rs` reads it.)

- [ ] **Step 3: Changelog, gates, commit**

Write `changelog/<ts>-m6-task-08-restructure-end-to-end.md`:

```markdown
# M6 Task 8: end-to-end restructuring over HTTP (<ts>)

Task 8 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `tests/restructure.rs`: 4 end-to-end tests over HTTP with rmcp's client: a move
  published and exported (revisions, both histories, the tarball); a conflict on a
  concurrent edit of a linking file; a replay onto a concurrent `index` of the same
  folder; a delete refused while linked (`StillLinked` with the link), then published.
```

Run every gate. Commit:

```bash
git add tests/restructure.rs changelog/
git commit -m "test: end-to-end move and delete over HTTP (M6.08)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: the M6 smoke test, README, design update and milestone changelog

This task restructures a throwaway import of the real catalog, timing each step, documents the three tools, and closes the milestone in the design. It follows M5's Task 14. It depends on every other task.

**Files:**
- Modify: `tests/smoke.rs` (add `restructuring_on_a_real_catalog`)
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`
- Create: `changelog/<ts>-m6-restructuring.md` (milestone) and `changelog/<ts>-m6-task-09-wrap-up.md`

- [ ] **Step 1: The smoke test**

In `tests/smoke.rs`'s module doc:
- "All four tests" becomes "All five tests";
- add to the list: "`restructuring_on_a_real_catalog` (M6) imports the checkout into a throwaway catalog, publishes a probe concept, then moves it, splits a section out of it, tries a delete that must be refused while linked, and publishes, timing each tool through rmcp's client (`rewrite_links` and the delete's link check walk the whole catalog)."

Add:

```rust
/// Restructure a real catalog over HTTP and time each tool and the publish.
///
/// The checkout is imported into the test's own Postgres container, so the
/// real catalog is never written. A probe concept is created and published
/// first; then it is moved (which rewrites links across the whole catalog),
/// a section is split out of it, and a delete of the split-out concept is
/// refused because the probe's stub links it. Only sizes and timings are
/// printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn restructuring_on_a_real_catalog() {
    use okf_mcp::archive::read_directory;
    use okf_mcp::config::Config;
    use okf_mcp::store::keys::Access;
    use serde_json::json;
    use support::server::{call, TestServer};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let files = read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    let count = files.len();
    let started = Instant::now();
    let server = TestServer::start(vec![("pg", files)], Config::default()).await;
    eprintln!(
        "start Postgres, import {count} files and start the server: {:?}",
        started.elapsed()
    );
    let key = server.key("smoke", Access::Write, &[], Some("pg")).await;
    let client = server
        .client_with(&key, &[("x-okf-changeset", "smoke")])
        .await
        .unwrap();

    let calls = [
        (
            "create_concept",
            json!({
                "path": "/m6-smoke/probe.md",
                "frontmatter": {
                    "type": "Note",
                    "title": "M6 smoke probe",
                    "description": "Written by the M6 smoke test.",
                },
                "body": "# M6 smoke probe\n\n## Part one\n\nOne.\n\n## Part two\n\nTwo.\n",
                "index_summary": "the M6 smoke test's probe",
            }),
        ),
        ("publish", json!({"message": "M6 smoke test: probe"})),
        (
            "move_concept",
            json!({"old": "/m6-smoke/probe", "new": "/m6-smoke/moved.md"}),
        ),
        (
            "split_concept",
            json!({
                "path": "/m6-smoke/moved.md",
                "section": "Part two",
                "to": "/m6-smoke/part-two.md",
                "description": "The second part.",
            }),
        ),
        ("delete_concept", json!({"path": "/m6-smoke/part-two.md"})),
        ("pending_changes", json!({})),
        ("publish", json!({"message": "M6 smoke test: restructure"})),
    ];
    let mut failed = Vec::new();
    let mut refusal = serde_json::Value::Null;
    // `Value::Null` until the last publish answers; indexing a missing key
    // of a `Value` gives `Null`, so the asserts below fail with a message.
    let mut published = serde_json::Value::Null;
    for (tool, arguments) in calls {
        let started = Instant::now();
        let result = call(&client, tool, arguments).await;
        let elapsed = started.elapsed();
        let bytes = result
            .structured_content
            .as_ref()
            .map_or(0, |value| value.to_string().len());
        eprintln!("{tool}: {bytes} bytes of JSON in {elapsed:?}");
        match (tool, result.is_error == Some(true)) {
            // The one refusal the test expects; its JSON names paths and lines only.
            ("delete_concept", true) => refusal = result.structured_content.unwrap_or_default(),
            (_, true) => failed.push((tool, result.content)),
            ("publish", false) => published = result.structured_content.unwrap_or_default(),
            (_, false) => {}
        }
    }
    client.cancel().await.unwrap();

    assert!(failed.is_empty(), "these tools returned errors: {failed:?}");
    assert_eq!(refusal["error"], "StillLinked");
    assert_eq!(
        refusal["links"],
        json!([{"source": "m6-smoke/moved.md", "line": 15}]),
        "the stub is the split-out concept's only link"
    );
    assert_eq!(published["published"], true);
    let revisions = published["revisions"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    eprintln!(
        "publish wrote {} revisions, replayed {}, {} warnings",
        revisions.len(),
        published["replayed"].as_array().map_or(0, Vec::len),
        published["warnings"].as_array().map_or(0, Vec::len)
    );
    let mut paths: Vec<&str> = revisions
        .iter()
        .filter_map(|revision| revision["path"].as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/m6-smoke/index.md",
            "/m6-smoke/moved.md",
            "/m6-smoke/part-two.md",
            "/m6-smoke/probe.md"
        ],
        "the folder index, the moved probe, the split-out part and the old path"
    );
}
```

The stub is on line 15 of `moved.md`:
- lines 1-6 are the frontmatter `create_concept` writes, in `KEY_ORDER`: `---`, `type`, `title`, `description`, `generated`, `---`;
- lines 7-14 are the body: `# M6 smoke probe`, blank, `## Part one`, blank, `One.`, blank, `## Part two`, blank;
- line 15 is the stub.

The folder's own listing of `part-two.md` in `m6-smoke/index.md` does not count (Decision 4).

Run: `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture restructuring_on_a_real_catalog`
Expected: passes. The output has one timing line per tool, and the publish wrote 4 revisions. Paste the timings into the PR and the milestone changelog.

- [ ] **Step 2: README**

- **Status paragraph:** "milestones 1 (core library), 2 (analysis), 3 (storage), 4 (read-only server) and 5 (writes) are delivered" becomes "milestones 1 (core library), 2 (analysis), 3 (storage), 4 (read-only server), 5 (writes) and 6 (restructuring) are delivered". Replace "Moving, splitting and deleting concepts arrive in milestone 6." with "Concepts can also be moved, split and deleted, with every link and listing kept in step."
- **"Write tools":** after the bookkeeping table, add:

```markdown
Restructuring (`write`):

| Tool | Does |
|---|---|
| `move_concept` | Moves or renames a concept (`old`, `new`), port of `okf.py mv`: its folder listing moves with it, and every link to it is rewritten, keeping each link's style and anchor |
| `split_concept` | Carves a `section` out of a concept (`path`) into a new one (`to`), port of `okf.py split`: the parent keeps a stub linking to it, links to the section follow it, and it is listed in its folder (`type`, `title`, `description`, `stub`, `sources` as `okf.py`'s flags) |
| `delete_concept` | Deletes a concept and its listing; refused with `StillLinked`, listing each link, while any other document links to it |
```

- **"Publish":** the first bullet becomes:

```markdown
- `log` and `index` entries (and `create_concept`'s listing), and the listing
  changes and link rewrites that `move_concept`, `split_concept` and
  `delete_concept` make in `index.md` and `log.md`, are replayed onto newer
  text instead of conflicting, so concurrent agents logging to the same
  `log.md` all get their lines in; a moved listing whose bullet someone
  changed meanwhile refuses the publish rather than drop that change;
```

  Its third bullet ends "...a link left pointing into a path the changeset deletes (or moves away from), refuses it with `LintFailed`; ...".
- **"Layout":**
  - the `src/okf/` bullet's "the edit operations `ops` (ported from `okf.py`, with the replay of `log` and `index` edits)" becomes "the edit operations `ops` (ported from `okf.py`: the bookkeeping, `mv`, `split` and its link rewriting, with the replay of their `index.md` and `log.md` edits; and `delete`)";
  - the `src/mcp/` bullet lists "the restructuring tools (`restructure`)" after the write tools;
  - add a bullet after `tests/writes.rs`: "`tests/restructure.rs` - end-to-end restructuring over HTTP: a move published and exported; a conflict and a replay against other sessions; a delete refused while linked.";
  - the `tests/smoke.rs` bullet adds "and the restructuring tools (move, split, a refused delete and publish)" after the write path.

- [ ] **Step 3: The design**

- **Header:** the "Status" line becomes "approved design. M1 (core library), M2 (analysis), M3 (storage), M4 (read-only server), M5 (writes) and M6 (restructuring) are delivered; see "Milestones"".
- **Milestones table:** the M6 row becomes:

  "| M6. Restructuring (delivered) | `move_concept`, `split_concept`, `delete_concept`; `ReplayOp::Unlist` and `ReplayOp::Relink`; byte parity with `okf.py`'s `mv` and `split` ([plan](../plans/2026-11-03-okf-mcp-m6-restructuring.md); PRs #A-#B) |"

  Here #A-#B are the first and last numbers of this milestone's merged PRs, from `gh pr list --state merged --search "M6."`.
- **"What M6 shipped differently":** add this section after "What M5 shipped differently" (before "Questions M5 answered"):

```markdown
### What M6 shipped differently

The M6 plan's "Decisions taken while planning", its "Simplifications and deviations
relative to the design" and its dated `Corrected` notes have the full list; most are
already written into the sections above.

Deviations planned:

- The listing changes are two new replay operations: `Unlist { target, bullet }`
  removes the bullet listing a path, refusing (with `bullet`) a listing changed since,
  and `Relink { redirect }` redirects the links into a moved document or section. A
  move across folders is `Unlist` plus `ListBullet`.
- Link rewrites in `log.md` replay like those in `index.md` (plan Decision 7); in
  every other document they are free edits.
- `delete_concept` refuses with a new error, `StillLinked { path, links }` (HTTP 409);
  links from `log.md` and other `index.md` files count, the folder's own listing does
  not; an emptied folder keeps its `index.md`.
- `split_concept`'s `type` defaults to `Note`, and its `sources` is `okf.py`'s
  `--source`.
- `history` stays per path: a move ends the old path's history with `delete` and
  starts the new one's with `create`.
- No derived-column backfill: M6 does not change `Derived::of`.
```

  Then add two lists from the `Corrected` notes M6's tasks wrote, each item naming its issue and what changed, as M5's section does:
  - "Found while building it:" for anything a task found that the plan had not foreseen;
  - "Plan corrections (each a dated `Corrected` note in the plan):".

  If a task wrote no `Corrected` note, its line reads "**#N (<task>):** shipped as planned". These lists record what happened, so they can only be written at this step.
- **"Questions M5 answered":** no change. "Carried into M3 and later": no change.

- [ ] **Step 4: The milestone changelog and this task's entry**

Write `changelog/<ts>-m6-restructuring.md`, shaped like `changelog/2026-11-03T114102Z-m5-writes.md`:

```markdown
# Milestone 6 (restructuring) delivered (<ts>)

Plan: `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md`; epic and
sub-issues as filed; PRs #A-#B.

## What shipped

- `okf::ops::relink`: `rewrite_links` (okf.py's link rewriter) and `paths::relpath`.
- `ReplayOp::Unlist` and `ReplayOp::Relink`, replayed at publish like `ListBullet`.
- `move_concept` and `split_concept`, byte for byte against okf.py's `mv` and `split`
  (golden and live parity), and `delete_concept` with `OkfError::StillLinked`.
- The three MCP tools in `restructure_router`.
- Tests: unit tests per op; publish database tests (replays, conflicts,
  link-into-deleted, tombstones, histories); end-to-end over HTTP; the smoke test.

## Gates at the end

<the six gates, each with its pass count, and the smoke test's timings>

## Follow-ups

Accepted in M5 and still open (plan Decision 2), for things-to-fix:

- pool limits (queued writers each hold a pooled connection; a publish needs a second
  to refresh a stale snapshot);
- the overlay cache's size cap;
- `write::apply`'s `Busy` branch for a closed changeset, which no test reaches;
- a test for `create_concept`'s `parse_checked` guard.

Deferred:

- an `okf-mcp reindex` backfill, with the first change to `Derived::of`;
- `history` across renames (not planned);
- deployment, the NTP check and TLS to Postgres (M7).
```

The `<the six gates ...>` line is filled with the real output of the final gate run, as M5's milestone changelog was. Run every gate first, then write it.

Write `changelog/<ts>-m6-task-09-wrap-up.md` with Added (the smoke test), Changed (README, design) and Notes (the timings).

- [ ] **Step 5: Gates, commit**

Run every gate, and the smoke test in release mode. Commit:

```bash
git add tests/smoke.rs README.md docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "docs: M6 smoke test, README, design update and changelog (M6.09)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-review (2026-11-03)

- **Design coverage (M6 row, and the sections it rests on).**
  - **The three tools:**
    - `move_concept`: op T3, tool T6;
    - `split_concept`: op T4, tool T6;
    - `delete_concept`: op T5, tool T6.
  - **"Write" step 4, the listing changes as replay operations:** `Unlist` and `Relink` (T2), recorded by the ops (T3-T5).
  - **"Publish" step 4, "links into deleted or moved paths":** the existing `links_into_deleted`, confirmed in Decision 1 and tested in T7.
  - **"Errors":** `StillLinked` (T5).
  - **"Testing":**
    - unit tests in T1-T6;
    - parity with `okf.py` for `mv` and `split` (T3, T4);
    - database tests (T7);
    - end-to-end over HTTP (T8);
    - the smoke test (T9).
  - **The M5 changelog's M6 follow-ups:**
    - the tools on `write::apply` (T6);
    - deletions through a removed touched path (T3's `rename`, T5's `remove`);
    - `ReplayOp` variants for renaming and removing a bullet (T2);
    - the backfill, deferred (Decision 5).
- **Placeholders.** Every step that changes code shows the code. Three values can only come from running the milestone:
  - the PR range `#A-#B` (T9);
  - the final gate counts and timings in the milestone changelog (T9);
  - the "Found while building it" and "Plan corrections" lists of "What M6 shipped differently", written from the tasks' `Corrected` notes (T9).

  Each says where its value comes from.
- **Names across tasks.** Each interface symbol has one definition, in the task the file map assigns it to: `relpath`, `outside_link_graph`, `Redirect`, `Moved`, `rewrite_links`, `rewrite_text`, `Unlist`, `Relink`, `listing_line`, `unlist_text`, `record_rewrites`, `collapse_whitespace`, `py_truthy_str`, `move_concept`, `rename`, `Split`, `DEFAULT_SPLIT_TYPE`, `split_concept`, `footnote_refs`, `atx_heading`, `delete_concept`, `StillLinked`, `LinkSource`, `MoveConceptParams`, `SplitConceptParams`, `DeleteConceptParams`, `restructure_router`. Tasks 3 and 4 run in parallel, so everything both use lands in Task 2: `collapse_whitespace`'s visibility, `py_truthy_str` and `record_rewrites`.
- **Docs that `cargo doc -D warnings` would refuse.** No public item's doc links a crate-private item (M5's Task 3 correction): `rewrite_text`, `unlist_text` and `listing_line` are named in backticks only.
- **Shared files.** These take lines from parallel tasks: `src/okf/ops/mod.rs`, the parity shim, script and Rust side (T3, T4), and the design (T2, T5, T6, T7, T9). The second of T3 and T4 to merge regenerates `ops.json`.
