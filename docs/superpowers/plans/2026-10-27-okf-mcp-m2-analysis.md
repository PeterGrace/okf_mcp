# OKF MCP Server, Milestone 2 (Analysis): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Port `okf.py`'s analysis commands (`lint`, `links`, `oldest`, `sprawl`, the secret scan) to Rust over an in-memory catalog snapshot, add search, and prove JSON-equal parity with `okf.py` on the synthetic corpus and the real catalog.

**Architecture:** `okf::snapshot::Snapshot::build(&dyn Workspace)` reads every file once (in parallel with `rayon`), parses frontmatter, resolves links and computes anchors, scans every file for secrets and builds the search fields. Every analysis is then a pure function over `&Snapshot` plus an injected "now" (and, for lint, optional git commit times), returning `serde::Serialize` report structs whose JSON matches `okf.py --json` field for field and key for key. `okf::search` ranks concepts in one snapshot; M3 merges across catalogs.

**Tech Stack:** Rust 2021 (toolchain 1.97.1), `regex` 1.13.1 (linear time, no lookaround), `rayon` 1.12.0, `serde` 1.0.229 (derive), `serde_json` 1.0.154 (`preserve_order`), `chrono` 0.4.45, `hegeltest` 0.50.3 for property tests, `python3` 3.14 plus `tools/okf.py` for live parity.

---

## Roadmap position

Milestone 2 of 6 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones"). M1 (core library) is on `main` at `8ecf3ba`. M3 (read-only server) consumes everything here: its read tools are thin wrappers over `Snapshot`, `links_report`, `oldest`, `sprawl`, `lint` and `search`.

## Scope

In scope (design "Milestones", M2 row, plus the design sections it references: "In-memory snapshot", "Tools / Read tools" for result shapes, "Rules enforced on every write" for the secret scan, "Time" for lint's 2-minute slack, "Testing"):

- `Workspace::list_files` (deferred from M1): every file, for the secret scan.
- Python-semantics helpers the ports need: `repr()`, `str()` of a frontmatter value, truthiness, `re.I` literals, `urllib.parse.unquote`.
- `okf::timestamp`: a port of the `datetime.fromisoformat` subset `okf.py` relies on, plus `isoformat()`.
- `okf::analysis::secrets`: `SECRET_RULES`, `ASSIGN_RE`, `_literal_secret`, `_mask`, `scan_secrets`.
- `okf::snapshot`: the catalog snapshot (`build_docs` plus anchors, backlinks, secret findings, search fields).
- `okf::analysis::links`: `link_analysis` (`links --json`).
- `okf::analysis::oldest`: `describe` and `cmd_oldest` (`oldest --json`, `--open-questions`).
- `okf::analysis::sprawl`: `sprawl_of` and `cmd_sprawl` (`sprawl --json`).
- `okf::analysis::lint`: `lint` (`lint --json`), with commit times as an input.
- `okf::search`: ranked search in one snapshot.
- Parity: the shim's new `analysis` command, new synthetic fixtures, a runtime-built secrets corpus, goldens, and live parity on the real catalog.
- Two M1 follow-ups the user chose: `yaml::scalar`'s two bools become an enum; tighten visibility (`MemWorkspace` behind a `test-util` feature, internal helpers `pub(crate)`).

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| The git module that fills `CommitTimes` for the server | M3 (clone/refresh) | M2 stays pure; lint takes commit times as an input (decision 2) |
| Snapshot swapping (`RwLock<Arc<Snapshot>>`), rebuild after fetch, per-session worktree snapshots | M3/M4 (`catalog`) | Needs clones and sessions; M2 delivers the immutable `Snapshot` they swap |
| Cross-catalog search merge | M3 (`search` tool) | Needs catalog selection; M2's `SearchHit` carries an `Ord` rank key so M3 can merge |
| Secret scan on write (refusing a write) | M4 | M2 provides `scan_secrets(path, text)`, the function M4 calls before writing |
| No-frontmatter error variant for `Doc::set_block` | M4 | Only write operations need it |
| `.okf-*.tmp` handling before `git add -A` | M4 (`publish`) | Publish concern |
| Per-session write lock, `core.symlinks=false` clones | M3 | From the M1 notes; server concerns |
| Text (non-JSON) output of `links`, `oldest`, `sprawl`, `lint` | Not planned | MCP tools return JSON plus a short summary (design "Tools") |
| `links --check` exit-code mode | Not planned | CLI-only; `lint` covers it |

## Decisions taken while planning (2026-10-27)

1. **Workflow.** M1's 37 commits were pushed to `origin/main` (`6e2a32d..8ecf3ba`). Each task is a GitHub sub-issue and a PR, reviewed by Brooke (`brooke-reviews[bot]`), which the user installed on the repository for this milestone. There is no CI: the gates below run locally and the PR reports their results.
2. **Lint and git.** `lint` takes `Option<&CommitTimes>` (path to last commit time, plus the set of dirty paths), mirroring `okf.py`'s `last_commit_times()` return value. `None` means "git unavailable", exactly as `okf.py` treats a failed `git log`. The live parity test fills it by running git itself; M3's git module fills it for the server.
3. **Search matching.** The query is split on whitespace into terms. A concept matches when every term occurs (case-insensitive substring) in at least one of its fields. Fields, best first: title or an alias, a tag, the description, the body. The tier is the best field containing the whole query (terms joined by one space), else 4 plus the best field containing any term. Deprecated concepts sort after all others; ties break by path. The snippet is the first body line containing the whole query, else any term, stripped and cut to 160 characters.
4. **M1 follow-ups included:** tighten visibility, and the `yaml::scalar` enum.

## Simplifications and deviations relative to the design

- **Module placement.** The design's crate table puts the snapshot under `catalog` (M3). M2 adds `okf::snapshot` (pure, workspace-based) and `okf::search`; M3's `catalog` will own a `RwLock<Arc<okf::snapshot::Snapshot>>`. Task 14 updates the design's crate layout table.
- **Secret findings live in the snapshot.** `Snapshot::build` scans every file (not only markdown) once, so `lint` needs no second walk. `okf.py` skips its own file (`os.path.realpath(path) == me`); the port skips `tools/okf.py`, which is that file in a catalog checkout (verified fact 9).
- **Documented divergences** (each listed in the module docs of the file that has it, and tested where testable):
  - `okf.py` crashes (`TypeError`) on a naive timestamp in `oldest` (`generated.at: 2026-10-26`) and on an unhashable `status` in `lint`; the port ignores the naive timestamp and reports the status with its `repr`.
  - `okf.py`'s fallback scan in `oldest` crashes on an `at:` value that matches `AT_RE` but is not a valid date; the port skips it.
  - `datetime.fromisoformat` week dates (`2026-W43-1`) and offsets with a fraction of a second (`+05:30:15.5`) are rejected by the port (`okf.py` accepts them). Neither occurs in a catalog. Whole-second offsets (`+05:30:15`) are accepted, as in Python.
  - A link whose normalized target escapes the catalog root is always broken in the port; `okf.py` asks the OS about the outside path.
  - Symlinks are listed per M1's listing contract (`DiskWorkspace` skips dangling and outside links that `os.walk` lists).
  - Non-UTF-8 file and directory names are skipped by `DiskWorkspace`'s walks (paths are `String`s); `os.walk` lists them, so `okf.py`'s secret scan reads such a file and the port's does not.

> **Corrected (issue #3), 2026-10-27.** Added the non-UTF-8 name divergence above, raised in review of PR #17; it is recorded as an accepted gap in `DiskWorkspace`'s listing contract (`src/okf/workspace.rs`). The shipped `list_files` doc also says a listed file's content may not be UTF-8, so callers (Task 6's `scan_all`) must tolerate read errors, which `scan_all` already does.

> **Corrected (issue #5), 2026-10-27.** The sub-minute offset divergence above is narrowed to fractional-second offsets: chrono's `FixedOffset` holds whole seconds, so `+05:30:15` parses and `isoformat()` writes it back as Python does.

## Verified facts the code relies on

Each was checked against the source or by running it on 2026-10-27.

1. **`okf.py` is unchanged since the M1 goldens:** `sha256sum tools/okf.py` = `b528dd51672c7617756efdd85ae8b17284c9b13b0c8d6e33b31f8532087b1e22`, equal to `tests/golden/SOURCE`.
2. **`okf.py` functions ported** (line numbers in `~/repos/pg_knowledge_catalog/tools/okf.py`): `walk_md` 598, `is_concept` 606, `split_frontmatter` 823, `body_split` 893, `resolve_link` 918, `prose_lines` 933, `_fm_fallback` 948, `build_docs` 961, `names_for` 996, `link_analysis` 1005, `AT_RE`/`_ats`/`describe`/`cmd_oldest` 1144-1233, `VOLATILE_RE`/`DATE_RE`/`LEADIN_RE`/`SPRAWL_*`/`volatile_reason`/`sprawl_of`/`cmd_sprawl` 1236-1337, `INDEX_HEADING_RE`/`_index_bullet` 1400-1404, `SECRET_RULES`/`ASSIGN_RE`/`PLACEHOLDER_RE`/`_literal_secret`/`_mask`/`scan_secrets` 1734-1791, `last_commit_times` 1798, `check_ts` 1815, `lint` 1835, `ISO_RE` 40, `FUTURE_SLACK` 41, `STATUSES` 36, `OUTCOMES` 37, `parse_ts` 57. `main()` (2209) accepts `--root` after the subcommand (each subparser has `parents=[common]`).
3. **Python 3.14 `datetime.fromisoformat`** (after `okf.py`'s `.replace("Z", "+00:00")`): accepts `2026-10-26T10:00` (no seconds), a space or any single character as separator (`2026-10-26X10:00:00`), basic format `20261026T100000`, `,` as decimal mark, more than 6 fraction digits (truncated), `24:00:00` (next midnight), `+HHMM` and `+23:59` offsets; a date alone gives a *naive* midnight. It rejects second 60, offset `+24:00`, `2026-02-29`, and non-ASCII digits (`２０２６-...`, though `ISO_RE`'s `\d` lets them through to that error). **Added (issue #5), checked against Python 3.14.4's C parser:** one stray character between the time and the offset is ignored (`10:00:00x+05:00`, `10:00:00.Z`, `10:+05:00`); offset fields are only range-checked as a whole (`+0099` is `+01:39`, `+01:60` is `+02:00`, `+23:60` fails); whole-second offsets parse (`-05:30:15`); a zero offset drops its fraction (`+00:00:00.5` is UTC); the basic time form takes a fraction without a decimal mark (`10000012` is `.120000`); and `24:00` moves the date *before* the date is validated (`2026-10-00T24:00` is `2026-10-01`, `0000-12-31T24:00` is `0001-01-01`).
4. **Python `round(x, 1)` equals Rust `format!("{:.1}", x)`** (both round the exact binary value, ties to even): checked for 0.25→0.2, 0.35→0.3, 2.675→2.7, 18.75→18.8, 0.05→0.1, 0.15→0.1, 0.45→0.5. So the port rounds with `format!("{:.1}", x).parse::<f64>()`.
5. **Float JSON:** serde_json 1.0.154 writes `f64` with `zmij` shortest round-trip (`serde_json-1.0.154/src/ser.rs:1716-1723`): `18.0`→`18.0`, `18.8`→`18.8`, `1e16`→`1e+16`, the same as Python's `json.dumps`. `to_string_pretty` uses two-space indent like `json.dumps(indent=2)`. Python's `str(18.0)` is `18.0`; Rust's `{:?}` (Debug) for `f64` gives `18.0` while `{}` gives `18`, so lint's sprawl message formats the score with `{:?}`.
6. **`regex` 1.13.1 case folding:** `(?i)` matches `K`/`k`/U+212A, `s`/U+017F, `ß`/`ẞ`, `µ`/`μ` like Python's `re.I`, but not `i`/`I` against U+0130 `İ` or U+0131 `ı`. Python's `re.I` makes all four of `i`, `I`, `İ`, `ı` match each other, and `[a-z]` match `İ` and `ı`. Ported `re.I` patterns therefore spell out `[iI\u{130}\u{131}]` (and add `\u{130}\u{131}` to `[a-z]`), as M1 did.
7. **`regex` has no lookaround or backreferences.** A lookbehind `(?<![X])` becomes a consumed `(?:\A|[^X])` before the match with the real match in a capture group, and a scan resumes at the group's end (M1's `find_links` technique, `src/okf/markdown.rs:100`). A trailing `\b` after a word character becomes a consumed `(?:[^\p{L}\p{N}_]|\z)`; a leading `\b` before a word character becomes `(?:\A|[^\p{L}\p{N}_])`. `ASSIGN_RE`'s `\2?` (an optional repeat of the opening quote) only consumes a closing quote that the value class `[^\s"'...]` already excludes, so dropping it changes no captured value and no later match start.
8. **Unicode classes compile:** `[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]` builds in `regex` 1.13.1 and classifies U+00AD, U+2028, U+00A0, space and U+0378 as non-printable, `a` and U+1F600 as printable (Python's `str.isprintable` is false for Cc, Cf, Cs, Co, Cn, Zl, Zp, Zs except the ASCII space).
9. **Real-catalog baseline:** `python3 tools/okf.py lint --json` on `pg_knowledge_catalog` gives 0 errors and 2 warnings (both `sprawl`) in 0.29 s. `scan_secrets` on `tools/okf.py` itself reports one `secret:assignment` hit (line 1789), which `okf.py` suppresses only because it skips its own file. Non-markdown extensions in the catalog: `.py`, `.sh`, `.txt`, none.
10. **Python `repr` of str:** `repr("a'b")` = `"a'b"`, `repr("a\"b'")` = `'a"b\''`, `repr("x\u200by\tz\xe9\x85")` = `'x\u200by\tzé\x85'` (U+200B is category Cf, so it is escaped; `é` is printable and kept). **`urllib.parse.unquote("a%20b%zz%C3%A9%FF")`** = `"a b%zz\xe9\ufffd"` (Python escapes; the result holds the real `é` and U+FFFD). Its UTF-8 `errors="replace"` decoding replaces maximal invalid subparts, as Rust's `String::from_utf8_lossy` does, so decoding all `%XX` bytes and literal characters into one byte buffer and calling `from_utf8_lossy` gives the same result.
11. **`rayon` 1.12.0:** `IntoParallelRefIterator::par_iter` (`rayon-1.12.0/src/iter/mod.rs:262-295`) and `ParallelIterator::collect` (`:2054`) into `Vec<T>` or `Result<Vec<T>, E>` keep the input order for indexed iterators (checked by running). Minimum Rust 1.80.
12. **`chrono` 0.4.45** with only the `now` feature (which enables `std` and `alloc`): `NaiveDate::from_ymd_opt` (`naive/date/mod.rs:196`), `NaiveTime::from_hms_micro_opt` (`naive/time/mod.rs:377`), `NaiveDateTime::new` (`naive/datetime/mod.rs:99`), `checked_add_days` (`:756`), `FixedOffset::east_opt` (`offset/fixed.rs:62`) and `local_minus_utc` (`:107`), `DateTime::from_naive_utc_and_offset` (`datetime/mod.rs:94`), `naive_local` (`:576`), `TimeDelta::num_microseconds` (`time_delta.rs:347`), `DateTime::from_timestamp` (`datetime/mod.rs:803`), `with_timezone` (`:412`), `fixed_offset` (`:420`), `DateTime::<Utc>::UNIX_EPOCH` (`:917`), `NaiveDate::and_hms_opt` (`naive/date/mod.rs:780`), `Timelike::nanosecond`. `DateTime<Tz>`'s `Ord` compares instants.
13. **M1 API used here:** `Workspace` (`src/okf/workspace.rs:21`; `read_text` is the universal-newline read, `list_markdown` the `walk_md` port), `DiskWorkspace::collect` (`:209`), `MemWorkspace` (`:412`), `find_links` (`src/okf/markdown.rs:100`, returns `LinkMatch { text, target, range, target_range }`), `split_frontmatter` (`:136`), `body_split` (`:157`), `headings` (`:183`, returns `Heading { index, level, text, slug }` with GitHub slugs), `anchors_in` (`:289`, not lowercased), `resolve_link` (`:340`, `Option<ResolvedLink { path, absolute }>`), `Doc::from_text` (`src/okf/doc.rs:169`), `Doc::parse` (`:261`), `Doc::block` (`:252`), `Doc::key_line` (`:282`), `Doc::frontmatter` (`:223`), `paths::{normpath, join, dirname, basename, escapes_root, is_concept, walk_order, NON_CONCEPTS, RESERVED}` (`src/okf/paths.rs`), `pystr::{is_py_word, py_strip, py_lstrip, universal_newlines}` (`src/okf/pystr.rs`), `yaml::Value` (`Null | Str | Seq | Map`, `src/okf/yaml.rs:33`), `clock::{Clock, FixedClock, TIMESTAMP_FORMAT}`.
14. **Brooke** is `brooke-reviews[bot]`; it skips a PR whose branch is behind `main` and re-triggers on a `/prrb` comment (catalog concept `/projects/autobots.md`).

## Conventions for every task

- Work in the worktree and branch the coordinator gives you (`.worktrees/okf_mcp-<issue>` off `origin/main`). Never edit the main checkout.
- The user is a Python expert and a Rust novice: comment Rust-specific ideas (`Cow`, `LazyLock`, `let ... else`, `?`, trait objects, `rayon`), as M1's code does.
- No `.unwrap()` outside tests; `.expect("why this can't fail")` only for invariants (regex literals, poisoned locks).
- Every public item has a doc comment with Arguments, Returns and Errors where they apply.
- Ported regexes: Python `\w` is `[\p{L}\p{N}_]`, `\s` is `[\s\x1c-\x1f]`, `re.I` needs `[iI\u{130}\u{131}]` (verified fact 6), and lookaround is emulated (verified fact 7). Use `okf::pystr` for strip, `isspace` and `\w`.
- Report structs derive `Debug, Clone, PartialEq, serde::Serialize`, with fields in exactly `okf.py`'s dict key order (the parity tests compare serialized text, so key order matters).
- Tests: Arrange-Act-Assert, `#[cfg(test)] mod tests` beside the code, `MemWorkspace` for catalogs, `FixedClock`-style fixed instants. No commented-out tests.
- **Gates** (all must pass before every push; paste the counts into the PR):
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test` (unit, doc and golden tests)
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - From Task 12 on, also `cargo test --test parity -- --ignored` with `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog` (needs `python3`).
- **Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m2-task-<NN>-<slug>.md`, with Added/Changed/Notes sections like `changelog/2026-10-27T014412Z-m1-core-library.md`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `src/okf/workspace.rs` | T1 (modify), T13 (modify) | `Workspace::list_files`; `MemWorkspace` gating |
| `src/okf/pystr.rs` | T2 (modify), T13 (modify) | `py_repr`, `py_str`, `py_truthy`, `ignorecase_literal`, `py_unquote` |
| `src/okf/timestamp.rs` | T3 (create) | `PyDateTime`, `parse_iso`, `isoformat`, `is_iso_timestamp` |
| `src/okf/yaml.rs` | T2, T4, T13 (modify) | Error messages use `repr`; `ScalarStyle`; visibility |
| `src/okf/doc.rs` | T2, T4, T13 (modify) | Error messages use `repr`; `scalar` call sites; visibility |
| `src/okf/markdown.rs` | T9 (modify) | `pub(crate) fn fence_marker` for sprawl's code-line set |
| `src/okf/mod.rs` | T3, T5, T6, T11, T13 (modify) | `pub mod` lines (keep both sides on conflict) |
| `Cargo.toml` | T5 (`serde`), T6 (`rayon`), T13 (`test-util` feature) | Dependencies |
| `src/okf/analysis/mod.rs` | T5 (create); T7, T8, T9, T10 (modify) | `Finding`; `pub mod` lines |
| `src/okf/analysis/secrets.rs` | T5 (create) | Secret rules and `scan_secrets` |
| `src/okf/snapshot.rs` | T6 (create), T11 (modify) | `Snapshot`, `SnapshotDoc`, `SnapshotLink`, `fm_fallback` |
| `src/okf/analysis/links.rs` | T7 (create) | `links_report` |
| `src/okf/analysis/oldest.rs` | T8 (create) | `oldest` |
| `src/okf/analysis/sprawl.rs` | T9 (create) | `sprawl_of`, `sprawl` |
| `src/okf/analysis/lint.rs` | T10 (create) | `CommitTimes`, `lint` |
| `src/okf/analysis/testdata/lint-mini.json` | T10 (create) | `okf.py`'s lint output for the unit-test catalog |
| `src/okf/search.rs` | T11 (create) | `SearchFields`, `SearchQuery`, `SearchHit`, `search` |
| `tests/parity/okf_shim.py` | T12 (modify) | `analysis` command |
| `tests/parity.rs` | T12 (modify), T13 (modify) | Analysis goldens and live parity |
| `tests/fixtures/catalog/analysis/**`, `tests/fixtures/commit-times.json` | T12 (create) | Analysis corpus |
| `tests/golden/*.json` | T12 (regenerate) | Goldens |
| `tests/smoke.rs` | T14 (create) | Every analysis on a real catalog, with timings |
| `README.md` | T14 (create) | What the crate is, how to test |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T14 (modify) | Crate layout; M2 delivered |
| `changelog/*-m2-*.md` | every task | Iteration notes |

## Task dependencies

```
T1 list_files ───┐
T5 secrets ──────┴─> T6 snapshot ─┬─> T7 links ────────────────┐
                                  ├─> T8 oldest ──> T9 sprawl ─┼─> T10 lint ──┐
                                  └─> T11 search ──────────────┼──────────────┴─> T12 parity ──> T13 visibility ──> T14 smoke/README/changelog
T2 pystr helpers ──> T7, T8, T10  (and T13)                    │
T3 timestamp ─────> T8, T10                                    │
T4 scalar enum ───> T13                                        │
```

Edges, explicitly:

- T1 -> T6; T5 -> T6
- T2 -> T7, T8, T10 (helpers; T10's parse-error messages also need T2's repr change)
- T3 -> T8, T10
- T6 -> T7, T8, T9, T11
- T8 -> T9 (`round_one`)
- T7 -> T10; T9 -> T10
- T7, T8, T9, T10, T11 -> T12
- T1, T2, T4, T12 -> T13
- every task -> T14

Tasks with no dependencies, which can start straight away: **T1, T2, T3, T4, T5**.

---

### Task 1: `Workspace::list_files`

The secret scan walks every file, not just markdown, skipping only `.git` and `.worktrees` directories at any depth (`okf.py` `lint`, around line 1957). M1 deferred this method.

**Files:**
- Modify: `src/okf/workspace.rs` (trait, `DiskWorkspace::collect`, `MemWorkspace`, tests)
- Create: `changelog/<ts>-m2-task-01-list-files.md`

- [ ] **Step 1: Write the failing tests**

In `src/okf/workspace.rs`'s test module, grow `FILES` with a `.git` file, a nested `.worktrees` file and a nested `.git` directory, and assert the new listing in `assert_contract` (which runs for both workspaces):

```rust
    const FILES: [(&str, &str); 10] = [
        ("index.md", "root"),
        ("b.md", "b"),
        ("infra/index.md", "infra"),
        ("infra/hosts/x.md", "x"),
        (".claude/skill.md", "hidden"),
        ("notes.txt", "not markdown"),
        ("crlf.txt", "a\r\nb\rc\n"),
        (".git/config", "git internals"),
        (".worktrees/s1/index.md", "a session worktree"),
        ("infra/.git/HEAD", "a nested repository"),
    ];
```

Add to `assert_contract`, right after the `list_markdown` assertion:

```rust
        // Every file, in os.walk order (files before subdirectories), skipping
        // only `.git` and `.worktrees` directories. Other dot-directories count.
        assert_eq!(
            ws.list_files().unwrap(),
            [
                "b.md",
                "crlf.txt",
                "index.md",
                "notes.txt",
                ".claude/skill.md",
                "infra/index.md",
                "infra/hosts/x.md",
            ]
        );
```

In the Unix-only test module (beside `write_onto_a_fifo_is_refused`), add:

```rust
        #[test]
        fn listings_skip_special_files() {
            let (_dir, ws) = disk_catalog(&FILES);
            let made = std::process::Command::new("mkfifo")
                .arg(ws.root().join("pipe.md"))
                .status();
            if !made.is_ok_and(|status| status.success()) {
                return; // no mkfifo on this system
            }
            // Reading a FIFO would block forever, so neither walk lists it.
            assert!(!ws.list_files().unwrap().contains(&"pipe.md".to_string()));
            assert!(!ws.list_markdown().unwrap().contains(&"pipe.md".to_string()));
        }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib workspace`
Expected: compile error, `no method named list_files`.

- [ ] **Step 3: Add the trait method**

In the `Workspace` trait, after `list_markdown`:

```rust
    /// Every regular file of any type, skipping only directories named
    /// `.git` or `.worktrees` (at any depth), in `os.walk` order (see
    /// [`walk_order`]). This is `okf.py` `lint`'s secret-scan walk: unlike
    /// [`Workspace::list_markdown`], other dot-directories (`.claude`) are
    /// included. The same symlink rules apply: every returned path is
    /// readable through this workspace.
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed.
    fn list_files(&self) -> Result<Vec<String>, OkfError>;
```

- [ ] **Step 4: Generalize `DiskWorkspace::collect`**

Above `impl DiskWorkspace`, add:

```rust
/// Which files a directory walk returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listing {
    /// `.md` files outside dot-directories (`okf.py`'s `walk_md`).
    Markdown,
    /// Every file outside `.git` and `.worktrees` directories (`lint`'s secret walk).
    AllFiles,
}

impl Listing {
    /// Does the walk descend into a directory with this name?
    fn descends_into(self, dir_name: &str) -> bool {
        match self {
            Listing::Markdown => !dir_name.starts_with('.'),
            Listing::AllFiles => !matches!(dir_name, ".git" | ".worktrees"),
        }
    }

    /// Does the walk list a file with this name?
    fn includes(self, file_name: &str) -> bool {
        match self {
            Listing::Markdown => file_name.ends_with(".md"),
            Listing::AllFiles => true,
        }
    }
}
```

Replace `collect` with a version that takes the listing (the symlink handling is unchanged; the new part is skipping FIFOs, sockets and devices, which a read would block on or fail):

```rust
    /// Recursively lists the files `listing` selects under `dir`, whose
    /// catalog-relative name is `prefix`.
    fn collect(
        &self,
        listing: Listing,
        dir: &Path,
        prefix: &str,
        out: &mut Vec<String>,
    ) -> Result<(), OkfError> {
        let io_err = |source| OkfError::Io {
            path: prefix.to_string(),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                tracing::warn!(?file_name, "skipping non-UTF-8 file name");
                continue;
            };
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            // `file_type` does not follow symlinks, so a link to a directory
            // is not a directory here and is never descended into.
            let file_type = entry.file_type().map_err(io_err)?;
            if file_type.is_dir() {
                if listing.descends_into(name) {
                    self.collect(listing, &entry.path(), &rel, out)?;
                }
                continue;
            }
            if !listing.includes(name) {
                continue;
            }
            if file_type.is_symlink() {
                let path = entry.path();
                let skip_reason = match self.resolve(&path) {
                    Resolution::Missing => Some("dangling symlink"),
                    Resolution::Outside => Some("symlink resolves outside the catalog"),
                    Resolution::Inside if !path.is_file() => Some("symlink to a non-file"),
                    Resolution::Inside => None,
                };
                if let Some(reason) = skip_reason {
                    tracing::warn!(path = %rel, reason, "not listing symlink");
                    continue;
                }
            } else if !file_type.is_file() {
                tracing::warn!(path = %rel, "not listing a special file (FIFO, socket or device)");
                continue;
            }
            out.push(rel);
        }
        Ok(())
    }

    /// Runs one walk from the root and sorts it into `os.walk` order.
    fn listing(&self, listing: Listing) -> Result<Vec<String>, OkfError> {
        let mut out = Vec::new();
        self.collect(listing, &self.root, "", &mut out)?;
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
```

In `impl Workspace for DiskWorkspace`, replace `list_markdown` and add `list_files`:

```rust
    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        self.listing(Listing::Markdown)
    }

    fn list_files(&self) -> Result<Vec<String>, OkfError> {
        self.listing(Listing::AllFiles)
    }
```

- [ ] **Step 5: Implement it for `MemWorkspace`**

Next to `in_dot_directory`, add:

```rust
/// Is any directory component `.git` or `.worktrees`?
fn in_skipped_directory(path: &str) -> bool {
    dirname(path)
        .split('/')
        .any(|p| matches!(p, ".git" | ".worktrees"))
}
```

In `impl Workspace for MemWorkspace`:

```rust
    fn list_files(&self) -> Result<Vec<String>, OkfError> {
        let mut out: Vec<String> = self
            .files()
            .keys()
            .filter(|k| !in_skipped_directory(k))
            .cloned()
            .collect();
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
```

Update the `DiskWorkspace` doc comment's listing contract to say "`list_markdown` and `list_files` return only paths readable through this workspace ... special files (FIFOs, sockets, devices) are never listed."

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib workspace`
Expected: PASS (the existing `list_markdown` assertions still pass: the new `FILES` entries are in dot-directories or nested `.git`).

- [ ] **Step 7: Gates, changelog, commit**

Run the four gates. Write `changelog/<ts>-m2-task-01-list-files.md` (Added: `list_files`; Changed: both walks skip special files). Commit:

```bash
git add src/okf/workspace.rs changelog/
git commit -m "workspace: add list_files for the secret scan; skip special files in walks"
```

---

### Task 2: Python-semantics helpers in `okf::pystr`

Lint messages embed Python `repr()` of frontmatter values; `oldest` uses `str()`; lint tests truthiness (`not data.get("type")`); the unlinked-mention patterns are `re.escape(name)` with `re.I`; anchor fragments go through `urllib.parse.unquote`. One home for all of them.

**Files:**
- Modify: `src/okf/pystr.rs`
- Modify: `src/okf/yaml.rs`, `src/okf/doc.rs` (error messages only)
- Create: `changelog/<ts>-m2-task-02-pystr-helpers.md`

- [ ] **Step 1: Write the failing tests**

Append to the test module in `src/okf/pystr.rs` (expected values come from Python 3.14; verified facts 6, 8 and 10):

```rust
    use crate::okf::yaml::Value;
    use indexmap::IndexMap;

    #[test]
    fn repr_str_picks_quotes_and_escapes_like_python() {
        assert_eq!(py_repr_str("abc"), "'abc'");
        assert_eq!(py_repr_str("a'b"), "\"a'b\"");
        assert_eq!(py_repr_str("a\"b'"), "'a\"b\\''");
        assert_eq!(py_repr_str("x\u{200b}y\tz\u{e9}\u{85}"), "'x\\u200by\\tz\u{e9}\\x85'");
        assert_eq!(py_repr_str("\\\n\r\u{7f}\u{0}"), "'\\\\\\n\\r\\x7f\\x00'");
        assert_eq!(py_repr_str("\u{a0} \u{1f600}\u{e0000}"), "'\\xa0 \u{1f600}\\U000e0000'");
    }

    #[test]
    fn repr_and_str_of_values_follow_python() {
        let mut map = IndexMap::new();
        map.insert("by".to_string(), Value::Str("x".to_string()));
        map.insert("at".to_string(), Value::Null);
        let value = Value::Seq(vec![Value::Map(map), Value::Seq(Vec::new())]);
        assert_eq!(py_repr(&value), "[{'by': 'x', 'at': None}, []]");
        assert_eq!(py_repr(&Value::Map(IndexMap::new())), "{}");
        assert_eq!(py_str(&Value::Str("plain".to_string())), "plain");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_repr_opt(None), "None");
    }

    #[test]
    fn truthiness_follows_python() {
        assert!(!py_truthy(None));
        assert!(!py_truthy(Some(&Value::Null)));
        assert!(!py_truthy(Some(&Value::Str(String::new()))));
        assert!(!py_truthy(Some(&Value::Seq(Vec::new()))));
        assert!(!py_truthy(Some(&Value::Map(IndexMap::new()))));
        assert!(py_truthy(Some(&Value::Str("0".to_string()))));
    }

    #[test]
    fn ignorecase_literal_matches_like_python_re_i() {
        let pattern = Regex::new(&format!("^(?i:{})$", ignorecase_literal("Time.Server"))).unwrap();
        assert!(pattern.is_match("TIME.SERVER"));
        assert!(pattern.is_match("t\u{130}me.server"));
        assert!(pattern.is_match("t\u{131}me.server"));
        assert!(!pattern.is_match("timeXserver"), "the dot is escaped");
        let dotted = Regex::new(&format!("^(?i:{})$", ignorecase_literal("\u{130}x"))).unwrap();
        assert!(dotted.is_match("ix") && dotted.is_match("IX"));
    }

    #[test]
    fn unquote_matches_urllib() {
        assert_eq!(py_unquote("plain"), "plain");
        assert_eq!(py_unquote("a%20b%zz%C3%A9%FF"), "a b%zz\u{e9}\u{fffd}");
        assert_eq!(py_unquote("%E2%82\u{e9}"), "\u{fffd}\u{e9}");
        assert_eq!(py_unquote("100%"), "100%");
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib pystr`
Expected: compile errors for the missing functions.

- [ ] **Step 3: Implement**

Add to `src/okf/pystr.rs` (new imports: `std::fmt::Write as _`, `crate::okf::yaml::Value`):

```rust
/// Python's `repr()` of a `str`: single quotes unless the text has a `'` and
/// no `"`; backslash escapes for the quote, `\\`, `\t`, `\n`, `\r`; `\xNN` for
/// other control characters and for non-printable characters up to U+00FF;
/// `\uNNNN`/`\UNNNNNNNN` above. Printable characters stay as they are.
///
/// "Printable" is Python's `str.isprintable`: not in Unicode categories Cc,
/// Cf, Co, Cn, Zl, Zp or Zs, except the ASCII space. (Python 3.14 and the
/// `regex` crate may disagree on a few recently assigned code points.)
///
/// # Examples
/// ```
/// use okf_mcp::okf::pystr::py_repr_str;
/// assert_eq!(py_repr_str("it's"), "\"it's\"");
/// ```
pub fn py_repr_str(text: &str) -> String {
    static NON_PRINTABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$")
            .expect("NON_PRINTABLE regex is valid")
    });
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    let mut buffer = [0u8; 4];
    for c in text.chars() {
        // `write!` into a String cannot fail; `let _ =` discards the `Ok(())`.
        match c {
            c if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c < ' ' || c == '\u{7f}' => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if c.is_ascii() => out.push(c),
            c if !NON_PRINTABLE.is_match(c.encode_utf8(&mut buffer)) => out.push(c),
            c if (c as u32) <= 0xff => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a parsed frontmatter value (`None`, `'text'`,
/// `['a', 'b']`, `{'k': 'v'}`), as `okf.py` embeds in lint messages.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Str(text) => py_repr_str(text),
        Value::Seq(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `repr(d.get(key))`: `None` for a missing key.
pub fn py_repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_string(), py_repr)
}

/// Python's `str()` of a frontmatter value: the text itself for a string,
/// otherwise its `repr()`.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        Value::Null | Value::Seq(_) | Value::Map(_) => py_repr(value),
    }
}

/// Python truthiness of `d.get(key)`: false for a missing key, `None`, an
/// empty string, an empty list and an empty mapping.
pub fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Str(text)) => !text.is_empty(),
        Some(Value::Seq(items)) => !items.is_empty(),
        Some(Value::Map(map)) => !map.is_empty(),
    }
}

