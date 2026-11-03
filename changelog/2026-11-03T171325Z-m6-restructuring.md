# Milestone 6 (restructuring) delivered (2026-11-03T17:13:25Z)

Plan: `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (PR #118); epic
#119 and sub-issues #120-#128; PRs #129-#137. Design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`.

Agents can now restructure a catalog through the server: `move_concept`,
`split_concept` and `delete_concept`, with every link and listing kept in step and
the `index.md` and `log.md` edits replayed onto newer text at publish.

## What shipped

- `okf::ops::relink`: `rewrite_links` (okf.py's link rewriter) and `paths::relpath`.
- `ReplayOp::Unlist` and `ReplayOp::Relink`, replayed at publish like `ListBullet`.
- `move_concept` and `split_concept`, byte for byte against okf.py's `mv` and `split`
  (golden and live parity), and `delete_concept` with `OkfError::StillLinked`.
- The three MCP tools in `restructure_router`.
- Tests: unit tests per op; publish database tests (replays, conflicts,
  link-into-deleted, tombstones, histories); end-to-end over HTTP; the smoke test.

## Pull requests

| Task | Issue | PR | What |
|---|---|---|---|
| 1 | #120 | #129 | `okf::ops::relink`: `rewrite_links` and `paths::relpath` |
| 2 | #121 | #130 | Replay: `ReplayOp::Unlist` and `ReplayOp::Relink` |
| 3 | #122 | #131 | `okf::ops::moves`: `move_concept`, and `mv` parity |
| 4 | #123 | #133 | `okf::ops::split`: `split_concept`, and `split` parity |
| 5 | #124 | #132 | `okf::ops::delete`: `delete_concept` and `OkfError::StillLinked` |
| 6 | #125 | #134 | The restructuring tools |
| 7 | #126 | #135 | Publish tests: moves, splits and deletes against other sessions |
| 8 | #127 | #136 | End-to-end restructuring over HTTP (`tests/restructure.rs`) |
| 9 | #128 | #137 | Smoke test, README, design update, this changelog |

Each task has its own entries under `changelog/` (`*-m6-task-NN-*.md`, plus a
`-review` entry for some rounds of review) with the detail.

## Deviations from the design

From the plan's Decisions and "Simplifications and deviations relative to the
design", all written into the design's "What M6 shipped differently":

- the listing changes are `Unlist` and `Relink` replay ops; a move across folders is
  `Unlist` plus `ListBullet`;
- link rewrites in `log.md` replay like those in `index.md`; elsewhere they are free
  edits;
- `delete_concept` refuses with `StillLinked { path, links }` (HTTP 409); an emptied
  folder keeps its `index.md`;
- `split_concept`'s `type` defaults to `Note`, and its `sources` is `--source`;
- `history` stays per path;
- no derived-column backfill.

## Found while building it, and plan corrections

Every dated note in the plan (`grep -n "Corrected" docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md`):

- **#120 (Task 1):** module docs name only code that exists; the `Relink` arm checks
  that `from` is a concept (review of PR #129).
- **#121 (Task 2):** the `Relink` arm returns `Refused` on a non-concept `from` rather
  than a `debug_assert!`, since the op is read back from the database; an `Unlist`
  with no file stays `NotFound`; three more tests (review of PR #130).
  `py_truthy_str` carried a temporary `#[cfg_attr(not(test), expect(dead_code))]`
  until Tasks 3 and 4 gave it callers and deleted it.
- **#122 (Task 3):** `MemWorkspace::check_placement` refuses an `mv` target below an
  existing file before anything is written, a third divergence from okf.py (whose
  `os.makedirs` raises); three more unit tests; the shim's git stub is a named,
  annotated function (review of PR #131).
- **#123 (Task 4):** split's index replay records `ListBullet` ahead of `Relink`. A real
  bug found in review of PR #133: in okf.py's order the replayed `Relink` made the index
  link the new concept first, `ListBullet`'s replay skipped it, and the listing was
  lost. A seventh test covers it, and the parity fixture's `splits/index.md` now links
  `parent.md#detail`. `# Errors` names `load_concept`'s errors instead of linking a
  crate-private item.
- **#124 (Task 5):** delete's emptied index keeps `"# X\n\n"` (`Unlist` removes only
  the bullet's line, as its replay does); `links_into` drops a redundant filter; nine
  tests (claims audit and review of PR #132).
- **#125 (Task 6):** `split_concept` refuses a blank `type` (shared
  `okf::ops::blank_type()`), a deliberate divergence from okf.py, which writes
  `type: ""` (review of PR #134).
- **#126 (Task 7):** Decision 18 corrected: a link to a split-out section's own anchor
  keeps resolving, because the parent keeps the heading; only subsection anchors
  become `broken-anchor` warnings. Twelve tests, not eight (Brooke's reviews of PR
  #135). Publish needed no code change.
- **#127 (Task 8):** shipped as planned (one extra assertion, no `Corrected` note).
- **#128 (Task 9, this task):** shipped as planned; the smoke test passed as written.

## Gates at the end

Final gates on this branch (`target/i128-logs/`), with Docker and `pg_knowledge_catalog`
at `85edb6f` (clean before and after):

- `cargo fmt --check`: clean (`fmt.log`).
- `cargo clippy --all-targets -- -D warnings`: clean (`clippy.log`).
- `cargo test -- --test-threads=6` (`test.log`): 566 unit, 7 golden parity (7 live
  ignored), 4 restructure, 1 roundtrip (1 ignored), 9 server, 6 writes, 0 smoke (5
  ignored), 18 doc passed; 0 failed. 611 in all.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean (`doc.log`).
- Live parity (`parity-live.log`): 7 passed of 7. Real-catalog round trip
  (`roundtrip-live.log`): 1 passed.

Against the end of M5 (539 passed by `cargo test`):

| Suite | M5 | M6 | Added |
|---|---|---|---|
| unit (`src/`) | 500 | 566 | +66 |
| `tests/server.rs` | 9 | 9 | 0 |
| `tests/writes.rs` | 6 | 6 | 0 |
| `tests/restructure.rs` | - | 4 | +4 (new) |
| golden parity | 7 | 7 | 0 (`ops.json` gained the `mv` and `split` steps) |
| roundtrip | 1 | 1 | 0 |
| doc | 16 | 18 | +2 |
| **`cargo test` total** | **539** | **611** | **+72** |
| live parity (ignored) | 7 | 7 | 0 |
| smoke (ignored) | 4 | 5 | +1 (`restructuring_on_a_real_catalog`) |

### Smoke test

`OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke --
--ignored --nocapture restructuring_on_a_real_catalog`: 1 passed in 1.80 s
(`smoke-restructure.log`). All five, serially (`--test-threads=1`,
`smoke-serial.log`): 5 passed in 7.73 s. The machine is shared (32 CPUs; `uptime`
right after the serial run gave a 1-minute load average of 8.13), so the timings are
upper bounds. The restructuring test in the serial run, verbatim:

```
start Postgres, import 226 files and start the server: 1.345032668s
create_concept: 317 bytes of JSON in 8.120373ms
publish: 371 bytes of JSON in 21.088746ms
move_concept: 191 bytes of JSON in 10.273791ms
split_concept: 473 bytes of JSON in 13.541225ms
delete_concept: 235 bytes of JSON in 7.817792ms
pending_changes: 1735 bytes of JSON in 908.624µs
publish: 336 bytes of JSON in 18.908087ms
publish wrote 4 revisions, replayed 0, 0 warnings
```

| Step | Time (release, over HTTP) |
|---|---|
| `create_concept` (builds the overlay) | 8.1 ms |
| `publish` (the probe) | 21.1 ms |
| `move_concept` (`rewrite_links` over the whole catalog) | 10.3 ms |
| `split_concept` | 13.5 ms |
| `delete_concept` (refused, `StillLinked`; the link check walks the catalog) | 7.8 ms |
| `pending_changes` | 0.9 ms |
| `publish` (4 revisions: the folder index, the moved probe, the split-out part, the old path's deletion) | 18.9 ms |

The other four tests matched M5's (snapshot of 184 documents 10.4 ms, links 168 ms,
lint 5.4 ms; import of 226 files 69 ms; every read tool over HTTP under 4 ms except
`links_report`, 163 ms then 14 ms with the cached matcher; export route 459 KB in
36 ms; the M5 write probe's publish 28.5 ms).

## Follow-ups

Accepted in M5 and still open (plan Decision 2), for things-to-fix:

- pool limits (queued writers each hold a pooled connection; a publish needs a second
  to refresh a stale snapshot);
- the overlay cache's size cap;
- `write::apply`'s `Busy` branch for a closed changeset, which no test reaches;
- a test for `create_concept`'s `parse_checked` guard.

Found in review of PR #137, for things-to-fix:

- `StillLinked.links[].source` is catalog-relative with no leading slash
  (`m6-smoke/moved.md`), while publish's revisions and the other tools use
  `/m6-smoke/...`. It predates this task (Task 5's `LinkSource`), and the smoke
  test now asserts the slash-less form, so a fix changes that assertion too.

Deferred:

- an `okf-mcp reindex` backfill, with the first change to `Derived::of`;
- `history` across renames (not planned);
- deployment, the NTP check and TLS to Postgres (M7).

Known, not filed: one `postgres:17-alpine` testcontainer, 13 hours old and not
started by this task, was still running after the final gates.