/// `re.escape(literal)` for use inside a `(?i:...)` group, matching what
/// Python's `re.I` matches. The `regex` crate's case folding agrees with
/// Python's except that Python lets `i`, `I`, U+0130 and U+0131 all match
/// each other, so those four become the class `[iI\u{130}\u{131}]`.
pub fn ignorecase_literal(literal: &str) -> String {
    let escaped = regex::escape(literal);
    // `regex::escape` only adds backslashes before ASCII punctuation, so no
    // escape sequence contains one of the four letters replaced here.
    let mut out = String::with_capacity(escaped.len());
    for c in escaped.chars() {
        match c {
            'i' | 'I' | '\u{130}' | '\u{131}' => out.push_str("[iI\u{130}\u{131}]"),
            c => out.push(c),
        }
    }
    out
}

/// Python's `urllib.parse.unquote(text)` (UTF-8, `errors="replace"`):
/// `%XX` escapes become bytes, invalid UTF-8 becomes U+FFFD (maximal
/// subparts, as Python's decoder does), and a `%` not followed by two hex
/// digits stays as it is. Returns the input without allocating when it has
/// no `%`.
pub fn py_unquote(text: &str) -> Cow<'_, str> {
    if !text.contains('%') {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' {
            if let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) {
                // Two hex digits fit a byte, so the cast cannot truncate.
                decoded.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    // Literal characters are copied as whole UTF-8 sequences and a decoded
    // lead byte never merges with a following literal (which starts with a
    // lead byte or ASCII), so one lossy pass equals Python's per-run decoding.
    Cow::Owned(String::from_utf8_lossy(&decoded).into_owned())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib pystr`
Expected: PASS. If a `py_repr_str` expectation fails, check it with `python3 -c 'print(repr("..."))'` and fix the code, not the expectation.

- [ ] **Step 5: YAML error messages quote with Python `repr`**

`lint` reports `frontmatter does not parse: {message}`, so the parser's messages must be `okf.py`'s byte for byte. `okf.py` quotes source text with `!r`; M1 used Rust's `{:?}`, which differs (`"a"` versus `'a'`, `\u{e9}` versus `é`). Replace every `{:?}`/`{x:?}` in a `YamlError` message with `py_repr_str`. The ten sites (`grep -n ':?}' src/okf/yaml.rs src/okf/doc.rs`, excluding test assertions), with their `okf.py` originals:

| File:line (M1) | Rust after the change | `okf.py` |
|---|---|---|
| `src/okf/yaml.rs:136` | `format!("{message} in {}", py_repr_str(self.source))` | `f"{msg} in {self.s!r}"` (90) |
| `src/okf/yaml.rs:159` | `format!("unsupported YAML indicator {}", py_repr_str(c.encode_utf8(&mut [0u8; 4])))` | 111 |
| `src/okf/yaml.rs:306` | `format!("duplicate key {}", py_repr_str(&key))` (in `flow_map`) | 197 |
| `src/okf/yaml.rs:397` | `format!("unquoted ': ' inside a plain value {}; quote it", py_repr_str(text))` | 233 |
| `src/okf/yaml.rs:518` | `format!("bad indentation or structure: {}", py_repr_str(py_strip(text)))` | 282 |
| `src/okf/yaml.rs:559` | `format!("bad indentation: {}", py_repr_str(py_strip(text)))` | 316 |
| `src/okf/yaml.rs:565` | `format!("expected 'key: value', got {}", py_repr_str(py_strip(text)))` | 319 |
| `src/okf/yaml.rs:592` | `format!("duplicate key {}", py_repr_str(&key))` | 330 |
| `src/okf/doc.rs:517` | `format!("duplicate key {}", py_repr_str(&b.key))` | 467 |
| `src/okf/doc.rs:534` | `format!("line is not part of any key: {}", py_repr_str(py_strip(line)))` | 474 |

> **Corrected (issue #4), 2026-10-27.** The table listed eight sites; there are ten. Added `yaml.rs:159` (`unsupported YAML indicator`, unchanged output for its ASCII indicators but converted for consistency) and `yaml.rs:306` (`duplicate key` in `flow_map`, which `tests/fixtures/catalog/errors/flow-duplicate-key.md` exercises and Task 10's lint parity depends on). `yaml.rs` gained `flow_errors_quote_like_python_repr` for both; `parse_errors_quote_like_python_repr` asserts the full message `expected ',' or ']' in flow list in '[\u{e9}'` rather than its suffix.

Add a test to `src/okf/doc.rs`'s test module:

```rust
    #[test]
    fn parse_errors_quote_like_python_repr() {
        let error = doc("---\na: 1\na: 2\n---\n").parse().unwrap_err();
        assert_eq!(error.message, "duplicate key 'a'");
        let error = doc("---\nk: [\u{e9}\n---\n").parse().unwrap_err();
        assert!(error.message.ends_with("in '[\u{e9}'"), "{}", error.message);
    }
```

Fix any unit test that asserted a `{:?}`-style message. `tests/parity.rs`'s `error_kind` keeps working (it cuts at the first quote of either kind).

> **Corrected (issue #4), 2026-10-27.** `error_kind` is gone (review of PR #18): with repr quoting the messages match `okf.py` in full, so `tests/parity.rs` and `tests/parity/okf_shim.py` compare the whole message as `error_message`, and `tests/golden/frontmatter.json` was regenerated with it. Every corpus file whose frontmatter fails to parse now checks message text byte for byte.

- [ ] **Step 6: Gates, changelog, commit**

```bash
git add src/okf/pystr.rs src/okf/yaml.rs src/okf/doc.rs changelog/
git commit -m "pystr: Python repr, str, truthiness, re.I literals and unquote; YAML errors quote with repr"
```

---

### Task 3: `okf::timestamp` (port of `parse_ts`, `isoformat`, `ISO_RE`)

`okf.py`'s `parse_ts(s)` is `datetime.fromisoformat(s.replace("Z", "+00:00"))`. `lint` only calls it after `ISO_RE` matched; `oldest` calls it on any `at:` string and on `stale_after`. The result may be naive (no offset), which `okf.py` can't compare with an aware time, so callers treat naive values as unusable (documented divergence).

**Files:**
- Create: `src/okf/timestamp.rs`
- Modify: `src/okf/mod.rs` (add `pub mod timestamp;`)
- Create: `changelog/<ts>-m2-task-03-timestamp.md`

- [ ] **Step 1: Write the failing tests**

Create `src/okf/timestamp.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn aware(text: &str) -> DateTime<FixedOffset> {
        match parse_iso(text) {
            Some(PyDateTime::Aware(t)) => t,
            other => panic!("{text}: expected aware, got {other:?}"),
        }
    }

    #[test]
    fn accepts_what_python_fromisoformat_accepts() {
        // Expected isoformat() output from Python 3.14 (verified fact 3).
        let cases = [
            ("2026-10-26T10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26T10:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26 10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26X10:00:00Z", "2026-10-26T10:00:00+00:00"),
            ("20261026T100000Z", "2026-10-26T10:00:00+00:00"),
            ("2026-10-26T10:00:00.5+0530", "2026-10-26T10:00:00.500000+05:30"),
            ("2026-10-26T10:00:00,5Z", "2026-10-26T10:00:00.500000+00:00"),
            ("2026-10-26T10:00:00.1234567Z", "2026-10-26T10:00:00.123456+00:00"),
            ("2026-10-26T24:00:00Z", "2026-10-27T00:00:00+00:00"),
            ("2026-10-26T10:00:00+23:59", "2026-10-26T10:00:00+23:59"),
            ("2026-10-26T10", "2026-10-26T10:00:00"),
        ];
        for (input, python) in cases {
            let parsed = parse_iso(input).unwrap_or_else(|| panic!("{input} should parse"));
            assert_eq!(parsed.isoformat(), python, "{input}");
        }
        assert!(matches!(parse_iso("2026-10-26"), Some(PyDateTime::Naive(_))));
    }

    #[test]
    fn rejects_what_python_rejects() {
        for input in [
            "2026-10-26T23:59:60Z",
            "2026-10-26T10:00:00+24:00",
            "2026-02-29T10:00:00Z",
            "\u{ff12}\u{ff10}\u{ff12}\u{ff16}-10-26T10:00:00Z",
            "2026-10-26T24:00:01Z",
            "0000-01-01T00:00:00Z",
            "2026-10",
            "not a date",
            "",
        ] {
            assert!(parse_iso(input).is_none(), "{input:?} should be rejected");
        }
    }

    #[test]
    fn iso_shape_matches_okf_py_iso_re() {
        assert!(is_iso_timestamp("2026-10-26T10:00:00Z"));
        assert!(is_iso_timestamp("2026-10-26T10:00:00.5-04:00"));
        assert!(is_iso_timestamp("2026-10-26T10:00:00Z\n"), "Python's $ allows one trailing newline");
        assert!(!is_iso_timestamp("2026-10-26T10:00Z"));
        assert!(!is_iso_timestamp("2026-10-26T10:00:00+0400"));
        assert!(!is_iso_timestamp("2026-10-26T10:00:00Z\n\n"));
    }

    #[test]
    fn comparisons_use_the_instant() {
        assert_eq!(aware("2026-10-26T10:00:00Z"), aware("2026-10-26T06:00:00-04:00"));
        assert!(aware("2026-10-26T10:00:01Z") > aware("2026-10-26T06:00:00-04:00"));
    }

    #[test]
    fn isoformat_uses_z_only_through_the_caller() {
        // okf.py writes `.isoformat().replace("+00:00", "Z")`; that replace is
        // the caller's job (see `oldest`), so isoformat keeps `+00:00`.
        assert_eq!(aware("2026-10-26T10:00:00Z").to_string_iso(), "2026-10-26T10:00:00+00:00");
    }

    #[hegel::test]
    fn isoformat_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        let seconds: i64 = tc.draw(generators::integers::<i64>().min_value(0).max_value(4_102_444_800));
        let micros: u32 = tc.draw(generators::integers::<u32>().max_value(999_999));
        let offset_minutes: i32 = tc.draw(generators::integers::<i32>().min_value(-1439).max_value(1439));
        let offset = FixedOffset::east_opt(offset_minutes * 60).expect("in range");
        let instant = DateTime::from_timestamp(seconds, micros * 1000)
            .expect("in range")
            .with_timezone(&offset);
        let text = instant.to_string_iso();
        assert_eq!(aware(&text), instant);
        assert_eq!(aware(&text).to_string_iso(), text);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib timestamp`
Expected: compile errors (`parse_iso`, `PyDateTime` not found). Add `pub mod timestamp;` to `src/okf/mod.rs` first.

> **Corrected (issue #5), 2026-10-27.** The shipped parser differs from the code below; read `src/okf/timestamp.rs`. It is a byte-level port of CPython's C `parse_hh_mm_ss_ff` / `parse_isoformat_time`, with its quirks (verified fact 3's issue #5 additions): `parse_fields` returns the fields plus a "leftover" flag that is an error only when no offset follows; `parse_offset` accepts `±HH[:MM[:SS[.f]]]` checked only as a whole (< 24 h) and rejects a fraction on a non-zero offset; `RawDate` keeps the date unvalidated until `24:00` has wrapped it CPython's way (`next_day_unchecked`). The plan's code rejected inputs Python accepts (`+01:60`, `+05:30:15`, `10:00:00x+05:00`, `2026-10-00T24:00`). `to_string_iso` also writes `:SS` for whole-second offsets. The public API is unchanged: `PyDateTime::{Aware, Naive}`, `PyDateTime::{isoformat, aware}`, `IsoFormat::to_string_iso`, `parse_iso(&str) -> Option<PyDateTime>`, `is_iso_timestamp`, so Tasks 8, 10 and 12 need no change. Tests added beyond Step 1: `follows_cpython_quirks`, `rejects_the_documented_divergences`, `aware_drops_naive_values`, `parse_iso_never_panics` (Hegel); `isoformat_round_trips` draws instants over years 1..=9999 and offsets to the second. A one-off differential run of 600,000 generated strings against Python 3.14.4 differed only on week dates and fractional-second offsets.

- [ ] **Step 3: Implement**

Put above the test module:

```rust
//! Timestamps as `okf.py` reads them: `parse_ts` is
//! `datetime.fromisoformat(s.replace("Z", "+00:00"))`, and `isoformat()`
//! writes them back.
//!
//! # Known divergences from `okf.py`
//!
//! * ISO week dates (`2026-W43-1`) and offsets with a fraction of a second
//!   (`+05:30:15.5`) are rejected; Python accepts them. Neither occurs in a
//!   catalog, and `lint`'s `ISO_RE` rejects both before parsing anyway.
//!   (Whole-second offsets such as `+05:30:15` are accepted, as in Python.)
//! * Python returns a *naive* datetime when there is no offset, and `okf.py`
//!   then crashes comparing it with an aware one. Callers here treat
//!   [`PyDateTime::Naive`] as "not a usable timestamp" instead.

use std::sync::LazyLock;

use chrono::{DateTime, Days, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use regex::Regex;

/// The result of Python's `datetime.fromisoformat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyDateTime {
    /// With a UTC offset (`Z` or `+HH:MM`): comparable with "now".
    Aware(DateTime<FixedOffset>),
    /// Without one (`2026-10-26`, `2026-10-26T10:00`): not comparable.
    Naive(NaiveDateTime),
}

impl PyDateTime {
    /// Python's `isoformat()`: `YYYY-MM-DDTHH:MM:SS`, then `.ffffff` when the
    /// microseconds are not zero, then the offset as `+HH:MM` for aware values.
    pub fn isoformat(&self) -> String {
        match self {
            PyDateTime::Aware(t) => t.to_string_iso(),
            PyDateTime::Naive(t) => naive_isoformat(t),
        }
    }

    /// The aware value, or `None` for a naive one.
    pub fn aware(self) -> Option<DateTime<FixedOffset>> {
        match self {
            PyDateTime::Aware(t) => Some(t),
            PyDateTime::Naive(_) => None,
        }
    }
}

/// Python `isoformat()` for an aware datetime.
///
/// A trait adds a method to chrono's type, which Rust only allows through a
/// trait defined in this crate (the "orphan rule").
pub trait IsoFormat {
    /// The `isoformat()` text, e.g. `2026-10-26T10:00:00.500000+05:30`.
    fn to_string_iso(&self) -> String;
}

impl IsoFormat for DateTime<FixedOffset> {
    fn to_string_iso(&self) -> String {
        let offset = self.offset().local_minus_utc();
        let sign = if offset < 0 { '-' } else { '+' };
        let minutes = offset.unsigned_abs() / 60;
        format!(
            "{}{sign}{:02}:{:02}",
            naive_isoformat(&self.naive_local()),
            minutes / 60,
            minutes % 60
        )
    }
}

fn naive_isoformat(t: &NaiveDateTime) -> String {
    // `Timelike` is the chrono trait that provides `nanosecond()`.
    let micros = t.nanosecond() / 1000;
    if micros == 0 {
        t.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        format!("{}.{micros:06}", t.format("%Y-%m-%dT%H:%M:%S"))
    }
}

/// Does `text` match `okf.py`'s `ISO_RE`
/// (`^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$`)?
///
/// `\d` is Unicode in both engines; Python's `$` also matches before a
/// final `\n`, spelled out here as `\n?\z`.
pub fn is_iso_timestamp(text: &str) -> bool {
    static ISO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\n?\z")
            .expect("ISO regex is valid")
    });
    ISO.is_match(text)
}

/// `okf.py`'s `parse_ts`: `datetime.fromisoformat(text.replace("Z", "+00:00"))`.
///
/// # Returns
/// `None` where Python raises `ValueError`.
pub fn parse_iso(text: &str) -> Option<PyDateTime> {
    let text = text.replace('Z', "+00:00");
    let (date, rest) = parse_date(&text)?;
    if rest.is_empty() {
        return Some(PyDateTime::Naive(date.and_hms_opt(0, 0, 0)?));
    }
    // Python 3.11+ accepts any single character as the separator.
    let mut chars = rest.chars();
    chars.next()?;
    let (time, extra_day, offset) = parse_time(chars.as_str())?;
    let mut naive = NaiveDateTime::new(date, time);
    if extra_day {
        naive = naive.checked_add_days(Days::new(1))?;
    }
    Some(match offset {
        None => PyDateTime::Naive(naive),
        // `-` on a NaiveDateTime and a TimeDelta: local time minus offset is UTC.
        Some(offset) => PyDateTime::Aware(DateTime::from_naive_utc_and_offset(
            naive - chrono::TimeDelta::seconds(i64::from(offset.local_minus_utc())),
            offset,
        )),
    })
}

/// Parses `N` ASCII digits at the start of `text`.
fn digits(text: &str, n: usize) -> Option<(u32, &str)> {
    let head = text.get(..n)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head.parse().ok()?, &text[n..]))
}

/// `YYYY-MM-DD` or `YYYYMMDD`; Python's year range is 1..=9999.
fn parse_date(text: &str) -> Option<(NaiveDate, &str)> {
    let (year, rest) = digits(text, 4)?;
    let (extended, rest) = match rest.strip_prefix('-') {
        Some(after) => (true, after),
        None => (false, rest),
    };
    let (month, rest) = digits(rest, 2)?;
    let rest = if extended { rest.strip_prefix('-')? } else { rest };
    let (day, rest) = digits(rest, 2)?;
    if year == 0 {
        return None;
    }
    // `i32::try_from` cannot fail for a 4-digit number; `ok()?` keeps it panic-free.
    Some((NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?, rest))
}

/// `HH[:MM[:SS[(.|,)f+]]]` or `HH[MM[SS[(.|,)f+]]]`, then an optional offset.
/// Returns the time, whether it was `24:00` (next midnight), and the offset.
fn parse_time(text: &str) -> Option<(NaiveTime, bool, Option<FixedOffset>)> {
    let split = text.find(['+', '-']).unwrap_or(text.len());
    let (clock, tz) = text.split_at(split);
    let (hour, mut rest) = digits(clock, 2)?;
    let extended = rest.starts_with(':');
    let field = |rest: &mut &str| -> Option<Option<u32>> {
        if rest.is_empty() {
            return Some(None);
        }
        let after = if extended { rest.strip_prefix(':')? } else { rest };
        let (value, tail) = digits(after, 2)?;
        *rest = tail;
        Some(Some(value))
    };
    let minute = field(&mut rest)?.unwrap_or(0);
    let second = field(&mut rest)?;
    let micros = match (second, rest.chars().next()) {
        (_, None) => 0,
        (Some(_), Some('.' | ',')) => {
            let fraction = &rest[1..];
            if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Python keeps 6 digits and drops the rest (no rounding).
            let kept = &fraction[..fraction.len().min(6)];
            kept.parse::<u32>().ok()? * 10u32.pow(6 - kept.len() as u32)
        }
        _ => return None,
    };
    let second = second.unwrap_or(0);
    let (time, extra_day) = if hour == 24 {
        if minute != 0 || second != 0 || micros != 0 {
            return None;
        }
        (NaiveTime::from_hms_micro_opt(0, 0, 0, 0)?, true)
    } else {
        // `from_hms_micro_opt` rejects second 60 (chrono models leap seconds
        // only as microseconds >= 1_000_000, which `micros` never reaches).
        (NaiveTime::from_hms_micro_opt(hour, minute, second, micros)?, false)
    };
    Some((time, extra_day, parse_offset(tz)?))
}

/// `""` (none), `±HH`, `±HH:MM` or `±HHMM`; strictly inside ±24 hours.
fn parse_offset(text: &str) -> Option<Option<FixedOffset>> {
    let Some(sign) = text.chars().next() else {
        return Some(None);
    };
    let (hours, rest) = digits(&text[1..], 2)?;
    let minutes = match rest {
        "" => 0,
        _ => {
            let after = rest.strip_prefix(':').unwrap_or(rest);
            let (minutes, tail) = digits(after, 2)?;
            if !tail.is_empty() || minutes > 59 {
                return None;
            }
            minutes
        }
    };
    if hours > 23 {
        return None;
    }
    let seconds = i32::try_from(hours * 3600 + minutes * 60).ok()?;
    let seconds = if sign == '-' { -seconds } else { seconds };
    Some(Some(FixedOffset::east_opt(seconds)?))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib timestamp`
Expected: PASS. For any disagreement, ask Python (`python3 -c 'from datetime import datetime; print(datetime.fromisoformat("..."))'`) and fix the code to match it.

- [ ] **Step 5: Gates, changelog, commit**

```bash
git add src/okf/timestamp.rs src/okf/mod.rs changelog/
git commit -m "timestamp: port okf.py parse_ts (fromisoformat subset), isoformat and ISO_RE"
```

---

### Task 4: `yaml::scalar` takes a `ScalarStyle` (M1 follow-up)

`scalar(text, flow, force)` has two positional bools, and `scalar(x, false, true)` doesn't say what it means. The M1 final review asked for an enum.

**Files:**
- Modify: `src/okf/yaml.rs` (`scalar`, its callers in `flow_value` and `block_item`, doc example, tests)
- Modify: `src/okf/doc.rs` (test helpers that call `scalar`)
- Create: `changelog/<ts>-m2-task-04-scalar-style.md`

- [ ] **Step 1: Write the failing test**

Add to `src/okf/yaml.rs`'s test module:

```rust
    #[test]
    fn scalar_styles() {
        assert_eq!(scalar("a,b", ScalarStyle::Block), "a,b");
        assert_eq!(scalar("a,b", ScalarStyle::Flow), "\"a,b\"");
        assert_eq!(scalar("plain", ScalarStyle::Quoted), "\"plain\"");
        assert_eq!(scalar("a: b", ScalarStyle::Block), "\"a: b\"");
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib yaml::tests::scalar_styles`
Expected: compile error, `ScalarStyle` not found.

- [ ] **Step 3: Implement**

Replace `scalar` in `src/okf/yaml.rs`:

```rust
/// Where a scalar is written, which decides whether it needs quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarStyle {
    /// A block value (`key: value`, `- item`): quoted only when needed.
    Block,
    /// Inside a flow collection (`[a, b]`, `{ k: v }`), where `,[]{}` are
    /// syntax: quoted when needed there.
    Flow,
    /// Always double-quoted.
    Quoted,
}

/// Renders a scalar in the given style.
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::{scalar, ScalarStyle};
/// assert_eq!(scalar("plain text", ScalarStyle::Block), "plain text");
/// assert_eq!(scalar("a: b", ScalarStyle::Block), "\"a: b\"");
/// assert_eq!(scalar("x", ScalarStyle::Quoted), "\"x\"");
/// ```
pub fn scalar(text: &str, style: ScalarStyle) -> String {
    let quote = match style {
        ScalarStyle::Block => needs_quote(text, false),
        ScalarStyle::Flow => needs_quote(text, true),
        ScalarStyle::Quoted => true,
    };
    if quote {
        double_quote(text)
    } else {
        text.to_string()
    }
}
```

Update the call sites: in `flow_value`, `scalar(k, true, false)` and `scalar(s, true, false)` become `scalar(k, ScalarStyle::Flow)` and `scalar(s, ScalarStyle::Flow)`; in `block_item`, `scalar(s, false, false)` and `scalar(k, false, false)` become `ScalarStyle::Block`. Find the rest with `grep -rn "scalar(" src tests`: the `src/okf/doc.rs` test helpers become `scalar(text, ScalarStyle::Quoted)` (was `false, true`) and `scalar(v, ScalarStyle::Block)` (was `false, false`), with `use crate::okf::yaml::ScalarStyle;` in that test module. The yaml test `scalar_round_trips_through_parse_inline` becomes `scalar(&text, ScalarStyle::Block)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: PASS, including the doc test and the golden render test (rendering is unchanged).

- [ ] **Step 5: Gates, changelog, commit**

```bash
git add src/okf/yaml.rs src/okf/doc.rs changelog/
git commit -m "yaml: scalar takes a ScalarStyle instead of two bools"
```

---

### Task 5: `okf::analysis` and the secret scan

Port of `SECRET_RULES`, `ASSIGN_RE`, `PLACEHOLDER_RE`, `_literal_secret`, `_mask` and `scan_secrets` (`okf.py` 1734-1791). M4 calls `scan_secrets` on every write; M2's snapshot calls it on every file.

**Never write a string that matches a secret rule into a source file**, a test included: GitHub push protection and the catalog's own lint would flag it. Tests build such strings at runtime (`format!`, `repeat`).

**Files:**
- Modify: `Cargo.toml` (add `serde`)
- Create: `src/okf/analysis/mod.rs`, `src/okf/analysis/secrets.rs`
- Modify: `src/okf/mod.rs` (`pub mod analysis;`)
- Create: `changelog/<ts>-m2-task-05-secrets.md`

- [ ] **Step 1: Add `serde`**

In `Cargo.toml` `[dependencies]`:

```toml
# `Serialize` derives for the analysis reports, whose JSON must match okf.py's.
serde = { version = "1.0.229", features = ["derive"] }
```

- [ ] **Step 2: Create `src/okf/analysis/mod.rs`**

```rust
//! Catalog analyses: ports of `okf.py`'s `lint`, `links`, `oldest`, `sprawl`
//! and secret scan. Each is a pure function over a
//! [`Snapshot`](crate::okf::snapshot::Snapshot) (and an injected "now"),
//! returning report structs whose JSON (`serde_json::to_value`) equals
//! `okf.py`'s `--json` output, keys in the same order.
//!
//! Report structs are plain records with public fields, the documented
//! exception to the private-fields rule (like `yaml::YamlError`).

pub mod secrets;

/// One lint-style finding: `{"file", "line", "rule", "message"}` in `okf.py`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Catalog-relative path, without a leading `/`.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// Rule id, e.g. `broken-link` or `secret:github-token`.
    pub rule: String,
    /// Human-readable explanation, exactly as `okf.py` words it.
    pub message: String,
}
```

- [ ] **Step 3: Write the failing tests**

Create `src/okf/analysis/secrets.rs` with the test module (the implementation goes above it in Step 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds rule-matching strings at runtime so no source file holds one.
    fn fake(prefix: &str, body: &str, n: usize) -> String {
        format!("{prefix}{}", body.repeat(n))
    }

    fn rules(text: &str) -> Vec<String> {
        scan_secrets("x.md", text).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn token_rules_fire_with_lookbehind_and_lookahead() {
        let github = fake("gh", "p_", 1) + &"A1".repeat(18);
        assert_eq!(rules(&format!("token {github}")), ["secret:github-token"]);
        assert!(rules(&format!("x{github}")).is_empty(), "preceded by a token character");
        let aws = fake("AK", "IA", 1) + &"Z".repeat(16);
        // `id` is not an ASSIGN_RE keyword, so only the token rule fires.
        assert_eq!(rules(&format!("id={aws}.")), ["secret:aws-key-id"]);
        assert!(rules(&format!("{aws}Q")).is_empty(), "followed by an id character");
        let pem = format!("-----BEGIN {} PRIVATE KEY-----", "RSA");
        assert_eq!(rules(&pem), ["secret:private-key"]);
    }

    #[test]
    fn messages_mask_the_value() {
        let slack = fake("xo", "xb-", 1) + &"1234567890".repeat(2);
        let finding = &scan_secrets("a/b.md", &format!("\n{slack}"))[0];
        assert_eq!((finding.file.as_str(), finding.line), ("a/b.md", 2));
        assert_eq!(
            finding.message,
            "possible secret xoxb...: Slack token prefix xoxb-/xoxp-/xoxa- followed by 10+ \
             token characters"
        );
    }

    #[test]
    fn assignments_need_a_literal_looking_value() {
        let value = fake("Ab", "c1", 4);
        let hits = scan_secrets("x.md", &format!("password: \"{value}\""));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].message,
            "possible secret: 'password' followed by a literal value Abc1... (value mixes \
             letters and digits)"
        );
        for prose in [
            "password: changeme",
            "token: ${TOKEN_FROM_ENV}",
            "secret: /var/run/secrets/x",
            "api_key: lookup(name)",
            "my-token: abcdefgh1",
            "token: short1",
        ] {
            assert!(scan_secrets("x.md", prose).is_empty(), "{prose}");
        }
        assert_eq!(rules(&format!("Client_Secret={}", fake("", "aB", 6))), ["secret:assignment"]);
        assert_eq!(rules(&format!("API-KEY = {}", "q".repeat(20))), ["secret:assignment"]);
    }

    #[test]
    fn assignment_keywords_ignore_case_like_python() {
        // Python's re.I lets `i` match U+0131 (dotless i); the port must too.
        let value = fake("Ab", "c1", 4);
        assert_eq!(rules(&format!("ap\u{131}_key: {value}")), ["secret:assignment"]);
    }

    #[test]
    fn several_assignments_on_one_line_are_all_reported() {
        let value = fake("Ab", "c1", 4);
        let line = format!("token='{value}' secret=\"{value}\"");
        assert_eq!(rules(&line), ["secret:assignment", "secret:assignment"]);
    }

    #[test]
    fn mask_keeps_four_characters() {
        assert_eq!(mask("abcdef"), "abcd...");
        assert_eq!(mask("abcd"), "...");
        assert_eq!(mask("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"), "\u{e9}\u{e9}\u{e9}\u{e9}...");
    }
}
```

- [ ] **Step 4: Run them to see them fail**

Run: `cargo test --lib secrets`
Expected: compile errors (`scan_secrets`, `mask` not found).

- [ ] **Step 5: Implement**

Above the tests in `src/okf/analysis/secrets.rs`:

```rust
//! The secret scan: a port of `okf.py`'s `SECRET_RULES`, `ASSIGN_RE`,
//! `_literal_secret` and `scan_secrets`.
//!
//! Python's lookbehinds (`(?<![A-Za-z0-9_])`) and lookahead are emulated
//! with a consumed neighbour character and a capture group (see verified
//! fact 7 in the M2 plan), so every pattern stays linear-time. A `RegexSet`
//! over all patterns rejects the common line (no hit) in one pass.

use std::sync::LazyLock;

use regex::{Regex, RegexSet};

use crate::okf::analysis::Finding;

/// The catalog's own copy of `okf.py`, which `okf.py` skips (as itself)
/// because its rule table would match its own source.
pub const OKF_PY_PATH: &str = "tools/okf.py";

/// One token rule: id, pattern (group 1 is the token), explanation.
struct TokenRule {
    id: &'static str,
    pattern: Regex,
    why: &'static str,
}

/// `(id, pattern, why)`; group 1 of each pattern is Python's `m.group(0)`.
const TOKEN_RULES: [(&str, &str, &str); 6] = [
    (
        "private-key",
        r"(-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----)",
        "PEM private-key header",
    ),
    (
        "github-token",
        r"(?:\A|[^A-Za-z0-9_])(gh[pousr]_[A-Za-z0-9]{36,})",
        "GitHub token prefix (ghp_/gho_/ghu_/ghs_/ghr_) followed by 36+ token characters",
    ),
    (
        "github-pat",
        r"(?:\A|[^A-Za-z0-9_])(github_pat_[A-Za-z0-9_]{22,})",
        "fine-grained GitHub PAT prefix github_pat_ followed by 22+ token characters",
    ),
    (
        "slack-token",
        r"(?:\A|[^A-Za-z0-9_-])(xox[bpa]-[A-Za-z0-9-]{10,})",
        "Slack token prefix xoxb-/xoxp-/xoxa- followed by 10+ token characters",
    ),
    (
        "sk-key",
        r"(?:\A|[^A-Za-z0-9_-])(sk-(?:[A-Za-z0-9]+-)*[A-Za-z0-9_]{20,})",
        "sk- API-key prefix (OpenAI/Anthropic/Stripe style) followed by 20+ key characters",
    ),
    (
        "aws-key-id",
        r"(?:\A|[^A-Z0-9])((?:AKIA|ASIA)[A-Z0-9]{16})(?:[^A-Z0-9]|\z)",
        "AWS access key id (AKIA/ASIA + 16 uppercase alphanumerics)",
    ),
];

/// `ASSIGN_RE`: group 1 is the keyword, group 2 the value. `(?<![\w-])`
/// becomes a consumed `(?:\A|[^\w-])`; Python's trailing `\2?` (an optional
/// closing quote) is dropped, which changes no captured value (fact 7).
/// Inside `(?i:...)`, `i` is spelled `[iI\u{130}\u{131}]` (fact 6).
const ASSIGN: &str = concat!(
    r#"(?:\A|[^\p{L}\p{N}_\-])"#,
    r#"(?i:(password|passwd|passphrase|secret|cl[iI\u{130}\u{131}]ent[_-]?secret|token|"#,
    r#"ap[iI\u{130}\u{131}][_-]?key|access[_-]?key))"#,
    r#"["']?[ \t]*[:=][ \t]*["']?([^\s\x1c-\x1f"'`,;<>]+)"#,
);

struct Rules {
    tokens: Vec<TokenRule>,
    assign: Regex,
    /// Every token pattern, then `ASSIGN`, in that order.
    any: RegexSet,
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let tokens = TOKEN_RULES
        .iter()
        .map(|(id, pattern, why)| TokenRule {
            id,
            pattern: Regex::new(pattern).expect("token rule regex is valid"),
            why,
        })
        .collect();
    let any = RegexSet::new(TOKEN_RULES.iter().map(|(_, p, _)| *p).chain([ASSIGN]))
        .expect("secret rule set is valid");
    Rules {
        tokens,
        assign: Regex::new(ASSIGN).expect("ASSIGN regex is valid"),
        any,
    }
});

/// `PLACEHOLDER_RE`: values that are clearly not real credentials.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\A(?:x+|\*+|\.+|changeme|redacted|example[\p{L}\p{N}_]*|placeholder|none|null|",
        r"true|false|todo|tbd|your[_-].*|my[_-].*|<.*>|\$.*|\{\{.*|%.*|env|",
        r"secret[\p{L}\p{N}_]*|password[\p{L}\p{N}_]*|token[\p{L}\p{N}_]*)\z",
    ))
    .expect("PLACEHOLDER regex is valid")
});
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d").expect("DIGIT regex is valid"));
static CALL_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[\p{L}\p{N}_]+\(").expect("CALL_LIKE regex is valid"));

/// `_literal_secret`: why an assigned value looks like a real credential,
/// or `None` for prose, references and placeholders.
fn literal_secret(value: &str) -> Option<&'static str> {
    let length = value.chars().count();
    if length < 8 || PLACEHOLDER.is_match(value) {
        return None;
    }
    let reference = ["/", "~", "http:", "https:", "file:"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    if reference || CALL_LIKE.is_match(value) {
        return None;
    }
    // Python's `\d` is any Unicode decimal digit (Nd), as is the `regex`
    // crate's; the letter checks are ASCII in both.
    let has_digit = DIGIT.is_match(value);
    let has_alpha = value.bytes().any(|b| b.is_ascii_alphabetic());
    let mixed = value.bytes().any(|b| b.is_ascii_lowercase())
        && value.bytes().any(|b| b.is_ascii_uppercase());
    if has_digit && has_alpha {
        return Some("value mixes letters and digits");
    }
    if mixed && length >= 12 {
        return Some("value is 12+ mixed-case characters");
    }
    if length >= 20 && !value.contains(['-', '_', '.']) {
        return Some("value is a 20+ character unbroken token");
    }
    None
}

/// `_mask`: the first four characters and `...`, or just `...`.
fn mask(value: &str) -> String {
    match value.char_indices().nth(4) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => "...".to_string(),
    }
}

/// Scans one file's text (read with universal newlines) for likely secrets.
///
/// # Arguments
/// * `path` - catalog-relative path, reported in each finding.
/// * `text` - the file's text.
///
/// # Returns
/// One [`Finding`] per hit, line by line: token rules in table order, then
/// assignments left to right, as `okf.py` reports them.
pub fn scan_secrets(path: &str, text: &str) -> Vec<Finding> {
    let rules = &*RULES;
    let mut hits = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        let candidates = rules.any.matches(line);
        if !candidates.matched_any() {
            continue;
        }
        let number = index + 1;
        for (rule_index, rule) in rules.tokens.iter().enumerate() {
            if !candidates.matched(rule_index) {
                continue;
            }
            if let Some(token) = rule.pattern.captures(line).and_then(|c| c.get(1)) {
                hits.push(Finding {
                    file: path.to_string(),
                    line: number,
                    rule: format!("secret:{}", rule.id),
                    message: format!("possible secret {}: {}", mask(token.as_str()), rule.why),
                });
            }
        }
        if candidates.matched(rules.tokens.len()) {
            hits.extend(assignments(path, number, line));
        }
    }
    hits
}

/// `ASSIGN_RE.finditer(line)` filtered by [`literal_secret`].
fn assignments(path: &str, number: usize, line: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut pos = 0;
    // `captures_at` still sees the text before `pos`, so `\A` only matches
    // at the true line start; resuming at the value's end lets the next
    // match consume the character after it as its "lookbehind" character.
    while let Some(caps) = RULES.assign.captures_at(line, pos) {
        let (Some(keyword), Some(value)) = (caps.get(1), caps.get(2)) else {
            break; // unreachable: both groups are mandatory
        };
        if let Some(why) = literal_secret(value.as_str()) {
            found.push(Finding {
                file: path.to_string(),
                line: number,
                rule: "secret:assignment".to_string(),
                message: format!(
                    "possible secret: '{}' followed by a literal value {} ({why})",
                    keyword.as_str(),
                    mask(value.as_str())
                ),
            });
        }
        pos = value.end();
    }
    found
}
```

Add `pub mod analysis;` to `src/okf/mod.rs`.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib secrets`
Expected: PASS. For a disagreement, run `okf.py`'s `scan_secrets` on the same (runtime-built) text and match it.

- [ ] **Step 7: Gates, changelog, commit**

```bash
git add Cargo.toml Cargo.lock src/okf/mod.rs src/okf/analysis/ changelog/
git commit -m "analysis: Finding and the secret scan (port of okf.py SECRET_RULES/ASSIGN_RE)"
```

---

### Task 6: `okf::snapshot`

The snapshot is the design's "In-memory snapshot": every markdown document parsed once, links resolved, anchors computed, every file scanned for secrets. It ports `build_docs` (`okf.py` 961) and `_fm_fallback` (948), and replaces `okf.py`'s repeated file reads (M1 note: "read each file once, then apply `universal_newlines` for the analysis view").

**Files:**
- Modify: `Cargo.toml` (add `rayon`)
- Create: `src/okf/snapshot.rs`
- Modify: `src/okf/mod.rs` (`pub mod snapshot;`)
- Create: `changelog/<ts>-m2-task-06-snapshot.md`

- [ ] **Step 1: Add `rayon`**

```toml
# Data parallelism for building snapshots and running analyses across documents.
rayon = "1.12.0"
```

- [ ] **Step 2: Write the failing tests**

Create `src/okf/snapshot.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn catalog() -> MemWorkspace {
        MemWorkspace::from_files([
            ("index.md", "---\nokf_version: \"0.2\"\n---\n* [Infra](infra/)\n"),
            ("AGENTS.md", "# Agents\n[x](/nowhere.md)\n"),
            ("infra/index.md", "* [A](/infra/a.md)\n"),
            (
                "infra/a.md",
                "---\ntype: Host\ntitle: Alpha\naliases: [al, alpha-one]\n---\n# Ports\r\n\
                 See [b](b.md#Ports), [self](#ports), [out](https://x.y) and [d](/infra/).\n",
            ),
            ("infra/b.md", "---\ntitle: [unclosed\n---\n## Ports\n"),
            (".claude/notes.md", "# Hidden\n"),
            ("tools/run.sh", "echo hi\n"),
        ])
    }

    #[test]
    fn documents_are_listed_in_walk_order_with_kinds() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let kinds: Vec<(&str, DocKind)> =
            snapshot.docs().iter().map(|d| (d.path(), d.kind())).collect();
        assert_eq!(
            kinds,
            [
                ("AGENTS.md", DocKind::Machinery),
                ("index.md", DocKind::Index),
                ("infra/a.md", DocKind::Concept),
                ("infra/b.md", DocKind::Concept),
                ("infra/index.md", DocKind::Index),
            ]
        );
        assert!(!snapshot.get("AGENTS.md").unwrap().in_link_graph());
        assert!(snapshot.get("infra/a.md").unwrap().in_link_graph());
        assert_eq!(snapshot.concepts().count(), 2);
    }

    #[test]
    fn text_is_universal_newlines_and_raw_is_kept() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let a = snapshot.get("infra/a.md").unwrap();
        assert!(a.text().contains("# Ports\n") && !a.text().contains('\r'));
        assert!(a.raw().contains("# Ports\r\n"));
    }

    #[test]
    fn titles_and_aliases_come_from_frontmatter_or_the_fallback() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let a = snapshot.get("infra/a.md").unwrap();
        assert_eq!(a.title(), Some("Alpha"));
        assert_eq!(a.aliases(), ["al", "alpha-one"]);
        let b = snapshot.get("infra/b.md").unwrap();
        assert!(matches!(b.frontmatter(), Frontmatter::Invalid(_)));
        // okf.py's _fm_fallback: `^title:\s*(.+?)\s*$`, stripped of quotes.
        assert_eq!(b.title(), Some("[unclosed"));
    }

    #[test]
    fn links_are_resolved_with_existence() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let links = snapshot.get("infra/a.md").unwrap().links();
        let summary: Vec<(usize, &str, &str, bool, Option<&str>, bool)> = links
            .iter()
            .map(|l| {
                (l.line, l.raw.as_str(), l.target.as_str(), l.absolute, l.fragment.as_deref(), l.target_exists)
            })
            .collect();
        assert_eq!(
            summary,
            [
                (7, "b.md#Ports", "infra/b.md", false, Some("Ports"), true),
                (7, "#ports", "infra/a.md", true, Some("ports"), true),
                (7, "/infra/", "infra/index.md", true, None, true),
            ]
        );
        assert!(links[1].in_page && !links[0].in_page);
    }

    #[test]
    fn anchors_are_lowercased_slugs_and_html_anchors() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        assert!(snapshot.anchors_of("infra/b.md").unwrap().contains("ports"));
        assert!(snapshot.anchors_of("missing.md").is_none());
    }

    #[test]
    fn backlinks_list_every_document_linking_in() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let sources: Vec<&str> = snapshot
            .backlinks("infra/index.md")
            .iter()
            .map(|b| b.source.as_str())
            .collect();
        assert_eq!(sources, ["index.md", "infra/a.md"]);
        // AGENTS.md is outside the link graph, so its links are not backlinks.
        assert!(snapshot.backlinks("nowhere.md").is_empty());
    }

    #[test]
    fn resolve_concept_accepts_okf_py_spellings() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        for reference in ["/infra/a.md", "infra/a", "/infra/a", "infra/a.md"] {
            assert_eq!(snapshot.resolve_concept(reference).unwrap(), "infra/a.md");
        }
        assert!(matches!(snapshot.resolve_concept("index"), Err(OkfError::InvalidPath { .. })));
        assert!(matches!(snapshot.resolve_concept("nope"), Err(OkfError::NotFound { .. })));
        assert!(matches!(snapshot.resolve_concept("../x"), Err(OkfError::NotFound { .. })));
    }

    #[test]
    fn secrets_are_scanned_in_every_file_but_okf_py() {
        let value = format!("Ab{}", "c1".repeat(4));
        let (password, token) = (format!("password: {value}"), format!("token: {value}"));
        let workspace = MemWorkspace::from_files([
            ("notes.txt", password.as_str()),
            ("tools/okf.py", password.as_str()),
            (".claude/x.md", token.as_str()),
        ]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let files: Vec<&str> = snapshot.secret_findings().iter().map(|f| f.file.as_str()).collect();
        assert_eq!(files, ["notes.txt", ".claude/x.md"]);
    }

    #[test]
    fn fallback_lists_follow_okf_py() {
        assert_eq!(fm_fallback_list("aliases: [a, 'b', , \"c\"]", "aliases"), ["a", "b", "c"]);
        assert_eq!(fm_fallback_list("aliases:\n  - x\n  - 'y'\nnext: 1", "aliases"), ["x", "y"]);
        assert!(fm_fallback_list("title: t", "aliases").is_empty());
        assert_eq!(fm_fallback_scalar("title:\n  next line", "title").as_deref(), Some("next line"));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib snapshot`
Expected: compile errors. Add `pub mod snapshot;` to `src/okf/mod.rs` first.

- [ ] **Step 4: Implement**

Above the tests:

```rust
//! A catalog read into memory once: every markdown document with its
//! frontmatter, analysis text, links (resolved, with existence), anchors and
//! backlinks, plus the secret scan of every file.
//!
//! The design's "In-memory snapshot". M3 keeps one per catalog behind an
//! `RwLock<Arc<Snapshot>>` and rebuilds it after each fetch or publish; the
//! analyses in [`crate::okf::analysis`] and [`crate::okf::search`] read it
//! and never touch the disk.
//!
//! # Known divergences from `okf.py`
//!
//! * A link whose normalized target escapes the catalog root is broken here
//!   (`target_exists` is false); `okf.py` asks the OS about the outside path.
//! * Files are listed per the M1 listing contract (`DiskWorkspace` skips
//!   dangling and outside symlinks and special files).

use std::collections::{BTreeSet, HashMap};

use indexmap::IndexMap;
use rayon::prelude::*;
use regex::Regex;

use crate::error::OkfError;
use crate::okf::analysis::secrets::{scan_secrets, OKF_PY_PATH};
use crate::okf::analysis::Finding;
use crate::okf::doc::Doc;
use crate::okf::markdown::{anchors_in, find_links, resolve_link, split_frontmatter};
use crate::okf::paths::{basename, escapes_root, is_concept, normpath};
use crate::okf::pystr::{py_strip, universal_newlines};
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{Value, YamlError};

/// What a markdown file is, by name (`okf.py`'s `build_docs` "kind").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// A concept: any `.md` not named in `paths::NON_CONCEPTS`.
    Concept,
    /// An `index.md`.
    Index,
    /// A `log.md`.
    Log,
    /// `AGENTS.md`, `CLAUDE.md` or `README.md`.
    Machinery,
}

impl DocKind {
    fn of(path: &str) -> Self {
        match basename(path) {
            "index.md" => DocKind::Index,
            "log.md" => DocKind::Log,
            _ if is_concept(path) => DocKind::Concept,
            _ => DocKind::Machinery,
        }
    }
}

/// A document's frontmatter, parsed once.
#[derive(Debug, Clone, PartialEq)]
pub enum Frontmatter {
    /// No `---` block.
    Absent,
    /// Parsed, keys in source order.
    Parsed(IndexMap<String, Value>),
    /// Present but does not parse.
    Invalid(YamlError),
}

impl Frontmatter {
    /// The parsed mapping, or `None` if absent or invalid.
    pub fn data(&self) -> Option<&IndexMap<String, Value>> {
        match self {
            Frontmatter::Parsed(data) => Some(data),
            Frontmatter::Absent | Frontmatter::Invalid(_) => None,
        }
    }
}

/// One link in a document (a record, so its fields are public).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLink {
    /// 1-based line in the file (frontmatter lines count, as in `okf.py`).
    pub line: usize,
    /// The target as written, e.g. `../b.md#Ports`.
    pub raw: String,
    /// Catalog-relative target path (this document for an in-page link).
    pub target: String,
    /// Written from the catalog root (`/x.md`), or in-page.
    pub absolute: bool,
    /// The text after the first `#`, if any (may be empty).
    pub fragment: Option<String>,
    /// An in-page link (`#heading`).
    pub in_page: bool,
    /// Does the target exist (file or directory) inside the catalog?
    pub target_exists: bool,
    /// Is the target a regular file?
    pub target_is_file: bool,
}

/// A link into a document from another one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// The linking document's path.
    pub source: String,
    /// The 1-based line of the link in it.
    pub line: usize,
}

/// One markdown document.
#[derive(Debug, Clone)]
pub struct SnapshotDoc {
    path: String,
    kind: DocKind,
    text: String,
    doc: Doc,
    frontmatter: Frontmatter,
    title: Option<String>,
    aliases: Vec<String>,
    links: Vec<SnapshotLink>,
    anchors: BTreeSet<String>,
}

impl SnapshotDoc {
    /// Catalog-relative path, e.g. `infra/time-server.md`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the file is.
    pub fn kind(&self) -> DocKind {
        self.kind
    }

    /// The text for analysis: universal newlines, as `okf.py` reads it.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The file exactly as read (rebuilt from [`Doc`], which keeps every byte).
    pub fn raw(&self) -> String {
        self.doc.render()
    }

    /// The byte-preserving document model (frontmatter blocks, key lines).
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The parsed frontmatter.
    pub fn frontmatter(&self) -> &Frontmatter {
        &self.frontmatter
    }

    /// `title` if it is a string (or, for invalid frontmatter, the regex fallback).
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The string `aliases` (a scalar alias counts as a one-item list).
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    /// Every internal link, in file order (external links are dropped).
    pub fn links(&self) -> &[SnapshotLink] {
        &self.links
    }

    /// Lowercased heading slugs and HTML anchors.
    pub fn anchors(&self) -> &BTreeSet<String> {
        &self.anchors
    }

    /// Does `okf.py`'s link analysis see this file? Everything except the
    /// root `AGENTS.md`, `CLAUDE.md` and `README.md` (`build_docs` skips them).
    pub fn in_link_graph(&self) -> bool {
        !(self.kind == DocKind::Machinery && !self.path.contains('/'))
    }
}

/// A whole catalog in memory. Immutable once built.
#[derive(Debug, Clone)]
pub struct Snapshot {
    docs: Vec<SnapshotDoc>,
    by_path: HashMap<String, usize>,
    /// Anchors of fragment-link targets that are not listed markdown (in a
    /// dot-directory, say), read once at build time.
    extra_anchors: HashMap<String, BTreeSet<String>>,
    backlinks: HashMap<String, Vec<Backlink>>,
    secrets: Vec<Finding>,
}

impl Snapshot {
    /// Reads and analyzes every file of a catalog.
    ///
    /// Documents are read and parsed in parallel (`rayon`), and the results
    /// keep `os.walk` order. Each file is read once.
    ///
    /// # Arguments
    /// * `workspace` - the catalog (`&dyn Workspace` is a trait object: any
    ///   type implementing the trait, like a Python duck-typed argument).
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed or a markdown file
    /// can't be read (non-markdown files that fail to read are skipped, as
    /// `okf.py`'s secret walk skips them).
    pub fn build(workspace: &dyn Workspace) -> Result<Self, OkfError> {
        let paths = workspace.list_markdown()?;
        // `par_iter` splits the work across a thread pool; collecting into a
        // `Result<Vec<_>, _>` keeps the input order and stops at the first error.
        let docs: Vec<SnapshotDoc> = paths
            .par_iter()
            .map(|path| read_document(workspace, path))
            .collect::<Result<_, _>>()?;
        let by_path: HashMap<String, usize> = docs
            .iter()
            .enumerate()
            .map(|(index, doc)| (doc.path.clone(), index))
            .collect();
        let extra_anchors = extra_anchors(workspace, &docs, &by_path);
        let backlinks = backlinks_of(&docs);
        let secrets = scan_all(workspace, &docs, &by_path)?;
        Ok(Self {
            docs,
            by_path,
            extra_anchors,
            backlinks,
            secrets,
        })
    }

    /// Every markdown document, in `os.walk` order.
    pub fn docs(&self) -> &[SnapshotDoc] {
        &self.docs
    }

    /// The document at a catalog-relative path.
    pub fn get(&self, path: &str) -> Option<&SnapshotDoc> {
        self.by_path.get(path).map(|&index| &self.docs[index])
    }

    /// The concept documents, in `os.walk` order (`okf.py`'s `concepts(root)`).
    pub fn concepts(&self) -> impl Iterator<Item = &SnapshotDoc> {
        self.docs.iter().filter(|d| d.kind == DocKind::Concept)
    }

    /// Lowercased anchors of a markdown file, or `None` if it was not read.
    pub fn anchors_of(&self, path: &str) -> Option<&BTreeSet<String>> {
        self.get(path)
            .map(SnapshotDoc::anchors)
            .or_else(|| self.extra_anchors.get(path))
    }

    /// Links into `path` from documents in the link graph (in-page links
    /// excluded), in walk order.
    pub fn backlinks(&self, path: &str) -> &[Backlink] {
        self.backlinks.get(path).map_or(&[], Vec::as_slice)
    }

    /// Secret-scan findings for every file except `tools/okf.py`.
    pub fn secret_findings(&self) -> &[Finding] {
        &self.secrets
    }

    /// Resolves `/dir/x.md`, `dir/x`, `/dir/x` or `dir/x.md` to a concept path,
    /// as [`crate::okf::paths::resolve_concept`] does on a workspace.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] when the file exists but is not a concept;
    /// [`OkfError::NotFound`] otherwise.
    pub fn resolve_concept(&self, reference: &str) -> Result<String, OkfError> {
        let bare = reference.trim_start_matches('/');
        for candidate in [normpath(bare), normpath(&format!("{bare}.md"))] {
            if escapes_root(&candidate) || !candidate.ends_with(".md") {
                continue;
            }
            let Some(doc) = self.get(&candidate) else {
                continue;
            };
            if doc.kind != DocKind::Concept {
                return Err(OkfError::InvalidPath {
                    path: candidate,
                    reason: "is not a concept document".to_string(),
                });
            }
            return Ok(candidate);
        }
        Err(OkfError::NotFound {
            path: reference.to_string(),
        })
    }
}

/// Reads one markdown file and derives everything the analyses need.
fn read_document(workspace: &dyn Workspace, path: &str) -> Result<SnapshotDoc, OkfError> {
    let raw = workspace.read_to_string(path)?;
    // `Doc` keeps its own copy of every line (head, frontmatter and tail), so
    // each document is held twice: as `doc` (byte-exact) and as `text`
    // (universal newlines). That doubles memory per document, which is
    // acceptable at catalog sizes (a few MB); `raw` itself is moved, not copied.
    let doc = Doc::from_text(path, &raw);
    let text = match universal_newlines(&raw) {
        std::borrow::Cow::Owned(normalized) => normalized,
        std::borrow::Cow::Borrowed(_) => raw,
    };
    let frontmatter = if !doc.has_frontmatter() {
        Frontmatter::Absent
    } else {
        match doc.parse() {
            Ok(data) => Frontmatter::Parsed(data),
            Err(error) => Frontmatter::Invalid(error),
        }
    };
    let (title, aliases) = title_and_aliases(&frontmatter, &text);
    let links = links_of(workspace, path, &text);
    let anchors = lowercase_anchors(&text);
    Ok(SnapshotDoc {
        path: path.to_string(),
        kind: DocKind::of(path),
        text,
        doc,
        frontmatter,
        title,
        aliases,
        links,
        anchors,
    })
}

/// `build_docs`' title and aliases: from parsed frontmatter, else the regex
/// fallback over `split_frontmatter`'s frontmatter text.
fn title_and_aliases(frontmatter: &Frontmatter, text: &str) -> (Option<String>, Vec<String>) {
    match frontmatter {
        Frontmatter::Invalid(_) => {
            let (fm, _, _) = split_frontmatter(text);
            (fm_fallback_scalar(fm, "title"), fm_fallback_list(fm, "aliases"))
        }
        Frontmatter::Absent | Frontmatter::Parsed(_) => {
            let data = frontmatter.data();
            let get = |key: &str| data.and_then(|d| d.get(key));
            let title = get("title").and_then(Value::as_str).map(str::to_string);
            // `data.get("aliases") or []`, then a scalar becomes a one-item list.
            let aliases = match get("aliases") {
                Some(Value::Seq(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                Some(Value::Str(alias)) if !alias.is_empty() => vec![alias.clone()],
                Some(Value::Map(_) | Value::Null | Value::Str(_)) | None => Vec::new(),
            };
            (title, aliases)
        }
    }
}

/// Every internal link in `text`, line by line (`build_docs`' link loop).
fn links_of(workspace: &dyn Workspace, path: &str, text: &str) -> Vec<SnapshotLink> {
    let mut links = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        for found in find_links(line) {
            let raw = found.target;
            let fragment = raw.split_once('#').map(|(_, f)| f.to_string());
            if raw.starts_with('#') {
                links.push(SnapshotLink {
                    line: index + 1,
                    target: path.to_string(),
                    absolute: true,
                    fragment,
                    in_page: true,
                    target_exists: true,
                    target_is_file: true,
                    raw,
                });
                continue;
            }
            let Some(resolved) = resolve_link(workspace, &raw, path) else {
                continue; // external (`https:`, `mailto:`) or empty
            };
            let inside = !escapes_root(&resolved.path);
            links.push(SnapshotLink {
                line: index + 1,
                target_exists: inside && workspace.exists(&resolved.path),
                target_is_file: inside && workspace.is_file(&resolved.path),
                target: resolved.path,
                absolute: resolved.absolute,
                fragment,
                in_page: false,
                raw,
            });
        }
    }
    links
}

fn lowercase_anchors(text: &str) -> BTreeSet<String> {
    anchors_in(text).into_iter().map(|a| a.to_lowercase()).collect()
}

/// Anchors for fragment-link targets that are markdown files but not
/// snapshot documents (for example in a dot-directory).
fn extra_anchors(
    workspace: &dyn Workspace,
    docs: &[SnapshotDoc],
    by_path: &HashMap<String, usize>,
) -> HashMap<String, BTreeSet<String>> {
    let mut extra = HashMap::new();
    let wanted = docs.iter().flat_map(|d| &d.links).filter(|l| {
        l.fragment.as_deref().is_some_and(|f| !f.is_empty())
            && l.target.ends_with(".md")
            && l.target_is_file
            && !by_path.contains_key(&l.target)
    });
    for link in wanted {
        if extra.contains_key(&link.target) {
            continue;
        }
        if let Ok(text) = workspace.read_text(&link.target) {
            extra.insert(link.target.clone(), lowercase_anchors(&text));
        }
    }
    extra
}

fn backlinks_of(docs: &[SnapshotDoc]) -> HashMap<String, Vec<Backlink>> {
    let mut backlinks: HashMap<String, Vec<Backlink>> = HashMap::new();
    for doc in docs.iter().filter(|d| d.in_link_graph()) {
        for link in doc.links.iter().filter(|l| !l.in_page && l.target_exists) {
            backlinks.entry(link.target.clone()).or_default().push(Backlink {
                source: doc.path.clone(),
                line: link.line,
            });
        }
    }
    backlinks
}

/// The secret scan over every file, reusing the text of markdown documents
/// already read. Unreadable files (not UTF-8, vanished) are skipped, as
/// `okf.py` skips `UnicodeDecodeError` and `OSError`.
fn scan_all(
    workspace: &dyn Workspace,
    docs: &[SnapshotDoc],
    by_path: &HashMap<String, usize>,
) -> Result<Vec<Finding>, OkfError> {
    let files = workspace.list_files()?;
    let per_file: Vec<Vec<Finding>> = files
        .par_iter()
        .filter(|path| path.as_str() != OKF_PY_PATH)
        .map(|path| match by_path.get(path) {
            Some(&index) => scan_secrets(path, &docs[index].text),
            None => match workspace.read_text(path) {
                Ok(text) => scan_secrets(path, &text),
                Err(error) => {
                    tracing::debug!(%path, %error, "secret scan skips an unreadable file");
                    Vec::new()
                }
            },
        })
        .collect();
    Ok(per_file.into_iter().flatten().collect())
}

/// `_fm_fallback(fm, key, False)`: `^key:\s*(.+?)\s*$` (multi-line), the
/// match stripped of whitespace and then of quote characters.
///
/// Python's `\s*` may cross a newline, so `title:` followed by an indented
/// line yields that line; the port keeps that.
pub(crate) fn fm_fallback_scalar(fm: &str, key: &str) -> Option<String> {
    // Only runs for documents whose frontmatter does not parse, so compiling
    // the pattern per call costs nothing in the normal case.
    let pattern = format!(r"(?m)^{}:[\s\x1c-\x1f]*(.+?)[\s\x1c-\x1f]*$", regex::escape(key));
    let found = Regex::new(&pattern).ok()?.captures(fm)?;
    Some(strip_quotes(py_strip(&found[1])).to_string())
}

/// `_fm_fallback(fm, key, True)`: a flow list on the key line, else a block
/// list below it, each item stripped of whitespace and quote characters.
pub(crate) fn fm_fallback_list(fm: &str, key: &str) -> Vec<String> {
    let key = regex::escape(key);
    let flow = format!(r"(?m)^{key}:[\s\x1c-\x1f]*\[(.*?)\][\s\x1c-\x1f]*$");
    if let Some(found) = Regex::new(&flow).ok().and_then(|r| r.captures(fm)) {
        return found[1]
            .split(',')
            .filter(|item| !py_strip(item).is_empty())
            .map(|item| strip_quotes(py_strip(item)).to_string())
            .collect();
    }
    let block = format!(
        r"(?m)^{key}:[\s\x1c-\x1f]*\n((?:[\s\x1c-\x1f]+-[\s\x1c-\x1f]+.+\n?)+)"
    );
    let Some(found) = Regex::new(&block).ok().and_then(|r| r.captures(fm)) else {
        return Vec::new();
    };
    static ITEM: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^[\s\x1c-\x1f]+-[\s\x1c-\x1f]+(.+)$").expect("ITEM regex is valid")
    });
    ITEM.captures_iter(&found[1])
        .map(|item| strip_quotes(py_strip(&item[1])).to_string())
        .collect()
}

/// Python's `s.strip("\"'")`.
fn strip_quotes(text: &str) -> &str {
    text.trim_matches(['"', '\''])
}
```

> Python's `.+` in `_fm_fallback`'s block pattern does not cross `\n`, and neither does the `regex` crate's `.`; `$` in multi-line mode matches before `\n` in both.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib snapshot`
Expected: PASS. The `links_are_resolved_with_existence` line numbers count the frontmatter lines (`okf.py` scans the whole text).

- [ ] **Step 6: Gates, changelog, commit**

```bash
git add Cargo.toml Cargo.lock src/okf/mod.rs src/okf/snapshot.rs changelog/
git commit -m "snapshot: read a catalog once, with links, anchors, backlinks and secret findings"
```

---

### Task 7: `okf::analysis::links` (`links --json`)

Port of `link_analysis` (`okf.py` 1005-1091), `names_for` (996) and `prose_lines` (933), over a snapshot.

**Files:**
- Create: `src/okf/analysis/links.rs`
- Modify: `src/okf/analysis/mod.rs` (`pub mod links;`)
- Create: `changelog/<ts>-m2-task-07-links.md`

- [ ] **Step 1: Write the failing tests**

Create `src/okf/analysis/links.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn report(files: &[(&str, &str)]) -> LinksReport {
        let workspace = MemWorkspace::from_files(files.iter().copied());
        links_report(&Snapshot::build(&workspace).expect("snapshot"), true)
    }

    const CATALOG: [(&str, &str); 5] = [
        (
            "index.md",
            "---\nokf_version: \"0.2\"\n---\n# Sections\n\n* [Infra](infra/)\n* [Wrong](/infra/a.md)\n",
        ),
        ("infra/index.md", "# Concepts\n\n* [A](/infra/a.md)\n"),
        (
            "infra/a.md",
            "---\ntype: Host\ntitle: Alpha Server\n---\n# Ports\n[b](b.md) [gone](/infra/gone.md) \
             [p](/infra/b.md#Missing%20Part) [q](/infra/b.md#real-part) [self](#ports)\n",
        ),
        (
            "infra/b.md",
            "---\ntype: Host\n---\n# Real Part\nThe Alpha Server runs `alpha server` here.\n",
        ),
        ("AGENTS.md", "[x](/missing.md)\n"),
    ];

    fn issue(file: &str, line: usize, link: &str) -> LinkIssue {
        LinkIssue {
            file: file.to_string(),
            line,
            link: link.to_string(),
        }
    }

    #[test]
    fn link_problems_match_okf_py() {
        let report = report(&CATALOG);
        assert_eq!(report.broken, [issue("infra/a.md", 6, "/infra/gone.md")]);
        assert_eq!(
            report.broken_anchors,
            [issue("infra/a.md", 6, "/infra/b.md#Missing%20Part")]
        );
        assert_eq!(report.relative, [issue("infra/a.md", 6, "b.md")]);
        assert_eq!(report.orphans, ["infra/a.md"]);
        assert_eq!(report.dead_ends, ["infra/b.md"]);
    }

    #[test]
    fn index_drift_lists_unlisted_and_misfiled_concepts() {
        let problems: Vec<(String, String)> = report(&CATALOG)
            .index_drift
            .into_iter()
            .map(|d| (d.file, d.problem))
            .collect();
        assert_eq!(
            problems,
            [
                ("infra/b.md".to_string(), "not listed in infra/index.md".to_string()),
                (
                    "index.md".to_string(),
                    "line 7: concept /infra/a.md is listed under # Sections (which lists \
                     folders); move it to # Concepts"
                        .to_string()
                ),
            ]
        );
    }

    #[test]
    fn unlinked_mentions_skip_code_and_report_the_first_name() {
        let report = report(&CATALOG);
        assert_eq!(report.unlinked_mentions.len(), 1);
        let mention = &report.unlinked_mentions[0];
        assert_eq!((mention.file.as_str(), mention.target.as_str()), ("infra/b.md", "infra/a.md"));
        assert_eq!(
            mention.hits,
            [MentionHit {
                line: 5,
                name: "Alpha Server".to_string(),
                text: "The Alpha Server runs `alpha server` here.".to_string(),
            }]
        );
        assert_eq!(
            report.summary,
            LinksSummary {
                concepts: 2,
                broken: 1,
                broken_anchors: 1,
                relative: 1,
                orphans: 1,
                dead_ends: 1,
                index_drift: 2,
                unlinked_mentions: 1,
            }
        );
    }

    #[test]
    fn mentions_can_be_turned_off() {
        let workspace = MemWorkspace::from_files(CATALOG);
        let report = links_report(&Snapshot::build(&workspace).expect("snapshot"), false);
        assert!(report.unlinked_mentions.is_empty());
        assert_eq!(report.summary.unlinked_mentions, 0);
    }

    #[test]
    fn mention_boundaries_follow_okf_py() {
        let pattern = Regex::new(&mention_pattern("Time Server")).unwrap();
        for hit in ["the time server.", "TIME SERVER", "(time server)", "t\u{131}me server"] {
            assert!(pattern.is_match(hit), "{hit}");
        }
        for miss in ["time servers", "/time server", "x-time server", "time server-x", "time_server"] {
            assert!(!pattern.is_match(miss), "{miss}");
        }
    }

    #[test]
    fn prose_lines_mask_code_links_and_urls() {
        let lines = prose_lines("---\nk: v\n---\na `b` [c](/d.md) https://e.f/g h\n```\ncode\n```\n[^1]: n\n");
        // From okf.py's prose_lines on the same text: the fence and the
        // footnote are skipped, the empty last line stays.
        let numbered: Vec<(usize, &str)> = lines.iter().map(|(n, m, _)| (*n, m.as_str())).collect();
        assert_eq!(numbered, [(4, "a       h"), (9, "")]);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib analysis::links`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! `okf.py links --json`: broken links and anchors, relative links, orphans,
//! dead ends, index drift and unlinked mentions (`link_analysis`).
//!
//! Unlinked mentions use one `RegexSet` holding every (concept, name)
//! pattern, so each prose line is scanned once for all concepts at once,
//! instead of once per concept per name as in `okf.py`. Concepts are
//! scanned in parallel.

use std::collections::HashSet;
use std::sync::LazyLock;

use rayon::prelude::*;
use regex::{Regex, RegexSet, RegexSetBuilder};
use serde::Serialize;

use crate::okf::markdown::{find_links, split_frontmatter};
use crate::okf::paths::{basename, dirname, join, normpath};
use crate::okf::pystr::{ignorecase_literal, py_lstrip, py_strip, py_unquote};
use crate::okf::snapshot::{DocKind, Snapshot, SnapshotDoc};

/// Names shorter than this (in characters) are never matched as mentions.
pub const MIN_NAME_LEN: usize = 4;
/// Mention hit text is cut to this many characters.
const HIT_TEXT_LEN: usize = 160;

/// A link problem: `{"file", "line", "link"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkIssue {
    /// The linking document.
    pub file: String,
    /// 1-based line of the link.
    pub line: usize,
    /// The link target as written.
    pub link: String,
}

/// An index problem: `{"file", "problem"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexDrift {
    /// The unlisted document, or the index with a misfiled bullet.
    pub file: String,
    /// What is wrong, worded as `okf.py` words it.
    pub problem: String,
}

/// One line mentioning a concept by name: `{"line", "name", "text"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MentionHit {
    /// 1-based line.
    pub line: usize,
    /// The name that matched (stem, title or alias).
    pub name: String,
    /// The line, stripped, cut to 160 characters.
    pub text: String,
}

/// A concept mentioning another without linking to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnlinkedMention {
    /// The mentioning concept.
    pub file: String,
    /// The mentioned concept.
    pub target: String,
    /// Every line with a mention, in order.
    pub hits: Vec<MentionHit>,
}

/// Counts: `concepts`, then one per report list, in `okf.py`'s key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinksSummary {
    /// Number of concepts.
    pub concepts: usize,
    /// `broken.len()`.
    pub broken: usize,
    /// `broken_anchors.len()`.
    pub broken_anchors: usize,
    /// `relative.len()`.
    pub relative: usize,
    /// `orphans.len()`.
    pub orphans: usize,
    /// `dead_ends.len()`.
    pub dead_ends: usize,
    /// `index_drift.len()`.
    pub index_drift: usize,
    /// `unlinked_mentions.len()`.
    pub unlinked_mentions: usize,
}

/// The whole report, keys in `okf.py`'s order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinksReport {
    /// Links whose target does not exist.
    pub broken: Vec<LinkIssue>,
    /// Links whose `#fragment` names no heading or anchor in the target.
    pub broken_anchors: Vec<LinkIssue>,
    /// Relative links in concepts (bundle-root absolute paths are preferred).
    pub relative: Vec<LinkIssue>,
    /// Concepts no other concept links to.
    pub orphans: Vec<String>,
    /// Concepts that link to no other concept.
    pub dead_ends: Vec<String>,
    /// Documents missing from their folder's `index.md`, and misfiled bullets.
    pub index_drift: Vec<IndexDrift>,
    /// Name mentions without a link (candidates only).
    pub unlinked_mentions: Vec<UnlinkedMention>,
    /// Counts.
    pub summary: LinksSummary,
}

/// Runs the link analysis.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `mentions` - also look for unlinked mentions (`lint` passes `false`).
///
/// # Returns
/// The report; `okf.py links --json` prints the same JSON.
pub fn links_report(snapshot: &Snapshot, mentions: bool) -> LinksReport {
    let graph: Vec<&SnapshotDoc> = snapshot.docs().iter().filter(|d| d.in_link_graph()).collect();
    let concepts: Vec<&SnapshotDoc> =
        graph.iter().copied().filter(|d| d.kind() == DocKind::Concept).collect();
    let concept_paths: HashSet<&str> = concepts.iter().map(|d| d.path()).collect();

    let mut report = LinksReport::default();
    let mut has_inbound: HashSet<&str> = HashSet::new();
    for doc in &graph {
        let in_concept = doc.kind() == DocKind::Concept;
        for link in doc.links() {
            let issue = || LinkIssue {
                file: doc.path().to_string(),
                line: link.line,
                link: link.raw.clone(),
            };
            if !link.target_exists {
                report.broken.push(issue());
                continue;
            }
            if let Some(fragment) = link.fragment.as_deref().filter(|f| !f.is_empty()) {
                if link.target.ends_with(".md") && link.target_is_file {
                    let wanted = py_unquote(fragment).to_lowercase();
                    let found = snapshot
                        .anchors_of(&link.target)
                        .is_some_and(|anchors| anchors.contains(&wanted));
                    if !found {
                        report.broken_anchors.push(issue());
                    }
                }
            }
            if in_concept && !link.absolute {
                report.relative.push(issue());
            }
            if in_concept && link.target != doc.path() && concept_paths.contains(link.target.as_str())
            {
                has_inbound.insert(link.target.as_str());
            }
        }
    }

    for doc in &concepts {
        if !has_inbound.contains(doc.path()) {
            report.orphans.push(doc.path().to_string());
        }
        let has_outbound = doc
            .links()
            .iter()
            .any(|l| l.target != doc.path() && concept_paths.contains(l.target.as_str()));
        if !has_outbound {
            report.dead_ends.push(doc.path().to_string());
        }
    }

    report.index_drift = index_drift(snapshot, &graph, &concept_paths);
    if mentions {
        report.unlinked_mentions = unlinked_mentions(&concepts);
    }
    report.summary = LinksSummary {
        concepts: concepts.len(),
        broken: report.broken.len(),
        broken_anchors: report.broken_anchors.len(),
        relative: report.relative.len(),
        orphans: report.orphans.len(),
        dead_ends: report.dead_ends.len(),
        index_drift: report.index_drift.len(),
        unlinked_mentions: report.unlinked_mentions.len(),
    };
    report
}

/// `^#+\s` (Python `\s`).
static INDEX_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#+[\s\x1c-\x1f]").expect("INDEX_HEADING regex is valid"));

/// Every concept listed by its folder's `index.md`, every folder index by
/// its parent's; and no concept bullet under an index's `# Sections`.
fn index_drift(
    snapshot: &Snapshot,
    graph: &[&SnapshotDoc],
    concept_paths: &HashSet<&str>,
) -> Vec<IndexDrift> {
    let mut drift = Vec::new();
    for doc in graph {
        let parent = match doc.kind() {
            DocKind::Log | DocKind::Machinery => continue,
            _ if doc.path() == "index.md" => continue,
            DocKind::Index => dirname(dirname(doc.path())),
            DocKind::Concept => dirname(doc.path()),
        };
        let listed_in = normpath(&join(parent, "index.md"));
        let index = snapshot.get(&listed_in).filter(|d| d.in_link_graph());
        let problem = match index {
            None => format!("no {listed_in} to list it"),
            Some(index) if !index.links().iter().any(|l| l.target == doc.path()) => {
                format!("not listed in {listed_in}")
            }
            Some(_) => continue,
        };
        drift.push(IndexDrift {
            file: doc.path().to_string(),
            problem,
        });
    }
    for index in graph.iter().filter(|d| d.kind() == DocKind::Index) {
        let mut heading: Option<String> = None;
        let mut section_bullets = HashSet::new();
        for (number, line) in (1..).zip(index.text().split('\n')) {
            if INDEX_HEADING.is_match(line) {
                heading = Some(py_strip(line.trim_start_matches('#')).to_lowercase());
            } else if heading.as_deref() == Some("sections")
                && (line.starts_with("* ") || line.starts_with("- "))
            {
                section_bullets.insert(number);
            }
        }
        let misfiled = index.links().iter().filter(|l| {
            !l.in_page && section_bullets.contains(&l.line) && concept_paths.contains(l.target.as_str())
        });
        for link in misfiled {
            drift.push(IndexDrift {
                file: index.path().to_string(),
                problem: format!(
                    "line {}: concept /{} is listed under # Sections (which lists folders); \
                     move it to # Concepts",
                    link.line, link.target
                ),
            });
        }
    }
    drift
}

/// `names_for`: the file stem, the title and the aliases, at least
/// [`MIN_NAME_LEN`] characters long, sorted and without duplicates.
fn names_for(doc: &SnapshotDoc) -> Vec<String> {
    let stem = basename(doc.path()).trim_end_matches(".md");
    let mut names: Vec<String> = std::iter::once(stem)
        .chain(doc.title())
        .chain(doc.aliases().iter().map(String::as_str))
        .filter(|name| name.chars().count() >= MIN_NAME_LEN)
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `(?<![\w/-])NAME(?![\w-])` with `re.I`, lookaround emulated by consumed
/// neighbour characters (an existence test only needs that).
fn mention_pattern(name: &str) -> String {
    format!(
        r"(?:\A|[^\p{{L}}\p{{N}}_/\-])(?i:{})(?:[^\p{{L}}\p{{N}}_\-]|\z)",
        ignorecase_literal(name)
    )
}

/// Every concept's names as one `RegexSet`.
struct MentionMatcher<'a> {
    /// `(concept path, its names)`, in concept order.
    targets: Vec<(&'a str, Vec<String>)>,
    /// For each set pattern: `(index into targets, index into its names)`.
    owners: Vec<(usize, usize)>,
    set: RegexSet,
}

impl<'a> MentionMatcher<'a> {
    fn new(concepts: &[&'a SnapshotDoc]) -> Self {
        let targets: Vec<(&str, Vec<String>)> = concepts
            .iter()
            .map(|doc| (doc.path(), names_for(doc)))
            .filter(|(_, names)| !names.is_empty())
            .collect();
        let mut owners = Vec::new();
        let mut patterns = Vec::new();
        for (target, (_, names)) in targets.iter().enumerate() {
            for (name_index, name) in names.iter().enumerate() {
                owners.push((target, name_index));
                patterns.push(mention_pattern(name));
            }
        }
        // Hundreds of patterns with Unicode classes outgrow the default
        // 10 MiB compile limit. 256 MiB still bounds compile memory; Step 5
        // measures the real catalog, and a catalog that exceeds the limit
        // needs the two-stage matcher described there.
        let set = RegexSetBuilder::new(patterns)
            .size_limit(256 << 20)
            .dfa_size_limit(256 << 20)
            .build()
            .expect("escaped mention patterns always compile");
        Self {
            targets,
            owners,
            set,
        }
    }
}

fn unlinked_mentions(concepts: &[&SnapshotDoc]) -> Vec<UnlinkedMention> {
    let matcher = MentionMatcher::new(concepts);
    // One task per mentioning concept; `collect` keeps concept order.
    let per_source: Vec<Vec<UnlinkedMention>> = concepts
        .par_iter()
        .map(|source| mentions_in(source, &matcher))
        .collect();
    per_source.into_iter().flatten().collect()
}

fn mentions_in(source: &SnapshotDoc, matcher: &MentionMatcher<'_>) -> Vec<UnlinkedMention> {
    let linked: HashSet<&str> = source.links().iter().map(|l| l.target.as_str()).collect();
    let mut hits: Vec<Vec<MentionHit>> = vec![Vec::new(); matcher.targets.len()];
    for (line, masked, original) in prose_lines(source.text()) {
        let matched = matcher.set.matches(&masked);
        let mut previous_target = None;
        // Ascending pattern order: a target's names are contiguous and
        // sorted, so the first match per target is `okf.py`'s first name.
        for pattern in matched.iter() {
            let (target, name) = matcher.owners[pattern];
            if previous_target == Some(target) {
                continue;
            }
            previous_target = Some(target);
            let (target_path, names) = &matcher.targets[target];
            if *target_path == source.path() || linked.contains(target_path) {
                continue;
            }
            hits[target].push(MentionHit {
                line,
                name: names[name].clone(),
                text: truncate_chars(py_strip(original), HIT_TEXT_LEN).to_string(),
            });
        }
    }
    matcher
        .targets
        .iter()
        .zip(hits)
        .filter(|(_, hits)| !hits.is_empty())
        .map(|((target, _), hits)| UnlinkedMention {
            file: source.path().to_string(),
            target: (*target).to_string(),
            hits,
        })
        .collect()
}

static CODE_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`[^`]*`").expect("CODE_SPAN regex is valid"));
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s\x1c-\x1f]+").expect("URL regex is valid"));
static FOOTNOTE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\^[^\]]+\]:").expect("FOOTNOTE_LINE regex is valid"));

/// `prose_lines`: `(file line, masked line, original line)` for body prose,
/// skipping fenced code and footnote definitions, with code spans, links and
/// URLs replaced by a space.
fn prose_lines(text: &str) -> Vec<(usize, String, &str)> {
    let (_, body, offset) = split_frontmatter(text);
    let mut in_fence = false;
    let mut out = Vec::new();
    for (index, line) in body.split('\n').enumerate() {
        if py_lstrip(line).starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || FOOTNOTE_LINE.is_match(line) {
            continue;
        }
        let masked = CODE_SPAN.replace_all(line, " ");
        let masked = mask_links(&masked);
        let masked = URL.replace_all(&masked, " ").into_owned();
        out.push((offset + index + 1, masked, line));
    }
    out
}

/// `LINK_RE.sub(" ", text)`.
fn mask_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for link in find_links(text) {
        out.push_str(&text[copied..link.range.start]);
        out.push(' ');
        copied = link.range.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// The first `n` characters (Python's `s[:n]`).
fn truncate_chars(text: &str, n: usize) -> &str {
    text.char_indices().nth(n).map_or(text, |(cut, _)| &text[..cut])
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib analysis::links`
Expected: PASS.

- [ ] **Step 5: Performance check**

Time the report on the real catalog in release mode with a scratch test (not committed):

```rust
#[test]
#[ignore]
fn time_links_report() {
    let ws = crate::okf::workspace::DiskWorkspace::new(std::env::var("OKF_PARITY_CATALOG").unwrap()).unwrap();
    let snapshot = Snapshot::build(&ws).unwrap();
    let start = std::time::Instant::now();
    let report = links_report(&snapshot, true);
    eprintln!("{} mentions in {:?}", report.unlinked_mentions.len(), start.elapsed());
}
```

Run: `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --lib time_links_report -- --ignored --nocapture`.
If it takes over 500 ms, switch the set to a two-stage match (a `RegexSet` of plain `(?i:literal)` patterns as a prefilter, then the bounded pattern for each matched index only), and record that in this plan. Delete the scratch test before committing.

- [ ] **Step 6: Gates, changelog, commit**

```bash
git add src/okf/analysis/ changelog/
git commit -m "analysis: links report (port of okf.py link_analysis)"
```

---

### Task 8: `okf::analysis::oldest` (`oldest --json`)

Port of `AT_RE`, `_ats`, `describe` and `cmd_oldest` (`okf.py` 1143-1233).

**Files:**
- Create: `src/okf/analysis/oldest.rs`
- Modify: `src/okf/analysis/mod.rs` (`pub mod oldest;`)
- Create: `changelog/<ts>-m2-task-08-oldest.md`

- [ ] **Step 1: Write the failing tests**

The expected rows are `okf.py`'s output for the same files with `now` frozen at `2026-10-27T12:00:00Z` (run on 2026-10-27).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::TimeZone;

    const FILES: [(&str, &str); 5] = [
        (
            "a.md",
            "---\ntype: T\ntitle: A\ngenerated: { by: x, at: 2026-10-01T00:00:00Z }\n\
             verified:\n  - { by: x, at: \"2026-10-10T12:00:00+05:30\" }\n\
             open_questions: [Why?, { k: v }]\n---\n",
        ),
        ("b.md", "---\ntype: T\ntitle: B\n---\n"),
        (
            "c.md",
            "---\ntype: T\nstatus: deprecated\ngenerated: { by: x, at: 2020-01-01T00:00:00Z }\n---\n",
        ),
        (
            "d.md",
            "---\ntype: T\ntitle: D\ntitle: again\ngenerated: { by: x, at: 2026-09-01T00:00:00Z }\n\
             stale_after: 2026-01-01T00:00:00Z\n---\n",
        ),
        (
            "e.md",
            "---\ntype: T\ngenerated: { by: x, at: 2026-10-20T00:00:00.5Z }\n\
             stale_after: 2027-01-01T00:00:00Z\n\
             reviewed: { by: x, at: 2026-10-21T00:00:00Z, outcome: verified, note: n }\n---\n",
        ),
    ];

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
    }

    fn rows(files: &[(&str, &str)], query: OldestQuery) -> Vec<OldestRow> {
        let workspace = MemWorkspace::from_files(files.iter().copied());
        oldest(&Snapshot::build(&workspace).unwrap(), now(), query)
    }

    #[test]
    fn rows_match_okf_py() {
        let found = rows(&FILES, OldestQuery { n: 10, open_questions_only: false });
        let json = serde_json::to_value(&found).unwrap();
        let expected = serde_json::json!([
            {"path": "/b.md", "title": "B", "status": "stable", "last_touched": null,
             "last_touched_by": null, "age_days": null, "stale": false, "open_questions": []},
            {"path": "/d.md", "title": "D", "status": "stable",
             "last_touched": "2026-09-01T00:00:00Z", "last_touched_by": "generated",
             "age_days": 56.5, "stale": true, "open_questions": []},
            {"path": "/a.md", "title": "A", "status": "stable",
             "last_touched": "2026-10-10T12:00:00+05:30", "last_touched_by": "verified",
             "age_days": 17.2, "stale": false, "open_questions": ["Why?", "{'k': 'v'}"]},
            {"path": "/e.md", "title": null, "status": "stable",
             "last_touched": "2026-10-21T00:00:00Z", "last_touched_by": "reviewed",
             "age_days": 6.5, "stale": false, "open_questions": []},
        ]);
        assert_eq!(json, expected);
        assert_eq!(json.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn n_limits_and_open_questions_filters() {
        assert_eq!(rows(&FILES, OldestQuery { n: 2, open_questions_only: false }).len(), 2);
        let with_questions = rows(&FILES, OldestQuery { n: 1, open_questions_only: true });
        let paths: Vec<&str> = with_questions.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/a.md"]);
    }

    #[test]
    fn naive_timestamps_are_ignored() {
        // okf.py raises TypeError here (documented divergence).
        let found = rows(
            &[("f.md", "---\ntype: T\ngenerated: { by: x, at: 2026-10-02 }\nstale_after: 2026-01-01\n---\n")],
            OldestQuery::default(),
        );
        assert_eq!(found[0].last_touched, None);
        assert!(!found[0].stale);
    }

    #[test]
    fn equal_instants_keep_the_first_field() {
        // Python's max() returns the first maximal item.
        let found = rows(
            &[(
                "g.md",
                "---\ntype: T\ngenerated: { by: x, at: 2026-10-02T00:00:00Z }\n\
                 verified: { by: x, at: \"2026-10-02T02:00:00+02:00\" }\n---\n",
            )],
            OldestQuery::default(),
        );
        assert_eq!(found[0].last_touched_by, Some("generated"));
        assert_eq!(found[0].last_touched.as_deref(), Some("2026-10-02T00:00:00Z"));
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib analysis::oldest`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! `okf.py oldest --json`: concepts ordered by when they were last touched
//! (`describe` and `cmd_oldest`).
//!
//! # Known divergences from `okf.py`
//!
//! * Naive timestamps (no offset) are ignored; `okf.py` raises `TypeError`.
//! * In the fallback for unparseable frontmatter, an `at:` value that
//!   `AT_RE` matches but that is not a valid date is skipped; `okf.py`
//!   raises `ValueError`.

use std::sync::LazyLock;

use chrono::{DateTime, FixedOffset, Utc};
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;

use crate::okf::pystr::py_str;
use crate::okf::snapshot::{fm_fallback_scalar, Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::timestamp::{parse_iso, IsoFormat};
use crate::okf::yaml::Value;

/// The stamp fields, in the order `okf.py` prefers on a tie.
const STAMP_FIELDS: [&str; 3] = ["generated", "verified", "reviewed"];

/// One row of `oldest --json`, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OldestRow {
    /// `/`-prefixed concept path.
    pub path: String,
    /// `title`, if a string.
    pub title: Option<String>,
    /// `status`, defaulting to `stable`.
    pub status: String,
    /// The newest stamp's `isoformat()`, with `+00:00` written `Z`.
    pub last_touched: Option<String>,
    /// Which field it came from (`generated`, `verified` or `reviewed`).
    pub last_touched_by: Option<&'static str>,
    /// Days since then, rounded to one decimal.
    pub age_days: Option<f64>,
    /// `stale_after` has passed.
    pub stale: bool,
    /// Queued `open_questions`, each as Python's `str()`.
    pub open_questions: Vec<String>,
    /// The sort key (the Unix epoch when never touched); not serialized.
    #[serde(skip)]
    fresh: DateTime<FixedOffset>,
}

/// What `oldest` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OldestQuery {
    /// How many rows (ignored with `open_questions_only`).
    pub n: usize,
    /// Every concept with queued open questions, instead of the `n` oldest.
    pub open_questions_only: bool,
}

impl Default for OldestQuery {
    /// `okf.py`'s defaults: `-n 5`, no `--open-questions`.
    fn default() -> Self {
        Self {
            n: 5,
            open_questions_only: false,
        }
    }
}

/// `cmd_oldest`: non-deprecated concepts, oldest first (ties by path).
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `now` - the current time (from the server clock).
/// * `query` - how many, or only those with open questions.
pub fn oldest(snapshot: &Snapshot, now: DateTime<Utc>, query: OldestQuery) -> Vec<OldestRow> {
    let now = now.fixed_offset();
    let mut rows: Vec<OldestRow> = snapshot
        .concepts()
        .map(|doc| describe(doc, now))
        .filter(|row| row.status != "deprecated")
        .collect();
    // `sort_by` is stable, like Python's `sorted`.
    rows.sort_by(|a, b| a.fresh.cmp(&b.fresh).then_with(|| a.path.cmp(&b.path)));
    if query.open_questions_only {
        rows.retain(|row| !row.open_questions.is_empty());
    } else {
        rows.truncate(query.n);
    }
    rows
}

/// `AT_RE`: `\bat:\s*['"]?(TIMESTAMP)`, `\b` emulated by a consumed character.
static AT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?:\A|[^\p{L}\p{N}_])at:[\s\x1c-\x1f]*['"]?"#,
        r"(\d{4}-\d{2}-\d{2}T[0-9:.]+(?:Z|[+-]\d{2}:?\d{2}))",
    ))
    .expect("AT regex is valid")
});

/// The newest of `values`, keeping the first on a tie (Python's `max`).
fn first_max(values: impl Iterator<Item = DateTime<FixedOffset>>) -> Option<DateTime<FixedOffset>> {
    values.reduce(|best, next| if next > best { next } else { best })
}

/// `_ats`: aware timestamps under the `at` keys of a mapping or list of mappings.
fn ats(value: Option<&Value>) -> impl Iterator<Item = DateTime<FixedOffset>> + '_ {
    let items: &[Value] = match value {
        Some(Value::Seq(items)) => items,
        Some(single) => std::slice::from_ref(single),
        None => &[],
    };
    items.iter().filter_map(|item| match item {
        Value::Map(map) => map.get("at")?.as_str().and_then(parse_iso)?.aware(),
        Value::Null | Value::Str(_) | Value::Seq(_) => None,
    })
}

/// The fields `describe` reads, from parsed frontmatter or the fallback.
struct Described {
    stamps: Vec<(&'static str, DateTime<FixedOffset>)>,
    title: Option<String>,
    status: Option<String>,
    stale_after: Option<String>,
    questions: Vec<String>,
}

fn from_data(data: &IndexMap<String, Value>) -> Described {
    let get = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_string);
    let questions = match data.get("open_questions") {
        Some(Value::Seq(items)) => items.iter().map(py_str).collect(),
        _ => Vec::new(),
    };
    Described {
        stamps: STAMP_FIELDS
            .iter()
            .filter_map(|&field| first_max(ats(data.get(field))).map(|t| (field, t)))
            .collect(),
        title: get("title"),
        status: get("status"),
        stale_after: get("stale_after"),
        questions,
    }
}

/// The regex fallback for frontmatter that does not parse.
fn from_fallback(doc: &SnapshotDoc) -> Described {
    let lines = doc.doc().frontmatter();
    let raw = lines.join("\n");
    let stamps = STAMP_FIELDS
        .iter()
        .filter_map(|&field| {
            let block = doc.doc().block(field)?;
            let text = lines[block.start..block.end].join("\n");
            let found = AT
                .captures_iter(&text)
                .filter_map(|c| parse_iso(&c[1])?.aware());
            first_max(found).map(|t| (field, t))
        })
        .collect();
    Described {
        stamps,
        title: fm_fallback_scalar(&raw, "title"),
        status: fm_fallback_scalar(&raw, "status"),
        stale_after: fm_fallback_scalar(&raw, "stale_after"),
        questions: Vec::new(),
    }
}

/// `describe`: one concept's row.
fn describe(doc: &SnapshotDoc, now: DateTime<FixedOffset>) -> OldestRow {
    let described = match doc.frontmatter() {
        Frontmatter::Parsed(data) => from_data(data),
        Frontmatter::Absent => from_data(&IndexMap::new()),
        Frontmatter::Invalid(_) => from_fallback(doc),
    };
    let newest = described
        .stamps
        .iter()
        .copied()
        .reduce(|best, next| if next.1 > best.1 { next } else { best });
    let stale = described
        .stale_after
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(parse_iso)
        .and_then(|t| t.aware())
        .is_some_and(|t| now >= t);
    OldestRow {
        path: format!("/{}", doc.path()),
        title: described.title,
        status: described
            .status
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "stable".to_string()),
        last_touched: newest.map(|(_, t)| t.to_string_iso().replace("+00:00", "Z")),
        last_touched_by: newest.map(|(field, _)| field),
        age_days: newest.map(|(_, t)| age_days(now, t)),
        stale,
        open_questions: described.questions,
        fresh: newest.map_or(DateTime::<Utc>::UNIX_EPOCH.fixed_offset(), |(_, t)| t),
    }
}

/// `round((now - fresh).total_seconds() / 86400, 1)`.
fn age_days(now: DateTime<FixedOffset>, fresh: DateTime<FixedOffset>) -> f64 {
    let micros = (now - fresh).num_microseconds().unwrap_or(i64::MAX);
    // Exact for |micros| < 2^53 (about 285 years), so both divisions round
    // like Python's `timedelta.total_seconds()` and `/ 86400`.
    round_one(micros as f64 / 1e6 / 86400.0)
}

/// Python's `round(x, 1)`: `{:.1}` rounds the exact binary value with ties
/// to even, as Python does (verified fact 4).
pub(crate) fn round_one(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}
```

`round_one` is `pub(crate)` because `sprawl` uses it too.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib analysis::oldest`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

```bash
git add src/okf/analysis/ changelog/
git commit -m "analysis: oldest (port of okf.py describe/cmd_oldest)"
```

---

### Task 9: `okf::analysis::sprawl` (`sprawl --json`)

Port of `VOLATILE_RE`, `DATE_RE`, `LEADIN_RE`, `SPRAWL_*`, `volatile_reason`, `sprawl_of` and `cmd_sprawl` (`okf.py` 1236-1337).

**Files:**
- Create: `src/okf/analysis/sprawl.rs`
- Modify: `src/okf/analysis/mod.rs` (`pub mod sprawl;`)
- Modify: `src/okf/markdown.rs` (add `pub(crate) fn fence_marker`)
- Create: `changelog/<ts>-m2-task-09-sprawl.md`

- [ ] **Step 1: Expose the fence matcher**

In `src/okf/markdown.rs`, below the `FENCE` static:

```rust
/// The fence marker (`` ``` `` or `~~~`) if `line` opens or closes a fenced
/// block (`okf.py`'s `FENCE_RE.match`).
pub(crate) fn fence_marker(line: &str) -> Option<&str> {
    FENCE.captures(line).and_then(|c| c.get(1)).map(|m| m.as_str())
}
```

Use it in `headings` too, replacing its inline `FENCE.captures` (one definition of the fence rule).

- [ ] **Step 2: Write the failing tests**

Expected values are `okf.py`'s output for the same files (run on 2026-10-27).

````rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    // A plain literal with real newlines keeps the indentation (a `\`
    // line continuation would strip it).
    const HOST: &str = "---
type: Host
sources:
  - { id: a, resource: x }
  - { id: b, resource: y }
  - { id: c, resource: z }
---
# Host

Intro line.
**As of 2026-10-20** the host runs fine.
**Owner** is pgrace.

## Incident on Oct 5, 2026

It broke.
Then 2026-10-06 it was fixed.

## Ports

```
2026-01-01 inside a fence does not count
```
- 22
- 80
";

    fn snapshot() -> Snapshot {
        Snapshot::build(&MemWorkspace::from_files([
            ("host.md", HOST),
            ("todo.md", "---\ntype: Backlog\n---\n# Items\n\n- one\n- two\n"),
            ("plain.md", "no frontmatter\n## History\n"),
        ]))
        .unwrap()
    }

    #[test]
    fn host_report_matches_okf_py() {
        let rows = sprawl(&snapshot(), SprawlQuery { concept: None, n: 10 }).unwrap();
        let json = serde_json::to_value(&rows).unwrap();
        let expected = serde_json::json!([
            {"path": "/host.md", "score": 6.8, "exempt": null, "lines": 13, "sections": 3,
             "sources": 3, "volatile": 2, "dates": 3,
             "sections_detail": [
                {"heading": "Host", "level": 1, "line": 8, "lines": 12, "volatile": null},
                {"heading": "Incident on Oct 5, 2026", "level": 2, "line": 14, "lines": 2,
                 "volatile": "incident"},
                {"heading": "Ports", "level": 2, "line": 19, "lines": 5, "volatile": null}],
             "volatile_detail": [
                {"kind": "heading", "text": "Incident on Oct 5, 2026", "line": 14, "why": "incident"},
                {"kind": "lead-in", "text": "As of 2026-10-20", "line": 11, "why": "as of"}],
             "candidates": [
                {"heading": "Incident on Oct 5, 2026", "level": 2, "line": 14, "lines": 2,
                 "volatile": "incident"},
                {"heading": "Host", "level": 1, "line": 8, "lines": 12, "volatile": null}]},
            {"path": "/plain.md", "score": 2.1, "exempt": null, "lines": 2, "sections": 1,
             "sources": 0, "volatile": 1, "dates": 0,
             "sections_detail": [
                {"heading": "History", "level": 2, "line": 2, "lines": 0, "volatile": "history"}],
             "volatile_detail": [
                {"kind": "heading", "text": "History", "line": 2, "why": "history"}],
             "candidates": [
                {"heading": "History", "level": 2, "line": 2, "lines": 0, "volatile": "history"}]},
        ]);
        assert_eq!(json, expected);
        assert_eq!(json.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn exempt_types_are_listed_only_by_name() {
        let rows = sprawl(&snapshot(), SprawlQuery { concept: Some("todo"), n: 5 }).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].exempt.as_deref(), rows[0].score), (Some("Backlog"), 0.7));
        assert!(matches!(
            sprawl(&snapshot(), SprawlQuery { concept: Some("nope"), n: 5 }),
            Err(OkfError::NotFound { .. })
        ));
    }

    #[test]
    fn volatile_words_ignore_case_like_python() {
        assert_eq!(volatile_reason("Recent \u{130}NCIDENT").as_deref(), Some("i\u{307}ncident"));
        assert_eq!(volatile_reason("Seen Oct 5, 2026"), Some("Oct 5, 2026".to_string()));
        assert_eq!(volatile_reason("fixedly"), None, "no word boundary after `fixed`");
    }
}
````

> `"\u{130}NCIDENT".to_lowercase()` is `"i\u{307}ncident"` in both Python 3.14 and Rust (checked).

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib analysis::sprawl`
Expected: compile errors.

- [ ] **Step 4: Implement**

```rust
//! `okf.py sprawl --json`: which concepts should be split for progressive
//! disclosure (`sprawl_of`, `cmd_sprawl`).
//!
//! score = lines/20 + sections/2 + sources/3 + 1.5 per volatile heading or
//! lead-in + distinct dates/5, rounded to one decimal.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::error::OkfError;
use crate::okf::analysis::oldest::round_one;
use crate::okf::markdown::{body_split, fence_marker, headings};
use crate::okf::pystr::py_strip;
use crate::okf::snapshot::{Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// `lint` warns above this score.
pub const SPRAWL_WARN: f64 = 16.0;
/// Concept types that grow by design (a list of entries, not a hub).
const EXEMPT_TYPES: [&str; 1] = ["backlog"];

/// One heading's section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SprawlSection {
    /// Heading text.
    pub heading: String,
    /// 1 to 6.
    pub level: usize,
    /// 1-based file line.
    pub line: usize,
    /// Non-blank lines until the next heading of the same or a higher level.
    pub lines: usize,
    /// Why the heading reads as volatile, if it does.
    pub volatile: Option<String>,
}

/// A volatile heading or bold lead-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolatileItem {
    /// `heading` or `lead-in`.
    pub kind: &'static str,
    /// The heading or lead-in text.
    pub text: String,
    /// 1-based file line.
    pub line: usize,
    /// The matched volatile word, or the date.
    pub why: String,
}

/// One concept's sprawl report, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SprawlReport {
    /// `/`-prefixed path.
    pub path: String,
    /// The score.
    pub score: f64,
    /// The concept's `type` when it is exempt (e.g. `Backlog`).
    pub exempt: Option<String>,
    /// Non-blank body lines.
    pub lines: usize,
    /// Headings.
    pub sections: usize,
    /// Entries in `sources`.
    pub sources: usize,
    /// Volatile headings and lead-ins.
    pub volatile: usize,
    /// Distinct dates outside code.
    pub dates: usize,
    /// Every section.
    pub sections_detail: Vec<SprawlSection>,
    /// Every volatile heading and lead-in.
    pub volatile_detail: Vec<VolatileItem>,
    /// Split candidates: volatile sections first, then large ones.
    pub candidates: Vec<SprawlSection>,
}

/// What `sprawl` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SprawlQuery<'a> {
    /// One concept (exempt or not), instead of the top `n`.
    pub concept: Option<&'a str>,
    /// How many of the highest scores (exempt concepts excluded).
    pub n: usize,
}

/// The `\w` class and its complement, for the emulated `\b`.
const WORD: &str = r"\p{L}\p{N}_";

/// `VOLATILE_RE` alternatives. `i` is widened to `[iI\u{130}\u{131}]` when
/// the regex is built (verified fact 6).
const VOLATILE_WORDS: &str = "as of|current state|status|outage|incident|investigation|\
    post-?mortem|change ?log|changes since|changed|open (?:items|questions)|traps?|gotchas?|\
    fix(?:ed|es)?|workaround|stuck|broken|to ?do|snapshot|history|timeline|procedure|runbook|\
    to check";

/// `\b(WORDS)\b`, case-insensitive. Every alternative starts and ends with
/// a word character, so the boundaries become consumed non-word neighbours.
static VOLATILE: LazyLock<Regex> = LazyLock::new(|| {
    let words = VOLATILE_WORDS.replace('i', "[iI\u{130}\u{131}]");
    Regex::new(&format!(r"(?:\A|[^{WORD}])(?i:({words}))(?:[^{WORD}]|\z)"))
        .expect("VOLATILE regex is valid")
});

/// `DATE_RE`: `2026-10-05`, `2026-10`, or `Oct 5, 2026` / `October 2026`.
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        concat!(
            r"(?:\A|[^{w}])(?i:((?:19|20)\d{{2}}-[01]\d(?:-[0-3]\d)?|",
            r"(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z\u{{130}}\u{{131}}]*\.? ",
            r"(?:\d{{1,2}},? )?(?:19|20)\d{{2}}))(?:[^{w}]|\z)"
        ),
        w = WORD
    ))
    .expect("DATE regex is valid")
});

/// `LEADIN_RE`: a bold lead-in at the start of a line.
static LEADIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\*\*([^*]{2,80}?)\*\*").expect("LEADIN regex is valid"));

/// `volatile_reason`: the lowercased volatile word, else the first date as written.
fn volatile_reason(text: &str) -> Option<String> {
    if let Some(found) = VOLATILE.captures(text) {
        return Some(found[1].to_lowercase());
    }
    DATE.captures(text).map(|found| found[1].to_string())
}

/// `DATE_RE.findall(line)`: non-overlapping dates, left to right.
fn dates_in(line: &str) -> impl Iterator<Item = &str> {
    let mut pos = 0;
    // `from_fn` builds an iterator from a closure that returns the next item.
    std::iter::from_fn(move || {
        let found = DATE.captures_at(line, pos)?.get(1)?;
        // Resume at the date's end so the consumed boundary character can
        // be the next match's leading boundary.
        pos = found.end();
        Some(found.as_str())
    })
}

/// `sprawl_of`: one concept's report.
pub fn sprawl_of(doc: &SnapshotDoc) -> SprawlReport {
    let (head, body) = body_split(doc.text());
    let data = doc.frontmatter().data();
    let sources = match data.and_then(|d| d.get("sources")) {
        Some(Value::Seq(items)) => items.len(),
        _ => 0,
    };
    let exempt = data
        .and_then(|d| d.get("type"))
        .and_then(Value::as_str)
        .filter(|t| EXEMPT_TYPES.contains(&py_strip(t).to_lowercase().as_str()))
        .map(str::to_string);

    // Lines inside fenced code, the fence lines included.
    let mut code = HashSet::new();
    let mut fence: Option<&str> = None;
    for (index, line) in body.iter().enumerate() {
        let marker = fence_marker(line);
        if marker.is_some() || fence.is_some() {
            code.insert(index);
        }
        if let Some(marker) = marker {
            fence = match fence {
                Some(open) if open == marker => None,
                Some(open) => Some(open),
                None => Some(marker),
            };
        }
    }
    let lines = body.iter().filter(|l| !py_strip(l).is_empty()).count();
    let dates: HashSet<String> = body
        .iter()
        .enumerate()
        .filter(|(index, _)| !code.contains(index))
        .flat_map(|(_, line)| dates_in(line))
        .map(str::to_lowercase)
        .collect();

    // Line numbers in the file: the frontmatter lines come first.
    let offset = head.len() + 1;
    let found = headings(&body);
    let mut sections = Vec::with_capacity(found.len());
    let mut volatile = Vec::new();
    for (k, heading) in found.iter().enumerate() {
        let end = found[k + 1..]
            .iter()
            .find(|h| h.level <= heading.level)
            .map_or(body.len(), |h| h.index);
        let size = body[heading.index + 1..end]
            .iter()
            .filter(|l| !py_strip(l).is_empty())
            .count();
        let why = volatile_reason(&heading.text);
        if let Some(why) = &why {
            volatile.push(VolatileItem {
                kind: "heading",
                text: heading.text.clone(),
                line: heading.index + offset,
                why: why.clone(),
            });
        }
        sections.push(SprawlSection {
            heading: heading.text.clone(),
            level: heading.level,
            line: heading.index + offset,
            lines: size,
            volatile: why,
        });
    }
    for (index, line) in body.iter().enumerate() {
        if code.contains(&index) {
            continue;
        }
        let Some(lead_in) = LEADIN.captures(line).and_then(|c| c.get(1)) else {
            continue;
        };
        if let Some(why) = volatile_reason(lead_in.as_str()) {
            volatile.push(VolatileItem {
                kind: "lead-in",
                text: py_strip(lead_in.as_str()).to_string(),
                line: index + offset,
                why,
            });
        }
    }

    // The weights in okf.py's order and arithmetic: weight * count, summed left to right.
    let score = round_one(
        (1.0 / 20.0) * lines as f64
            + (1.0 / 2.0) * found.len() as f64
            + (1.0 / 3.0) * sources as f64
            + 1.5 * volatile.len() as f64
            + (1.0 / 5.0) * dates.len() as f64,
    );
    let large = 12.max(lines / 5);
    let mut candidates: Vec<SprawlSection> = sections
        .iter()
        .filter(|s| s.volatile.is_some() || s.lines >= large)
        .cloned()
        .collect();
    // Stable sort on (not volatile, -lines), as okf.py's key (-bool(v), -lines).
    candidates.sort_by(|a, b| {
        b.volatile
            .is_some()
            .cmp(&a.volatile.is_some())
            .then(b.lines.cmp(&a.lines))
    });
    SprawlReport {
        path: format!("/{}", doc.path()),
        score,
        exempt,
        lines,
        sections: found.len(),
        sources,
        volatile: volatile.len(),
        dates: dates.len(),
        sections_detail: sections,
        volatile_detail: volatile,
        candidates,
    }
}

/// `cmd_sprawl`: the highest scores first (ties by path), either the top
/// `n` non-exempt concepts or the one named concept.
///
/// # Errors
/// From [`Snapshot::resolve_concept`] when `query.concept` names no concept.
pub fn sprawl(snapshot: &Snapshot, query: SprawlQuery<'_>) -> Result<Vec<SprawlReport>, OkfError> {
    if let Some(reference) = query.concept {
        let path = snapshot.resolve_concept(reference)?;
        let doc = snapshot.get(&path).ok_or(OkfError::NotFound { path })?;
        return Ok(vec![sprawl_of(doc)]);
    }
    let mut rows: Vec<SprawlReport> = snapshot.concepts().map(sprawl_of).collect();
    rows.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    rows.retain(|row| row.exempt.is_none());
    rows.truncate(query.n);
    Ok(rows)
}
```

Check `offset` against `okf.py`: it is `text.count("\n", 0, len(text) - len("\n".join(body)))`, the number of lines before the body, which is `head.len()`; `okf.py` then adds 1 to each 0-based body index, so the file line is `index + head.len() + 1`, which is what `offset = head.len() + 1` gives.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib analysis::sprawl`
Expected: PASS.

- [ ] **Step 6: Gates, changelog, commit**

```bash
git add src/okf/analysis/ src/okf/markdown.rs changelog/
git commit -m "analysis: sprawl (port of okf.py sprawl_of/cmd_sprawl)"
```

---

### Task 10: `okf::analysis::lint` (`lint --json`)

Port of `check_ts` (1815) and `lint` (1835-1972). Git commit times come in as data (decision 2).

**Files:**
- Create: `src/okf/analysis/lint.rs`
- Create: `src/okf/analysis/testdata/lint-mini.json`
- Modify: `src/okf/analysis/mod.rs` (`pub mod lint;`)
- Create: `changelog/<ts>-m2-task-10-lint.md`

- [ ] **Step 1: Write the failing tests**

`src/okf/analysis/testdata/lint-mini.json` is `okf.py`'s `lint()` output for the `MINI` catalog below, with `now_utc` frozen at `2026-10-27T12:00:00Z` and `last_commit_times` returning `({"notype.md": 2026-10-19T00:00:00Z, "stamps.md": 2026-10-01T00:00:00Z}, {"stamps.md"})` (generated on 2026-10-27):

```json
{
  "errors": [
    {"file": "bare.md", "line": 1, "rule": "no-frontmatter", "message": "concept has no YAML frontmatter block"},
    {"file": "broken.md", "line": 3, "rule": "frontmatter-parse", "message": "frontmatter does not parse: expected ',' or ']' in flow list in '[x'"},
    {"file": "infra/sub/index.md", "line": 1, "rule": "index-frontmatter", "message": "non-root index.md must not have frontmatter"},
    {"file": "notype.md", "line": 1, "rule": "no-type", "message": "frontmatter has no 'type'"},
    {"file": "shapes.md", "line": 3, "rule": "timestamp", "message": "generated.at is not an ISO 8601 timestamp: '2026-10-01'"},
    {"file": "stamps.md", "line": 4, "rule": "future-timestamp", "message": "generated.at 2027-01-01T00:00:00Z is in the future (now 2026-10-27T12:00:00Z)"},
    {"file": "stamps.md", "line": 10, "rule": "timestamp", "message": "sources[1].last_modified is not a valid date/time: '2026-13-01T00:00:00Z'"},
    {"file": "stamps.md", "line": 14, "rule": "broken-link", "message": "link target does not exist: /gone.md"}
  ],
  "warnings": [
    {"file": "bare.md", "line": 1, "rule": "index-drift", "message": "not listed in index.md"},
    {"file": "bare.md", "line": 1, "rule": "orphan", "message": "no other concept links to this concept"},
    {"file": "broken.md", "line": 1, "rule": "index-drift", "message": "not listed in index.md"},
    {"file": "broken.md", "line": 1, "rule": "orphan", "message": "no other concept links to this concept"},
    {"file": "index.md", "line": 1, "rule": "okf-version", "message": "root index.md does not declare okf_version"},
    {"file": "notype.md", "line": 1, "rule": "index-drift", "message": "not listed in index.md"},
    {"file": "notype.md", "line": 1, "rule": "no-description", "message": "frontmatter has no 'description'"},
    {"file": "notype.md", "line": 1, "rule": "orphan", "message": "no other concept links to this concept"},
    {"file": "notype.md", "line": 3, "rule": "generated-after-commit", "message": "generated.at 2026-10-20T00:00:00Z is later than the file's last commit 2026-10-19T00:00:00Z"},
    {"file": "shapes.md", "line": 1, "rule": "index-drift", "message": "not listed in index.md"},
    {"file": "shapes.md", "line": 1, "rule": "no-description", "message": "frontmatter has no 'description'"},
    {"file": "shapes.md", "line": 1, "rule": "no-title", "message": "frontmatter has no 'title'"},
    {"file": "shapes.md", "line": 1, "rule": "orphan", "message": "no other concept links to this concept"},
    {"file": "shapes.md", "line": 3, "rule": "generated-shape", "message": "generated should be a mapping with 'by' and 'at'"},
    {"file": "shapes.md", "line": 4, "rule": "reviewed-shape", "message": "reviewed should be a mapping with by, at, outcome, note"},
    {"file": "shapes.md", "line": 5, "rule": "sources-shape", "message": "sources should be a list"},
    {"file": "stamps.md", "line": 1, "rule": "no-description", "message": "frontmatter has no 'description'"},
    {"file": "stamps.md", "line": 1, "rule": "no-title", "message": "frontmatter has no 'title'"},
    {"file": "stamps.md", "line": 1, "rule": "orphan", "message": "no other concept links to this concept"},
    {"file": "stamps.md", "line": 3, "rule": "status", "message": "status 'bogus' is not draft, stable or deprecated"},
    {"file": "stamps.md", "line": 5, "rule": "timestamp-utc", "message": "verified[0].at is not in UTC: '2026-10-01T10:00:00-04:00'"},
    {"file": "stamps.md", "line": 5, "rule": "verified-shape", "message": "verified[1] should be a mapping with 'by' and 'at'"},
    {"file": "stamps.md", "line": 8, "rule": "reviewed-outcome", "message": "reviewed.outcome 'maybe' is not one of verified, updated, partial, unverifiable"},
    {"file": "stamps.md", "line": 9, "rule": "stale", "message": "stale_after 2026-01-01T00:00:00Z has passed; re-check the concept"},
    {"file": "stamps.md", "line": 10, "rule": "sources-shape", "message": "sources[0] has no 'resource'"},
    {"file": "stamps.md", "line": 14, "rule": "relative-link", "message": "relative link infra/index.md; prefer a bundle-root absolute path"}
  ]
}
```

The tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::timestamp::parse_iso;
    use crate::okf::workspace::MemWorkspace;
    use chrono::TimeZone;

    const MINI: [(&str, &str); 8] = [
        ("bare.md", "no frontmatter\n"),
        ("broken.md", "---\ntype: T\nbad: [x\n---\n"),
        ("index.md", "---\ntitle: Root\n---\n# Concepts\n\n* [Stamps](/stamps.md)\n* [Infra](infra/)\n"),
        ("infra/index.md", "* [Sub](/infra/sub/)\n"),
        ("infra/sub/index.md", "---\nx: 1\n---\n"),
        ("notype.md", "---\ntitle: No type\ngenerated: { by: x, at: 2026-10-20T00:00:00Z }\n---\n"),
        ("shapes.md", "---\ntype: T\ngenerated: { at: \"2026-10-01\" }\nreviewed: yes\nsources: one\n---\n"),
        ("stamps.md", "---\ntype: Note\nstatus: bogus\ngenerated: { by: x, at: 2027-01-01T00:00:00Z }\nverified:\n  - { by: x, at: \"2026-10-01T10:00:00-04:00\" }\n  - plain\nreviewed: { by: x, at: 2026-10-02T00:00:00Z, outcome: maybe }\nstale_after: 2026-01-01T00:00:00Z\nsources:\n  - { id: a }\n  - { id: b, resource: r, last_modified: 2026-13-01T00:00:00Z }\n---\nSee [gone](/gone.md) and [rel](infra/index.md).\n"),
    ];

    fn at(text: &str) -> DateTime<FixedOffset> {
        parse_iso(text).and_then(|t| t.aware()).expect("aware timestamp")
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
    }

    fn mini_report() -> LintReport {
        let snapshot = Snapshot::build(&MemWorkspace::from_files(MINI)).unwrap();
        let commits = CommitTimes::new(
            [
                ("notype.md".to_string(), at("2026-10-19T00:00:00Z")),
                ("stamps.md".to_string(), at("2026-10-01T00:00:00Z")),
            ]
            .into_iter()
            .collect(),
            ["stamps.md".to_string()].into_iter().collect(),
        );
        lint(&snapshot, now(), Some(&commits))
    }

    #[test]
    fn mini_catalog_matches_okf_py() {
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("testdata/lint-mini.json")).unwrap();
        let actual = serde_json::to_value(mini_report()).unwrap();
        // Compare finding by finding so a failure names the one at fault.
        for list in ["errors", "warnings"] {
            let (a, e) = (actual[list].as_array().unwrap(), expected[list].as_array().unwrap());
            for (i, (a, e)) in a.iter().zip(e).enumerate() {
                assert_eq!(a, e, "{list}[{i}]");
            }
            assert_eq!(a.len(), e.len(), "{list} count");
        }
        assert_eq!(actual.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn without_git_there_is_no_commit_check() {
        let snapshot = Snapshot::build(&MemWorkspace::from_files(MINI)).unwrap();
        let report = lint(&snapshot, now(), None);
        assert!(report.warnings.iter().all(|w| w.rule != "generated-after-commit"));
    }

    #[test]
    fn secrets_and_sprawl_are_reported() {
        let value = format!("Ab{}", "c1".repeat(4));
        let long_body = "line\n".repeat(400);
        let files = [
            ("index.md", "---\nokf_version: \"0.2\"\n---\n* [A](/a.md)\n".to_string()),
            ("a.md", format!("---\ntype: T\ntitle: A\ndescription: d\n---\n{long_body}")),
            ("notes.txt", format!("password: {value}")),
        ];
        let workspace = MemWorkspace::from_files(files.iter().map(|(p, c)| (*p, c.as_str())));
        let report = lint(&Snapshot::build(&workspace).unwrap(), now(), None);
        assert!(report.errors.iter().any(|e| e.rule == "secret:assignment" && e.file == "notes.txt"));
        let sprawl = report.warnings.iter().find(|w| w.rule == "sprawl").expect("sprawl warning");
        assert_eq!(
            sprawl.message,
            "sprawl score 20.0 > 16 (400 lines, 0 sections, 0 volatile); consider splitting \
             (okf.py sprawl a.md, okf-reorganize skill)"
        );
    }

    #[test]
    fn non_string_values_are_reported_with_python_repr() {
        let files = [(
            "x.md",
            "---\ntype: T\nstatus: [a, b]\nreviewed: { at: 2026-10-01T00:00:00Z, outcome: [1] }\n---\n",
        )];
        let report = lint(&Snapshot::build(&MemWorkspace::from_files(files)).unwrap(), now(), None);
        let messages: Vec<&str> = report.warnings.iter().map(|w| w.message.as_str()).collect();
        // okf.py raises TypeError on an unhashable status (documented divergence).
        assert!(messages.contains(&"status ['a', 'b'] is not draft, stable or deprecated"));
        assert!(messages.contains(
            &"reviewed.outcome ['1'] is not one of verified, updated, partial, unverifiable"
        ));
    }
}
```

The sprawl expectation: 400 non-blank lines give 400/20 = 20.0, and `okf.py` formats the float as `20.0` (verified fact 5).

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib analysis::lint`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! `okf.py lint --json`: frontmatter, timestamp, link, index, sprawl and
//! secret checks, as errors and warnings.
//!
//! The one check that needs git (`generated-after-commit`) takes the commit
//! times as data, so this module stays pure; M3's git module fills them.
//!
//! # Known divergences from `okf.py`
//!
//! * A `status` that is a list or mapping is reported with its `repr`;
//!   `okf.py` raises `TypeError` (unhashable type).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::Serialize;

use crate::clock::TIMESTAMP_FORMAT;
use crate::okf::analysis::links::links_report;
use crate::okf::analysis::sprawl::{sprawl_of, SPRAWL_WARN};
use crate::okf::analysis::Finding;
use crate::okf::paths::is_concept;
use crate::okf::pystr::{py_repr, py_repr_opt, py_truthy};
use crate::okf::snapshot::{Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::timestamp::{is_iso_timestamp, parse_iso};
use crate::okf::yaml::Value;

/// `STATUSES`.
const STATUSES: [&str; 3] = ["draft", "stable", "deprecated"];
/// `OUTCOMES`, in `okf.py`'s order (the message lists them).
const OUTCOMES: [&str; 4] = ["verified", "updated", "partial", "unverifiable"];
/// `FUTURE_SLACK`: two minutes of clock skew allowed.
const FUTURE_SLACK_SECONDS: i64 = 120;

/// Each file's last commit time, and the files with uncommitted changes:
/// what `okf.py`'s `last_commit_times()` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitTimes {
    times: HashMap<String, DateTime<FixedOffset>>,
    dirty: HashSet<String>,
}

impl CommitTimes {
    /// Builds the record.
    ///
    /// # Arguments
    /// * `times` - catalog-relative path to the committer time of the last
    ///   commit touching it.
    /// * `dirty` - paths with uncommitted changes (`git status --porcelain`).
    pub fn new(times: HashMap<String, DateTime<FixedOffset>>, dirty: HashSet<String>) -> Self {
        Self { times, dirty }
    }
}

/// `{"errors": [...], "warnings": [...]}`, each sorted by (file, line, rule).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LintReport {
    /// Problems that block `publish`.
    pub errors: Vec<Finding>,
    /// Advice.
    pub warnings: Vec<Finding>,
}

/// Runs every lint check.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `now` - the current time, for the future and `stale_after` checks.
/// * `commits` - git commit times, or `None` when git is unavailable (the
///   `generated-after-commit` check is then skipped, as in `okf.py`).
///
/// # Returns
/// The findings, each list sorted by (file, line, rule) with ties in
/// discovery order.
pub fn lint(snapshot: &Snapshot, now: DateTime<Utc>, commits: Option<&CommitTimes>) -> LintReport {
    let mut lint = Linter {
        now: now.fixed_offset(),
        report: LintReport::default(),
    };
    for doc in snapshot.docs() {
        lint.document(doc, commits);
    }
    lint.links(snapshot);
    for doc in snapshot.concepts() {
        let sprawl = sprawl_of(doc);
        if sprawl.score > SPRAWL_WARN && sprawl.exempt.is_none() {
            lint.warn(
                doc.path(),
                1,
                "sprawl",
                format!(
                    // `{:?}` prints an f64 as Python's str() does (20.0, not 20).
                    "sprawl score {:?} > {} ({} lines, {} sections, {} volatile); consider \
                     splitting (okf.py sprawl {}, okf-reorganize skill)",
                    sprawl.score,
                    SPRAWL_WARN,
                    sprawl.lines,
                    sprawl.sections,
                    sprawl.volatile,
                    doc.path()
                ),
            );
        }
    }
    lint.report.errors.extend(snapshot.secret_findings().iter().cloned());
    let key = |f: &Finding| (f.file.clone(), f.line, f.rule.clone());
    // `sort_by_key` is stable, like Python's `sorted`.
    lint.report.errors.sort_by_key(key);
    lint.report.warnings.sort_by_key(key);
    lint.report
}

/// The findings being collected, and "now".
struct Linter {
    now: DateTime<FixedOffset>,
    report: LintReport,
}

impl Linter {
    fn error(&mut self, file: &str, line: usize, rule: &str, message: String) {
        self.report.errors.push(finding(file, line, rule, message));
    }

    fn warn(&mut self, file: &str, line: usize, rule: &str, message: String) {
        self.report.warnings.push(finding(file, line, rule, message));
    }

    /// The per-file checks of `okf.py`'s first loop.
    fn document(&mut self, doc: &SnapshotDoc, commits: Option<&CommitTimes>) {
        let path = doc.path();
        if path == "index.md" || path.ends_with("/index.md") {
            self.index(doc);
            return;
        }
        if !is_concept(path) {
            return;
        }
        let data = match doc.frontmatter() {
            Frontmatter::Absent => {
                let message = "concept has no YAML frontmatter block".to_string();
                return self.error(path, 1, "no-frontmatter", message);
            }
            Frontmatter::Invalid(error) => {
                let message = format!("frontmatter does not parse: {}", error.message);
                return self.error(path, error.line.unwrap_or(1), "frontmatter-parse", message);
            }
            Frontmatter::Parsed(data) => data,
        };
        let line = |key: &str| doc.doc().key_line(key);
        let get = |key: &str| data.get(key).filter(|v| !matches!(v, Value::Null));

        if !py_truthy(data.get("type")) {
            self.error(path, 1, "no-type", "frontmatter has no 'type'".to_string());
        }
        for key in ["title", "description"] {
            if !py_truthy(data.get(key)) {
                self.warn(path, 1, &format!("no-{key}"), format!("frontmatter has no '{key}'"));
            }
        }
        if let Some(status) = get("status") {
            if !status.as_str().is_some_and(|s| STATUSES.contains(&s)) {
                let message =
                    format!("status {} is not draft, stable or deprecated", py_repr(status));
                self.warn(path, line("status"), "status", message);
            }
        }
        let generated_at = get("generated").and_then(|g| self.generated(path, line("generated"), g));
        if let Some(verified) = get("verified") {
            self.verified(path, line("verified"), verified);
        }
        if let Some(reviewed) = get("reviewed") {
            self.reviewed(path, line("reviewed"), reviewed);
        }
        if let Some(stale_after) = get("stale_after") {
            let at = line("stale_after");
            if let Some(ts) = self.check_ts(stale_after, "stale_after", path, at, false) {
                if ts <= self.now {
                    let message = format!(
                        "stale_after {} has passed; re-check the concept",
                        stale_after.as_str().unwrap_or_default()
                    );
                    self.warn(path, at, "stale", message);
                }
            }
        }
        if let Some(sources) = get("sources") {
            self.sources(path, line("sources"), sources);
        }
        let (Some(generated_at), Some(commits)) = (generated_at, commits) else {
            return;
        };
        if commits.dirty.contains(path) {
            return;
        }
        let Some(&committed) = commits.times.get(path) else {
            return;
        };
        if generated_at > committed + Duration::seconds(FUTURE_SLACK_SECONDS) {
            let message = format!(
                // okf.py formats generated.at with strftime on its own
                // offset (local fields, then a literal Z): `naive_local`.
                "generated.at {} is later than the file's last commit {}",
                generated_at.naive_local().format(TIMESTAMP_FORMAT),
                committed.with_timezone(&Utc).format(TIMESTAMP_FORMAT)
            );
            self.warn(path, line("generated"), "generated-after-commit", message);
        }
    }

    /// `index.md` rules: no frontmatter below the root; the root declares `okf_version`.
    fn index(&mut self, doc: &SnapshotDoc) {
        let path = doc.path();
        if path != "index.md" {
            if doc.doc().has_frontmatter() {
                let message = "non-root index.md must not have frontmatter".to_string();
                self.error(path, 1, "index-frontmatter", message);
            }
            return;
        }
        match doc.frontmatter() {
            Frontmatter::Invalid(error) => {
                let message = format!("frontmatter does not parse: {}", error.message);
                self.error(path, error.line.unwrap_or(1), "frontmatter-parse", message);
            }
            Frontmatter::Absent | Frontmatter::Parsed(_) => {
                let declared = doc
                    .frontmatter()
                    .data()
                    .and_then(|d| d.get("okf_version"))
                    .is_some_and(|v| !matches!(v, Value::Null));
                if !declared {
                    let message = "root index.md does not declare okf_version".to_string();
                    self.warn(path, 1, "okf-version", message);
                }
            }
        }
    }

    fn generated(&mut self, path: &str, line: usize, value: &Value) -> Option<DateTime<FixedOffset>> {
        let map = match value {
            Value::Map(map) => Some(map),
            Value::Null | Value::Str(_) | Value::Seq(_) => None,
        };
        if !map.is_some_and(|m| m.contains_key("at") && m.contains_key("by")) {
            let message = "generated should be a mapping with 'by' and 'at'".to_string();
            self.warn(path, line, "generated-shape", message);
        }
        let at = map?.get("at")?;
        self.check_ts(at, "generated.at", path, line, true)
    }

    fn verified(&mut self, path: &str, line: usize, value: &Value) {
        let items = match value {
            Value::Seq(items) => items.as_slice(),
            Value::Null | Value::Str(_) | Value::Map(_) => std::slice::from_ref(value),
        };
        for (i, item) in items.iter().enumerate() {
            match item {
                Value::Map(map) if map.contains_key("at") => {
                    let at = &map["at"];
                    self.check_ts(at, &format!("verified[{i}].at"), path, line, true);
                }
                _ => {
                    let message = format!("verified[{i}] should be a mapping with 'by' and 'at'");
                    self.warn(path, line, "verified-shape", message);
                }
            }
        }
    }

    fn reviewed(&mut self, path: &str, line: usize, value: &Value) {
        let Value::Map(map) = value else {
            let message = "reviewed should be a mapping with by, at, outcome, note".to_string();
            return self.warn(path, line, "reviewed-shape", message);
        };
        let Some(at) = map.get("at") else {
            let message = "reviewed should be a mapping with by, at, outcome, note".to_string();
            return self.warn(path, line, "reviewed-shape", message);
        };
        self.check_ts(at, "reviewed.at", path, line, true);
        let outcome = map.get("outcome");
        if !outcome.and_then(Value::as_str).is_some_and(|o| OUTCOMES.contains(&o)) {
            let message = format!(
                "reviewed.outcome {} is not one of {}",
                py_repr_opt(outcome),
                OUTCOMES.join(", ")
            );
            self.warn(path, line, "reviewed-outcome", message);
        }
    }

    fn sources(&mut self, path: &str, line: usize, value: &Value) {
        let Value::Seq(items) = value else {
            return self.warn(path, line, "sources-shape", "sources should be a list".to_string());
        };
        for (i, item) in items.iter().enumerate() {
            let map = match item {
                Value::Map(map) if py_truthy(map.get("resource")) => map,
                _ => {
                    let message = format!("sources[{i}] has no 'resource'");
                    self.warn(path, line, "sources-shape", message);
                    continue;
                }
            };
            if let Some(modified) = map.get("last_modified").filter(|v| !matches!(v, Value::Null)) {
                let label = format!("sources[{i}].last_modified");
                self.check_ts(modified, &label, path, line, false);
            }
        }
    }

    /// `check_ts`: shape, validity, UTC and (optionally) not in the future.
    fn check_ts(
        &mut self,
        value: &Value,
        label: &str,
        path: &str,
        line: usize,
        future_check: bool,
    ) -> Option<DateTime<FixedOffset>> {
        let Some(text) = value.as_str().filter(|t| is_iso_timestamp(t)) else {
            let message = format!("{label} is not an ISO 8601 timestamp: {}", py_repr(value));
            self.error(path, line, "timestamp", message);
            return None;
        };
        let Some(ts) = parse_iso(text).and_then(|t| t.aware()) else {
            let message = format!("{label} is not a valid date/time: {}", py_repr(value));
            self.error(path, line, "timestamp", message);
            return None;
        };
        if !text.ends_with('Z') && !text.ends_with("+00:00") {
            let message = format!("{label} is not in UTC: {}", py_repr(value));
            self.warn(path, line, "timestamp-utc", message);
        }
        if future_check && ts > self.now + Duration::seconds(FUTURE_SLACK_SECONDS) {
            let message = format!(
                "{label} {text} is in the future (now {})",
                self.now.format(TIMESTAMP_FORMAT)
            );
            self.error(path, line, "future-timestamp", message);
        }
        Some(ts)
    }

    /// The link report's findings, as `okf.py` maps them.
    fn links(&mut self, snapshot: &Snapshot) {
        let links = links_report(snapshot, false);
        for b in links.broken {
            let message = format!("link target does not exist: {}", b.link);
            self.error(&b.file, b.line, "broken-link", message);
        }
        for b in links.broken_anchors {
            let message = format!("no heading in the target matches the anchor: {}", b.link);
            self.error(&b.file, b.line, "broken-anchor", message);
        }
        for b in links.relative {
            let message = format!("relative link {}; prefer a bundle-root absolute path", b.link);
            self.warn(&b.file, b.line, "relative-link", message);
        }
        for orphan in links.orphans {
            let message = "no other concept links to this concept".to_string();
            self.warn(&orphan, 1, "orphan", message);
        }
        for drift in links.index_drift {
            self.warn(&drift.file, 1, "index-drift", drift.problem);
        }
    }
}

fn finding(file: &str, line: usize, rule: &str, message: String) -> Finding {
    Finding {
        file: file.to_string(),
        line,
        rule: rule.to_string(),
        message,
    }
}
```

Python details this code mirrors, for the reviewer:

- `data.get(k) is not None` treats a YAML `null` like a missing key, hence `get` filtering `Value::Null`; `not data.get("type")` is [`py_truthy`].
- `SPRAWL_WARN` is the int `16` in `okf.py`, so the message shows `16`; Rust's `{}` of `16.0_f64` prints `16`, which matches.
- `index.md` files never reach the concept checks (`continue` in `okf.py`).

If `mini_catalog_matches_okf_py` fails only on the `frontmatter-parse` message, the YAML parser's message text differs from `okf.py`'s: fix the message in `src/okf/yaml.rs` (it must be byte-identical), not the test.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib analysis::lint`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

```bash
git add src/okf/analysis/ changelog/
git commit -m "analysis: lint with commit times as input (port of okf.py lint)"
```

---

### Task 11: `okf::search`

Ranked search within one snapshot, as decision 3 defines it. M3's `search` tool runs it per selected catalog and merges hits by `SearchHit::rank` then path.

**Files:**
- Create: `src/okf/search.rs`
- Modify: `src/okf/mod.rs` (`pub mod search;`)
- Modify: `src/okf/snapshot.rs` (each concept carries its `SearchFields`)
- Create: `changelog/<ts>-m2-task-11-search.md`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn snapshot() -> Snapshot {
        Snapshot::build(&MemWorkspace::from_files([
            (
                "infra/time-server.md",
                "---\ntype: Service\ntitle: GPS time server\ndescription: Stratum 1 NTP.\n\
                 aliases: [NTP server]\ntags: [ntp, time]\n---\nServes time to the LAN.\n",
            ),
            (
                "infra/clock.md",
                "---\ntype: Host\ntitle: Wall clock\ndescription: Shows the time server's time.\n\
                 tags: [Time]\n---\nA clock.\n",
            ),
            (
                "infra/old-ntp.md",
                "---\ntype: Service\ntitle: Old NTP server\nstatus: deprecated\n---\nRetired.\n",
            ),
            (
                "notes/body.md",
                "---\ntype: Note\ntitle: Notes\n---\nfirst line\nWe asked the TIME SERVER twice.\n",
            ),
            ("index.md", "time server\n"),
        ]))
        .unwrap()
    }

    fn paths(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.path.as_str()).collect()
    }

    #[test]
    fn ranks_by_field_then_deprecated_last() {
        // The phrase is in time-server's title (tier 0), clock's description
        // (tier 2) and body.md's body (tier 3); old-ntp has no "time" at all.
        let hits = search(&snapshot(), &SearchQuery::new("time server"));
        assert_eq!(paths(&hits), ["/infra/time-server.md", "/infra/clock.md", "/notes/body.md"]);
        let tiers: Vec<u8> = hits.iter().map(|h| h.rank.tier).collect();
        assert_eq!(tiers, [0, 2, 3]);
        // Both match "ntp server" in a name (tier 0); the deprecated one sorts last.
        let hits = search(&snapshot(), &SearchQuery::new("ntp server"));
        assert_eq!(paths(&hits), ["/infra/time-server.md", "/infra/old-ntp.md"]);
        assert!(hits[1].rank.deprecated && hits[1].rank.tier == 0);
    }

    #[test]
    fn every_term_must_match_somewhere() {
        assert!(search(&snapshot(), &SearchQuery::new("time banana")).is_empty());
        assert_eq!(paths(&search(&snapshot(), &SearchQuery::new("lan gps"))), ["/infra/time-server.md"]);
    }

    #[test]
    fn snippets_show_the_first_matching_body_line() {
        let hits = search(&snapshot(), &SearchQuery::new("time server"));
        let body = hits.iter().find(|h| h.path == "/notes/body.md").unwrap();
        assert_eq!(body.snippet.as_deref(), Some("We asked the TIME SERVER twice."));
        assert_eq!(hits[0].title.as_deref(), Some("GPS time server"));
    }

    #[test]
    fn filters_and_limit() {
        let mut query = SearchQuery::new("");
        query.tags = vec!["TIME".to_string()];
        assert_eq!(paths(&search(&snapshot(), &query)), ["/infra/clock.md", "/infra/time-server.md"]);
        let mut query = SearchQuery::new("server");
        query.concept_type = Some("service".to_string());
        query.status = Some("stable".to_string());
        assert_eq!(paths(&search(&snapshot(), &query)), ["/infra/time-server.md"]);
        let mut query = SearchQuery::new("time");
        query.limit = 1;
        assert_eq!(search(&snapshot(), &query).len(), 1);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib search`
Expected: compile errors.

- [ ] **Step 3: Implement `src/okf/search.rs`**

```rust
//! Ranked full-text search within one catalog snapshot.
//!
//! A concept matches when every whitespace-separated term of the query
//! occurs (case-insensitively) in at least one of its fields. Fields, best
//! first: title or alias, tag, description, body. The tier is the best field
//! holding the whole query (terms joined by one space), else 4 plus the
//! best field holding any term. Deprecated concepts sort after the rest;
//! ties break by path.

use std::cmp::Ordering;

use rayon::prelude::*;
use serde::Serialize;

use crate::okf::markdown::split_frontmatter;
use crate::okf::pystr::py_strip;
use crate::okf::snapshot::{Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// Snippets are cut to this many characters.
const SNIPPET_LEN: usize = 160;
/// Number of ranked fields.
const FIELDS: usize = 4;

/// A concept's lowercased search fields, built once per snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFields {
    /// Title and aliases.
    names: Vec<String>,
    tags: Vec<String>,
    description: String,
    body: String,
    concept_type: String,
    status: String,
}

impl SearchFields {
    /// Builds the fields from a parsed concept.
    pub(crate) fn of(doc: &SnapshotDoc) -> Self {
        let data = doc.frontmatter().data();
        let text = |key: &str| {
            data.and_then(|d| d.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
        };
        let tags = match data.and_then(|d| d.get("tags")) {
            Some(Value::Seq(items)) => items.iter().filter_map(Value::as_str).map(str::to_lowercase).collect(),
            Some(Value::Str(tag)) => vec![tag.to_lowercase()],
            Some(Value::Null | Value::Map(_)) | None => Vec::new(),
        };
        let names = doc
            .title()
            .into_iter()
            .chain(doc.aliases().iter().map(String::as_str))
            .map(str::to_lowercase)
            .collect();
        let status = text("status");
        Self {
            names,
            tags,
            description: text("description"),
            body: split_frontmatter(doc.text()).1.to_lowercase(),
            concept_type: text("type"),
            status: if status.is_empty() { "stable".to_string() } else { status },
        }
    }

    /// Does field `index` (0 = names, 1 = tags, 2 = description, 3 = body)
    /// contain `needle`?
    fn contains(&self, index: usize, needle: &str) -> bool {
        match index {
            0 => self.names.iter().any(|n| n.contains(needle)),
            1 => self.tags.iter().any(|t| t.contains(needle)),
            2 => self.description.contains(needle),
            _ => self.body.contains(needle),
        }
    }
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Free text; empty matches every concept that passes the filters.
    pub text: String,
    /// Only concepts of this `type` (case-insensitive).
    pub concept_type: Option<String>,
    /// Only concepts carrying every one of these tags (case-insensitive).
    pub tags: Vec<String>,
    /// Only concepts with this `status` (missing counts as `stable`).
    pub status: Option<String>,
    /// At most this many hits.
    pub limit: usize,
}

impl SearchQuery {
    /// A query for `text` with no filters and the design's default limit (20).
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            concept_type: None,
            tags: Vec::new(),
            status: None,
            limit: 20,
        }
    }
}

/// How a hit ranks; `Ord` sorts best first (derived: field order matters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchRank {
    /// Deprecated concepts sort after all others (`false < true`).
    pub deprecated: bool,
    /// 0-3 for the field holding the whole query, 4-7 for the field holding a term.
    pub tier: u8,
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    /// `/`-prefixed concept path.
    pub path: String,
    /// The concept's title.
    pub title: Option<String>,
    /// Its description.
    pub description: Option<String>,
    /// The first body line with a hit, stripped and cut to 160 characters.
    pub snippet: Option<String>,
    /// The ranking key (M3 merges catalogs by it); not serialized.
    #[serde(skip)]
    pub rank: SearchRank,
}

/// Searches the concepts of one snapshot.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `query` - text, filters and limit.
///
/// # Returns
/// At most `query.limit` hits, best first.
pub fn search(snapshot: &Snapshot, query: &SearchQuery) -> Vec<SearchHit> {
    let terms: Vec<String> = query.text.split_whitespace().map(str::to_lowercase).collect();
    let phrase = terms.join(" ");
    let wanted_tags: Vec<String> = query.tags.iter().map(|t| t.to_lowercase()).collect();
    let wanted_type = query.concept_type.as_deref().map(str::to_lowercase);
    let wanted_status = query.status.as_deref().map(str::to_lowercase);

    let mut ranked: Vec<(SearchRank, &SnapshotDoc)> = snapshot
        .docs()
        .par_iter()
        .filter_map(|doc| {
            let fields = doc.search_fields()?;
            if wanted_type.as_ref().is_some_and(|t| *t != fields.concept_type)
                || wanted_status.as_ref().is_some_and(|s| *s != fields.status)
                || !wanted_tags.iter().all(|t| fields.tags.contains(t))
            {
                return None;
            }
            let tier = tier(fields, &terms, &phrase)?;
            Some((
                SearchRank {
                    deprecated: fields.status == "deprecated",
                    tier,
                },
                doc,
            ))
        })
        .collect();

    let order = |a: &(SearchRank, &SnapshotDoc), b: &(SearchRank, &SnapshotDoc)| -> Ordering {
        a.0.cmp(&b.0).then_with(|| a.1.path().cmp(b.1.path()))
    };
    // Only the best `limit` need sorting: partition around the cut first.
    if ranked.len() > query.limit {
        if query.limit == 0 {
            return Vec::new();
        }
        ranked.select_nth_unstable_by(query.limit - 1, order);
        ranked.truncate(query.limit);
    }
    ranked.sort_unstable_by(order);
    ranked
        .into_iter()
        .map(|(rank, doc)| hit(doc, rank, &terms, &phrase))
        .collect()
}

/// The tier, or `None` when some term occurs in no field.
fn tier(fields: &SearchFields, terms: &[String], phrase: &str) -> Option<u8> {
    if terms.is_empty() {
        return Some(0);
    }
    let every_term = terms
        .iter()
        .all(|term| (0..FIELDS).any(|field| fields.contains(field, term)));
    if !every_term {
        return None;
    }
    let best = (0..FIELDS)
        .find(|&field| fields.contains(field, phrase))
        .or_else(|| {
            (0..FIELDS)
                .find(|&field| terms.iter().any(|t| fields.contains(field, t)))
                .map(|field| FIELDS + field)
        })?;
    // At most 7, so the cast cannot truncate.
    Some(best as u8)
}

fn hit(doc: &SnapshotDoc, rank: SearchRank, terms: &[String], phrase: &str) -> SearchHit {
    let data = doc.frontmatter().data();
    let description = data
        .and_then(|d| d.get("description"))
        .and_then(Value::as_str)
        .map(str::to_string);
    SearchHit {
        path: format!("/{}", doc.path()),
        title: doc.title().map(str::to_string),
        description,
        snippet: snippet(split_frontmatter(doc.text()).1, terms, phrase),
        rank,
    }
}

/// The first body line holding the phrase, else the first holding any term.
fn snippet(body: &str, terms: &[String], phrase: &str) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let lines: Vec<(&str, String)> = body.split('\n').map(|l| (l, l.to_lowercase())).collect();
    let found = lines
        .iter()
        .find(|(_, lower)| lower.contains(phrase))
        .or_else(|| lines.iter().find(|(_, lower)| terms.iter().any(|t| lower.contains(t))))?;
    let text = py_strip(found.0);
    Some(text.char_indices().nth(SNIPPET_LEN).map_or(text, |(cut, _)| &text[..cut]).to_string())
}
```

> `fields.contains` takes `_` for the body arm because `index` only ever comes from `0..FIELDS`; that is the one catch-all, and it is covered by the tests.

- [ ] **Step 4: Give each concept its fields**

In `src/okf/snapshot.rs`: add `search: Option<crate::okf::search::SearchFields>` to `SnapshotDoc`, with:

```rust
    /// The search fields; `Some` for concepts only.
    pub fn search_fields(&self) -> Option<&crate::okf::search::SearchFields> {
        self.search.as_ref()
    }
```

and fill it at the end of `read_document`, once the rest of the document is built:

```rust
    let mut document = SnapshotDoc {
        // ... the fields as before ...
        search: None,
    };
    if document.kind == DocKind::Concept {
        document.search = Some(crate::okf::search::SearchFields::of(&document));
    }
    Ok(document)
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib search snapshot`
Expected: PASS.

- [ ] **Step 6: Gates, changelog, commit**

```bash
git add src/okf/search.rs src/okf/mod.rs src/okf/snapshot.rs changelog/
git commit -m "search: ranked search within a snapshot"
```

---

### Task 12: Parity for the analyses

Extends M1's harness: the shim gets an `analysis` command that runs `okf.py`'s `links`, `oldest`, `sprawl` and `lint` with a frozen clock and injected commit times. Goldens cover the synthetic corpus and a secrets corpus built at runtime; live tests also cover the real catalog with real git.

**Files:**
- Modify: `tests/parity/okf_shim.py`
- Modify: `tests/parity.rs`
- Create: `tests/fixtures/catalog/analysis/**`, `tests/fixtures/commit-times.json`; modify `tests/fixtures/catalog/index.md`
- Regenerate: `tests/golden/*.json` (the new fixtures change `frontmatter.json` and `markdown.json` too); create `tests/golden/analysis.json`, `tests/golden/secrets.json`
- Create: `changelog/<ts>-m2-task-12-parity.md`

- [ ] **Step 1: Create the fixtures**

Run this script from the repository root (it is not committed; the files it writes are). It was checked on 2026-10-27: `okf.py` runs every analysis on the resulting corpus without error, and the corpus exercises every lint rule except the secret rules (which live in the runtime corpus, Step 4): broken links (missing, escaping the root, an existing text file, a dot-directory target), broken and percent-encoded anchors, relative links, orphans, dead ends, index drift (unlisted, misfiled under `# Sections`, a folder index with frontmatter), an unlinked mention (masked in code, URL, fence and footnote), a sprawl warning, an exempt backlog, `oldest`'s fallback for unparseable frontmatter, a tie between equal instants with different offsets, and every timestamp rule.

````bash
#!/usr/bin/env bash
# Creates the M2 analysis fixtures. Run from the repository root.
set -euo pipefail
C=tests/fixtures/catalog
A=$C/analysis
mkdir -p "$A/sub" "$A/.hidden"

printf -- '---\nokf_version: "0.2"\n---\n\n# Fixture catalog\n\n* [Infra](infra/) - systems\n* [Analysis](analysis/) - analysis cases\n' > "$C/index.md"

cat > "$A/index.md" <<'EOF'
# Analysis

# Concepts

* [Hub](/analysis/hub.md) - links out
* [Lead](/analysis/lead.md) - a sprawling concept
* [Stamps](/analysis/stamps.md) - timestamp cases
* [Backlog](/analysis/backlog.md) - exempt from sprawl
* [Offsets](/analysis/offsets.md) - non-UTC stamps

# Sections

* [Sub](/analysis/sub/) - a folder
* [Hub again](/analysis/hub.md) - a concept misfiled under Sections
EOF

cat > "$A/hub.md" <<'EOF'
---
type: Hub
title: Analysis Hub
description: Links of every kind.
aliases: [the hub]
tags: [analysis, links]
generated: { by: fixture, at: 2026-10-20T00:00:00Z }
---
# Links

- [lead](/analysis/lead.md) and [its history](/analysis/lead.md#history)
- [a missing anchor](/analysis/lead.md#no-such-heading)
- [an encoded anchor](/analysis/lead.md#Open%20items) and [a good one](/analysis/lead.md#open-items)
- [relative](stamps.md), [missing](/analysis/missing.md), [folder](/analysis/sub/)
- [up and out](../../outside.md), [in page](#links), [bad in page](#nowhere)
- [external](https://example.com/x) and ![an image](/analysis/missing.png)
- [a text file](/analysis/diagram.txt) and [hidden](/analysis/.hidden/notes.md#hidden-heading)

The Offsets concept explains non-UTC stamps; `Offsets` in code and
https://example.com/Offsets in a URL do not count.

```
Offsets inside a fence do not count either.
```

[^1]: Offsets in a footnote do not count.
EOF

{
cat <<'EOF'
---
type: Service
title: Lead Concept
status: draft
sources:
  - { id: s1, resource: a }
  - { id: s2, resource: b }
  - { id: s3, resource: c }
  - { id: s4, resource: d }
  - { id: s5, resource: e }
---
# Lead

**As of 2026-10-20** the service runs on two hosts.
**Owner** is the fixture.

EOF
for i in $(seq 1 40); do echo "Overview line $i."; done
echo; echo "## History"; echo
for i in $(seq 1 30); do echo "In 2026-0$(( (i % 9) + 1 ))-1$(( i % 10 )) something happened ($i)."; done
echo; echo "## Open items"; echo
for i in $(seq 1 10); do echo "- item $i"; done
echo; echo "## Incident on Oct 5, 2026"; echo
for i in $(seq 1 20); do echo "Step $i of the incident."; done
echo; echo '```'; echo "2025-01-01 inside a fence"; echo '```'
echo; echo "## Ports"; echo; echo "- 22"; echo "- 443"
} > "$A/lead.md"

cat > "$A/stamps.md" <<'EOF'
---
type: Note
title: Stamp Rules
status: bogus
generated: { by: fixture, at: 2027-01-01T00:00:00Z }
verified:
  - { by: fixture, at: "2026-10-01T10:00:00-04:00" }
  - plain
reviewed: { by: fixture, at: 2026-10-02T00:00:00Z, outcome: maybe }
stale_after: 2026-01-01T00:00:00Z
open_questions:
  - Is the clock right?
  - { asked: by fixture }
sources:
  - { id: a }
  - { id: b, resource: r, last_modified: 2026-13-01T00:00:00Z }
  - { id: c, resource: r, last_modified: "2026-10-01T00:00:00+00:00" }
---
Timestamps of every shape. See [the hub](/analysis/hub.md).
EOF

{
printf -- '---\ntype: Backlog\ntitle: Fixture backlog\n---\n# Items\n\n'
for i in $(seq 1 400); do echo "- backlog item $i"; done
} > "$A/backlog.md"

cat > "$A/offsets.md" <<'EOF'
---
type: Note
title: Offsets
description: Non-UTC and fractional stamps.
generated: { by: fixture, at: "2026-10-26T10:00:00.5+05:30" }
reviewed: { by: fixture, at: 2026-10-26T04:30:00.5Z, outcome: verified, note: same instant }
stale_after: 2026-11-01T00:00:00Z
---
Back to [the hub](/analysis/hub.md).
EOF

cat > "$A/broken-stamps.md" <<'EOF'
---
type: Note
title: Broken stamps
title: duplicated
generated: { by: fixture, at: 2026-09-01T00:00:00Z }
stale_after: 2026-01-01T00:00:00Z
---
Unparseable frontmatter; oldest falls back to a regex scan.
EOF

printf -- '---\nnote: folder indexes must not have frontmatter\n---\n* [Deep](/analysis/sub/deep.md)\n' > "$A/sub/index.md"
printf -- '---\ntype: Note\ntitle: Deep\n---\nA concept in a sub-folder, linking [up](/analysis/hub.md).\n' > "$A/sub/deep.md"
printf -- '# Hidden heading\n\nIn a dot-directory: not a document, but a link target.\n' > "$A/.hidden/notes.md"
printf -- 'a plain text file\n' > "$A/diagram.txt"

cat > tests/fixtures/commit-times.json <<'EOF'
{
  "times": {
    "analysis/hub.md": "2026-10-19T00:00:00Z",
    "analysis/offsets.md": "2026-10-27T00:00:00Z",
    "analysis/stamps.md": "2026-10-01T00:00:00Z"
  },
  "dirty": ["analysis/stamps.md"]
}
EOF
````

None of these is a documented divergence (fixture rule in `tests/parity.rs`).

- [ ] **Step 2: Add the shim command**

In `tests/parity/okf_shim.py`, extend the usage text:

```
    okf_shim.py analysis ROOT NOW COMMITS
                                     links, oldest, sprawl and lint --json for ROOT with
                                     now_utc frozen at NOW; COMMITS is a commit-times JSON
                                     file, "none" (git unavailable) or "git" (real git)
```

and add (new imports: `contextlib`, `io`):

```python
def analysis(okf: ModuleType, root: str, now: str, commits: str) -> Json:
    """Run the analysis commands as okf.py's CLI does, returning their JSON."""
    fixed = okf.parse_ts(now)
    okf.now_utc = lambda: fixed
    if commits == "none":
        okf.last_commit_times = lambda _root: (None, set())
    elif commits != "git":
        with open(commits, encoding="utf-8") as fh:
            data = json.load(fh)
        times = {path: okf.parse_ts(at) for path, at in data["times"].items()}
        dirty = set(data["dirty"])
        okf.last_commit_times = lambda _root: (times, dirty)

    def run(*argv: str) -> Json:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            try:
                okf.main([*argv, "--root", root])
            except SystemExit:  # lint exits 1 when there are errors
                pass
        result: Json = json.loads(out.getvalue())
        return result

    return {
        "links": run("links", "--json"),
        "oldest": run("oldest", "--json", "-n", "1000000"),
        "oldest_open_questions": run("oldest", "--json", "--open-questions"),
        "sprawl": run("sprawl", "--json", "-n", "1000000"),
        "lint": run("lint", "--json"),
    }
```

with the dispatch branch:

```python
    elif command == "analysis":
        if len(args) != 3:
            usage("analysis takes ROOT NOW COMMITS")
        result = analysis(load_okf(), *args)
```

`okf.py`'s lint skips its own file. With `OKF_PY=$OKF_PARITY_CATALOG/tools/okf.py` (the default) that is the catalog's `tools/okf.py`, which the port skips too. Point `OKF_PY` at that same catalog's copy when running the live tests.

Run `mypy --strict tests/parity/okf_shim.py` if `mypy` is installed (M1 typed the shim).

- [ ] **Step 3: The Rust side of the analysis JSON**

In `tests/parity.rs` (new imports: `chrono::{DateTime, Utc}`, `okf_mcp::okf::analysis::{links::links_report, lint::{lint, CommitTimes}, oldest::{oldest, OldestQuery}, sprawl::{sprawl, SprawlQuery}}`, `okf_mcp::okf::snapshot::Snapshot`, `okf_mcp::okf::timestamp::parse_iso`, `std::collections::{HashMap, HashSet}`):

```rust
const COMMIT_TIMES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/commit-times.json");
/// The frozen "now" of the analysis goldens.
const FIXED_NOW: &str = "2026-10-27T12:00:00Z";

fn instant(text: &str) -> DateTime<Utc> {
    parse_iso(text)
        .and_then(|t| t.aware())
        .unwrap_or_else(|| panic!("{text} is an aware timestamp"))
        .with_timezone(&Utc)
}

fn commit_times_from_file(path: &str) -> CommitTimes {
    let data: Json = serde_json::from_str(&std::fs::read_to_string(path).expect("read commit times"))
        .expect("commit times are JSON");
    let times: HashMap<String, _> = data["times"]
        .as_object()
        .expect("times is an object")
        .iter()
        .map(|(path, at)| (path.clone(), instant(at.as_str().expect("a string")).fixed_offset()))
        .collect();
    let dirty: HashSet<String> = data["dirty"]
        .as_array()
        .expect("dirty is a list")
        .iter()
        .map(|p| p.as_str().expect("a string").to_string())
        .collect();
    CommitTimes::new(times, dirty)
}

/// `okf.py`'s `last_commit_times(root)`, run through git the same way.
fn git_commit_times(root: &Path) -> Option<CommitTimes> {
    let git = |args: &[&str]| {
        Command::new("git").arg("-C").arg(root).args(args).output().ok().filter(|o| o.status.success())
    };
    let log = git(&["log", "--format=%x00%cI", "--name-only", "--no-renames"])?;
    let mut times = HashMap::new();
    let mut current = None;
    for line in String::from_utf8_lossy(&log.stdout).split('\n') {
        if let Some(stamp) = line.strip_prefix('\0') {
            current = parse_iso(stamp).and_then(|t| t.aware());
        } else if let (false, Some(at)) = (line.trim().is_empty(), current) {
            times.entry(line.trim().to_string()).or_insert(at);
        }
    }
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    let dirty = String::from_utf8_lossy(&status.stdout)
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.get(3..))
        .map(|rest| rest.rsplit(" -> ").next().unwrap_or(rest).trim_matches('"').to_string())
        .collect();
    Some(CommitTimes::new(times, dirty))
}

fn rust_analysis(root: &Path, now: DateTime<Utc>, commits: Option<&CommitTimes>) -> Json {
    let workspace = DiskWorkspace::new(root).expect("corpus root exists");
    let snapshot = Snapshot::build(&workspace).expect("snapshot builds");
    let all = OldestQuery { n: 1_000_000, open_questions_only: false };
    let questions = OldestQuery { n: 5, open_questions_only: true };
    let rows = sprawl(&snapshot, SprawlQuery { concept: None, n: 1_000_000 }).expect("sprawl");
    let value = |v: serde_json::Result<Json>| v.expect("report serializes");
    json!({
        "links": value(serde_json::to_value(links_report(&snapshot, true))),
        "oldest": value(serde_json::to_value(oldest(&snapshot, now, all))),
        "oldest_open_questions": value(serde_json::to_value(oldest(&snapshot, now, questions))),
        "sprawl": value(serde_json::to_value(rows)),
        "lint": value(serde_json::to_value(lint(&snapshot, now, commits))),
    })
}

fn python_analysis(root: &Path, now: &str, commits: &str) -> Json {
    shim(
        &[OsStr::new("analysis"), root.as_os_str(), OsStr::new(now), OsStr::new(commits)],
        None,
    )
}

/// Recursive comparison that names the JSON path of the first difference
/// and checks object key order (serde_json's `Map` equality ignores it).
fn assert_json_equal(rust: &Json, python: &Json, what: &str) {
    match (rust, python) {
        (Json::Object(r), Json::Object(p)) => {
            let (rk, pk): (Vec<&String>, Vec<&String>) = (r.keys().collect(), p.keys().collect());
            assert_eq!(rk, pk, "{what}: keys or key order (Rust left, okf.py right)");
            for (key, value) in r {
                assert_json_equal(value, &p[key], &format!("{what}.{key}"));
            }
        }
        (Json::Array(r), Json::Array(p)) => {
            for (i, (a, b)) in r.iter().zip(p).enumerate() {
                assert_json_equal(a, b, &format!("{what}[{i}]"));
            }
            assert_eq!(r.len(), p.len(), "{what}: length (Rust left, okf.py right)");
        }
        _ => assert_eq!(rust, python, "{what} (Rust left, okf.py right)"),
    }
}
```

- [ ] **Step 4: The runtime secrets corpus**

Secret-shaped strings never go into a committed file (GitHub push protection, and the catalog's own lint). This corpus is written to a temp directory at test time:

```rust
/// A corpus holding one hit for every secret rule, plus near misses.
fn secrets_corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let github = format!("gh{}_{}", "p", "A1".repeat(18));
    let pat = format!("github_{}_{}", "pat", "B2".repeat(11));
    let slack = format!("xo{}-{}", "xb", "1234567890".repeat(2));
    let sk = format!("s{}-{}", "k", "C3".repeat(12));
    let aws = format!("AK{}{}", "IA", "Z".repeat(16));
    let pem = format!("-----BEGIN {} PRIVATE KEY-----", "OPENSSH");
    let value = format!("Ab{}", "c1".repeat(4));
    let files = [
        ("index.md", "---\nokf_version: \"0.2\"\n---\n* [Keys](/keys.md)\n".to_string()),
        (
            "keys.md",
            format!(
                "---\ntype: Note\ntitle: Keys\ndescription: d\n---\n{github}\n{pat}\n{slack}\n{sk}\n\
                 id {aws}\n{pem}\npassword: \"{value}\" token={value} secret: changeme\n\
                 x{github}\n{aws}Q\n"
            ),
        ),
        ("notes.txt", format!("api_key = {value}\n")),
        (".claude/settings.md", format!("Client_Secret: {value}\n")),
    ];
    for (path, contents) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(full, contents).expect("write corpus file");
    }
    // Not UTF-8: both implementations skip it.
    std::fs::write(dir.path().join("blob.bin"), [0xff, 0xfe, 0x00]).expect("write blob");
    dir
}
```

- [ ] **Step 5: Golden tests**

```rust
#[test]
fn analysis_matches_golden() {
    let commits = commit_times_from_file(COMMIT_TIMES);
    assert_json_equal(
        &rust_analysis(Path::new(FIXTURES), instant(FIXED_NOW), Some(&commits)),
        &read_golden("analysis.json"),
        "analysis",
    );
}

#[test]
fn secrets_match_golden() {
    let corpus = secrets_corpus();
    let rust = rust_analysis(corpus.path(), instant(FIXED_NOW), None);
    assert_json_equal(&rust["lint"], &read_golden("secrets.json"), "secrets lint");
}
```

- [ ] **Step 6: Live tests**

```rust
#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_analysis_parity() {
    let fixtures = Path::new(FIXTURES);
    let commits = commit_times_from_file(COMMIT_TIMES);
    assert_json_equal(
        &rust_analysis(fixtures, instant(FIXED_NOW), Some(&commits)),
        &python_analysis(fixtures, FIXED_NOW, COMMIT_TIMES),
        "analysis of the synthetic corpus",
    );
    let corpus = secrets_corpus();
    assert_json_equal(
        &rust_analysis(corpus.path(), instant(FIXED_NOW), None)["lint"],
        &python_analysis(corpus.path(), FIXED_NOW, "none")["lint"],
        "lint of the secrets corpus",
    );
    let Some(real) = std::env::var_os("OKF_PARITY_CATALOG").map(PathBuf::from) else {
        return;
    };
    // One shared "now", to the second, for both sides.
    let now = instant(&okf_mcp::clock::format_timestamp(Utc::now()));
    let started = std::time::Instant::now();
    let rust = rust_analysis(&real, now, git_commit_times(&real).as_ref());
    eprintln!("Rust analysis of {} took {:?}", real.display(), started.elapsed());
    assert_json_equal(
        &rust,
        &python_analysis(&real, &okf_mcp::clock::format_timestamp(now), "git"),
        &format!("analysis of {}", real.display()),
    );
}
```

Add both new goldens to `regenerate_goldens`' `files` array:

```rust
        (
            "analysis.json",
            pretty(&python_analysis(root, FIXED_NOW, COMMIT_TIMES)),
        ),
        (
            "secrets.json",
            pretty(&python_analysis(secrets_corpus().path(), FIXED_NOW, "none")["lint"]),
        ),
```

- [ ] **Step 7: Regenerate and run**

```bash
UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens
cargo test --test parity
OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test parity -- --ignored
```

Expected: all pass. Review `git diff tests/golden/frontmatter.json tests/golden/markdown.json`: the only changes should be the new `analysis/` entries and the root `index.md`. A live failure is a port bug (or a new divergence): fix the port; if it really is a divergence, document it in the module and in this plan's "Deviations" list, and remove the fixture that shows it. Note the real-catalog timing from the `eprintln!` in the PR.

- [ ] **Step 8: Gates, changelog, commit**

```bash
git add tests/ changelog/
git commit -m "parity: analysis goldens and live parity for links, oldest, sprawl, lint and secrets"
```

---

### Task 13: Tighten visibility (M1 follow-up)

The M1 final review listed public items that only the crate itself uses. With M2's API in place, the crate's real surface is visible: `tests/parity.rs`, the doc tests and M3's planned tool layer (snapshot, analyses, search, workspace, doc, yaml values, clock, timestamp).

**Files:**
- Modify: `Cargo.toml` (`[features] test-util = []`)
- Modify: `src/okf/mod.rs`, `src/okf/workspace.rs`, `src/okf/pystr.rs`, `src/okf/yaml.rs`, `src/okf/doc.rs`
- Modify: `tests/parity.rs` only if it used something that is now crate-private
- Create: `changelog/<ts>-m2-task-13-visibility.md`

- [ ] **Step 1: Gate `MemWorkspace`**

`Cargo.toml`:

```toml
[features]
# Exposes test doubles (MemWorkspace) to integration tests and dependents' tests.
test-util = []
```

In `src/okf/workspace.rs`, put `#[cfg(any(test, feature = "test-util"))]` on `struct MemWorkspace`, its `impl` blocks and the helpers only it uses (`has_key_with_prefix`, `in_dot_directory`, `in_skipped_directory`). `cfg(test)` covers the crate's own unit tests, which is where every use is today.

- [ ] **Step 2: Make crate-internal helpers `pub(crate)`**

| Item | Change |
|---|---|
| `okf::pystr` (module) | `pub(crate) mod pystr;` in `src/okf/mod.rs`; its items stay `pub` inside (visible crate-wide only) |
| `yaml::parse_value`, `yaml::NumberedLine` | `pub(crate)` |
| `Doc::frontmatter` (the raw frontmatter lines) | `pub(crate)` (only `analysis::oldest` reads it) |

A doc test is compiled as an external crate, so an example on a now-private item no longer compiles: move each such example (`pystr::universal_newlines`, `pystr::py_repr_str`, any in `yaml::parse_value`) into the module's unit tests, keeping the assertion.

- [ ] **Step 3: Find what else is unused outside the crate**

Run `cargo check --all-targets` after each change. Any item that `tests/parity.rs`, a doc test or M3's planned use needs stays `pub`. If `cargo clippy --all-targets -- -D warnings` then reports an item as dead code, it was used by nothing: delete it rather than silencing the lint (no unused code).

- [ ] **Step 4: Gates, changelog, commit**

Run the gates and the live parity tests. Commit:

```bash
git add Cargo.toml src/ tests/ changelog/
git commit -m "Tighten visibility: MemWorkspace behind test-util, internal helpers pub(crate)"
```

---

### Task 14: Smoke test, README, design update and milestone changelog

**Files:**
- Create: `tests/smoke.rs`
- Create: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`
- Create: `changelog/<ts>-m2-analysis.md`

- [ ] **Step 1: The smoke test**

`tests/smoke.rs` runs the whole M2 surface on a real catalog in one go and prints timings. It is `#[ignore]`d (it needs a checkout) and runs with the live parity tests:

```rust
//! End-to-end smoke test of the M2 analysis surface on a real catalog.
//!
//! `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`

use std::time::Instant;

use chrono::Utc;
use okf_mcp::okf::analysis::{
    links::links_report,
    lint::lint,
    oldest::{oldest, OldestQuery},
    sprawl::{sprawl, SprawlQuery},
};
use okf_mcp::okf::search::{search, SearchQuery};
use okf_mcp::okf::snapshot::Snapshot;
use okf_mcp::okf::workspace::DiskWorkspace;

#[test]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG"]
fn every_analysis_runs_on_a_real_catalog() {
    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let workspace = DiskWorkspace::new(root).expect("catalog root exists");

    let started = Instant::now();
    let snapshot = Snapshot::build(&workspace).expect("snapshot builds");
    eprintln!("snapshot: {} documents in {:?}", snapshot.docs().len(), started.elapsed());

    let now = Utc::now();
    let timed = |name: &str, run: &dyn Fn() -> usize| {
        let started = Instant::now();
        let count = run();
        eprintln!("{name}: {count} in {:?}", started.elapsed());
    };
    timed("links (with mentions)", &|| links_report(&snapshot, true).summary.unlinked_mentions);
    timed("lint findings", &|| {
        let report = lint(&snapshot, now, None);
        report.errors.len() + report.warnings.len()
    });
    timed("oldest rows", &|| oldest(&snapshot, now, OldestQuery::default()).len());
    timed("sprawl rows", &|| sprawl(&snapshot, SprawlQuery { concept: None, n: 5 }).expect("sprawl").len());
    timed("search hits", &|| search(&snapshot, &SearchQuery::new("time server")).len());

    let hits = search(&snapshot, &SearchQuery::new("time server"));
    assert_eq!(
        hits.first().map(|h| h.path.as_str()),
        Some("/infra/time-server.md"),
        "the time server concept has 'time server' as an alias"
    );
}
```

Run it and paste the timings into the PR and the milestone changelog. The target is a snapshot build plus lint well under a second in release mode (`okf.py lint` takes 0.29 s on the same catalog, verified fact 9); report the real numbers whether or not they meet it.

- [ ] **Step 2: README**

Create `README.md`:

```markdown
# okf_mcp

A Rust port of `tools/okf.py`, the Open Knowledge Format catalog CLI, on its
way to becoming a shared MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`).

Status: milestones 1 (core library) and 2 (analysis) are done. The library
reads a catalog into an in-memory `Snapshot` and runs `okf.py`'s analyses
on it with identical JSON output: `lint`, the links report, `oldest`,
`sprawl` and the secret scan, plus ranked `search`. There is no server yet
(milestone 3).

## Layout

- `src/okf/` - the document model (`yaml`, `doc`, `markdown`, `paths`,
  `workspace`, `timestamp`), the `snapshot`, `analysis::*` and `search`.
- `src/clock.rs`, `src/error.rs` - the time source and the error type.
- `tests/parity.rs` - differential tests against `okf.py`.
- `changelog/` - one dated note per change.

## Testing

    cargo test                                   # unit, doc and golden tests
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test parity -- --ignored    # live parity against okf.py
    UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens

The live tests need `python3` and `okf.py` (`OKF_PY`, default
`~/repos/pg_knowledge_catalog/tools/okf.py`). The real catalog is read in
place and never copied into this repository.
```

- [ ] **Step 3: Update the design**

In `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`:

- Crate layout table: add rows for `okf::timestamp` (Python `fromisoformat`/`isoformat` port), `okf::snapshot` (the in-memory snapshot; `catalog` in M3 holds it behind `RwLock<Arc<_>>`) and `okf::search` (ranked search within one snapshot, decision 3 of the M2 plan). Note under `okf::analysis` that lint takes commit times as input.
- Milestones table: mark M1 and M2 as delivered, linking the M2 plan.
- "Tools / Read tools": `search` ranking follows the M2 plan's decision 3 (all terms must match; the tier is the best field holding the whole query, then the best holding a term).

- [ ] **Step 4: The milestone changelog**

Create `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m2-analysis.md` summarizing the milestone: Added (each module), Changed (`list_files`, `ScalarStyle`, repr-quoted YAML errors, visibility), divergences (the plan's list, plus any added during execution), the real test counts (unit, doc, golden, live) and the smoke-test timings, and every `> **Corrected (issue #N).**` note made to this plan.

- [ ] **Step 5: Final verification and commit**

Run every gate, the live parity tests and the smoke test on a fresh checkout of the branch. Then:

```bash
git add tests/smoke.rs README.md docs/ changelog/
git commit -m "M2: smoke test, README, design update and milestone changelog"
```

---

## Self-review (2026-10-27)

- **Design coverage (M2 row):** `lint` (T10), links report (T7), `oldest` (T8), `sprawl` (T9), secret scan (T5, run on every file by T6), snapshot (T6), search (T11). Supporting work: `list_files` (T1), Python semantics (T2, T3), parity (T12). Chosen follow-ups: T4, T13. Write-time secret refusal is M4 (deferred table).
- **Placeholders:** none; every code step has the code. Values in tests marked as `okf.py`'s were produced by running `okf.py` on 2026-10-27.
- **Names across tasks:** `Finding` (T5) is used by T6 and T10; `Snapshot`, `SnapshotDoc`, `SnapshotLink`, `Frontmatter`, `DocKind`, `fm_fallback_scalar` (T6) by T7-T11; `round_one` (T8) by T9; `sprawl_of`, `SPRAWL_WARN` (T9) and `links_report` (T7) by T10; `SearchFields::of` (T11) by T6's `read_document`; `parse_iso`, `IsoFormat`, `is_iso_timestamp` (T3) by T8, T10, T12; `py_repr`, `py_repr_opt`, `py_str`, `py_truthy`, `ignorecase_literal`, `py_unquote` (T2) by T7, T8, T10.
