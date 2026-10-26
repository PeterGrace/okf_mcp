# OKF MCP Server, Milestone 1 (Core Library): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port `okf.py`'s document model to Rust: the YAML-subset parser and renderer, byte-preserving frontmatter edits, markdown structure (headings, slugs, footnotes, links), catalog path rules and a filesystem abstraction. Prove parity with `okf.py` through differential tests.

**Architecture:** A library crate (`src/lib.rs`) beside the existing binary. `okf::*` is pure logic over a `Workspace` trait (disk and in-memory implementations) and a `Clock` trait, so it needs no git or disk in unit tests. A Python shim exposes `okf.py` internals as JSON. Parity tests compare the Rust output with it live, and with committed golden files that keep working after `okf.py` is retired.

**Tech Stack:** Rust 1.97 (edition 2021), `regex` 1.13, `fancy-regex` 0.20 (lookaround patterns), `indexmap` 2.14 (ordered maps), `chrono` 0.4 (UTC clock), `serde_json` 1.0, `thiserror` 2.0; dev: `hegeltest` 0.50 (property tests, imported as `hegel`), `tempfile` 3.27. Parity tests need `python3` and `okf.py`.

**Spec:** `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`

**Status of this code:** every file below was compiled and tested in a scratch copy of this repository before this plan was written. Results: `cargo fmt`, `cargo clippy --all-targets -- -D warnings` clean; 67 unit tests, 8 doc tests and 3 golden tests passing; the live parity tests passing against the real catalog (169 documents, 142 with frontmatter, all matching `okf.py`).

---

## Roadmap (one plan per milestone)

This plan covers **M1 only**. Each later milestone gets its own detailed plan when it starts, because each depends on facts the previous one establishes.

| Milestone | Delivers | Depends on |
|---|---|---|
| **M1. Core library** (this plan) | `okf::yaml`, `okf::doc`, `okf::markdown`, `okf::paths`, `okf::workspace`, `clock`, `error`, parity harness | |
| M2. Analysis | `lint`, links report, `oldest`, `sprawl`, secret scan; catalog snapshot and search | M1 |
| M3. Read-only server | axum + rmcp streamable HTTP, API-key auth, catalog selection, clone/refresh, read tools, `hash-key`. Must first confirm whether rmcp 3.8 exposes HTTP request parts to tool handlers (spec "Data flow / Read" step 2) | M2 |
| M4. Writes | Session worktrees, write tools, `publish`, `discard`, `pending_changes`, `stranded_sessions` | M3 |
| M5. Restructuring | `move_concept`, `split_concept`, `delete_concept` | M4 |
| M6. Deployment | Dockerfile, k8s manifests in `argo2025`, webhook, NTP check, operator docs | M5 |

## Conventions for every task

- Work on the existing branch `okf-mcp-server` in `~/repos/okf_mcp`.
- The user is a Python expert and Rust novice. Keep the comments that explain Rust-specific ideas (`Send + Sync`, `LazyLock`, `let ... else`, `?`), as the code below already does.
- No `.unwrap()` outside tests; `.expect("why this can't fail")` only for invariants (regex literals, poisoned locks).
- After each task: `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean before committing.
- Commit messages end with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Don't touch `src/main.rs` in M1. It is the template's tracing setup, and M3 rewrites it.

## File structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Adds `[lib]`, M1 dependencies, dev-dependencies |
| `.gitignore` | Adds `.hegel/` (Hegel's cache and failure database) |
| `src/lib.rs` | Library root: `clock`, `error`, `okf` |
| `src/error.rs` | `OkfError`, the crate's error enum (grows in later milestones) |
| `src/clock.rs` | `Clock` trait, `SystemClock`, `FixedClock`, `format_timestamp` |
| `src/okf/mod.rs` | OKF module root |
| `src/okf/yaml.rs` | `Value`, `YamlError`, YAML-subset parser and renderer |
| `src/okf/doc.rs` | `Doc`: frontmatter split, blocks, parse, textual edits, `append_seq_item` |
| `src/okf/paths.rs` | `normpath`, `dirname`/`basename`/`join`, `walk_order`, `is_concept`, `resolve_concept`, `new_concept_path` |
| `src/okf/workspace.rs` | `Workspace` trait, `DiskWorkspace`, `MemWorkspace` |
| `src/okf/markdown.rs` | Links, frontmatter/body splits, headings and slugs, anchors, footnotes, `resolve_link` |
| `tests/parity/okf_shim.py` | Exposes `okf.py` internals as JSON |
| `tests/fixtures/catalog/**` | Synthetic parity corpus (edge cases; no private content) |
| `tests/golden/*.json` | `okf.py`'s output for the synthetic corpus |
| `tests/parity.rs` | Golden tests (always run) and live parity tests (`--ignored`) |
| `changelog/<timestamp>-m1-core-library.md` | Iteration notes, as the project's CLAUDE.md requires |

---

### Task 1: Crate setup, error type and clock

**Files:**
- Modify: `Cargo.toml` (replace whole file)
- Modify: `.gitignore`
- Create: `src/lib.rs`, `src/error.rs`, `src/clock.rs`

- [ ] **Step 1: Replace `Cargo.toml`**

```toml
[package]
name = "okf_mcp"
version = "0.1.0"
edition = "2021"
authors = ["Peter Grace <pete.grace@gmail.com>"]

[lib]
name = "okf_mcp"
path = "src/lib.rs"

[[bin]]
name = "okf_mcp"
path = "src/main.rs"

[dependencies]
# Wall-clock timestamps for catalog stamps (UTC only; no local-time database needed).
chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }
ctrlc = { version = "3.4.5", features = ["termination"] }
# Lookaround regexes (okf.py's LINK_RE, slugify) that the `regex` crate can't express.
fancy-regex = "0.20.0"
# Insertion-ordered maps, matching Python dict order for frontmatter mappings.
indexmap = "2.14.2"
lazy_static = "1.5.0"
regex = "1.13.1"
serde = { version = "1.0.216", features = ["derive"] }
serde_json = "1.0.154"
thiserror = "2.0.6"
tokio = { version = "1.42.0", features = ["rt-multi-thread", "macros", "time"] }
tracing = "0.1.41"
tracing-log = "0.2.0"
tracing-subscriber = { version = "0.3.19", features = ["fmt", "env-filter"] }
console-subscriber = "0.4.1"
dotenv = "0.15.0"

[dev-dependencies]
# Property-based tests (Hypothesis-backed); the library is imported as `hegel`.
hegeltest = "0.50.3"
# Throwaway directories for DiskWorkspace tests.
tempfile = "3.27.0"
```

- [ ] **Step 2: Add `.hegel/` to `.gitignore`**

```
/target
.hegel/
```

- [ ] **Step 3: Create `src/lib.rs` with the first two modules**

```rust
//! okf_mcp: an MCP server for Open Knowledge Format (OKF) catalogs.
//!
//! The library holds everything except process startup: the OKF document
//! model (`okf`), errors, and the clock. `main.rs` only wires it together.

pub mod clock;
pub mod error;
```

- [ ] **Step 4: Create `src/error.rs`**

```rust
//! The crate's error type.

/// Errors from reading, parsing or editing an OKF catalog.
///
/// Later milestones add variants (auth, git, lint, publish); each variant
/// carries the catalog-relative path it concerns so an agent can act on it.
#[derive(Debug, thiserror::Error)]
pub enum OkfError {
    /// No concept or document at the given path.
    #[error("no concept {path:?}")]
    NotFound {
        /// The path as the caller gave it.
        path: String,
    },

    /// The path is outside the catalog, reserved, machinery, or already taken.
    #[error("{path}: {reason}")]
    InvalidPath {
        /// The path as the caller gave it.
        path: String,
        /// Why it was refused.
        reason: String,
    },

    /// A document's YAML frontmatter does not parse (or would not, after an edit).
    #[error("{path}: frontmatter does not parse (line {line}): {message}")]
    FrontmatterParse {
        /// Catalog-relative path of the document.
        path: String,
        /// 1-based line number in the file.
        line: usize,
        /// The parser's message.
        message: String,
    },

    /// A frontmatter key has a shape the edit can't handle (e.g. `sources` is a string).
    #[error("{path}: {key} has an unexpected shape; fix it by hand")]
    Shape {
        /// Catalog-relative path of the document.
        path: String,
        /// The frontmatter key.
        key: String,
    },

    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Catalog-relative path being read or written.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
```

- [ ] **Step 5: Write the failing clock tests.** Create `src/clock.rs` containing only the tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fixed_clock_returns_its_instant() {
        let instant = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(FixedClock(instant).now(), instant);
    }

    #[test]
    fn format_drops_subseconds() {
        let instant = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
            + chrono::Duration::milliseconds(999);
        assert_eq!(format_timestamp(instant), "2026-01-02T03:04:05Z");
    }

    #[test]
    fn system_clock_is_close_to_utc_now() {
        let delta = (SystemClock.now() - Utc::now()).num_seconds().abs();
        assert!(delta < 5);
    }
}
```

- [ ] **Step 6: Run the tests and check they fail**

Run: `cargo test --lib clock`
Expected: compile errors (`cannot find type FixedClock`, `cannot find function format_timestamp`).

- [ ] **Step 7: Implement the clock.** Insert above the tests module in `src/clock.rs`:

````rust
//! The source of "now". Every timestamp the server writes comes from here, so
//! tests can inject a fixed instant and agents can never pass one in.

use chrono::{DateTime, Utc};

/// The format of every timestamp written to a catalog: `2026-10-26T23:01:11Z`.
pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// A source of the current UTC time.
///
/// `Send + Sync` lets one clock be shared across tokio tasks (Python has no
/// equivalent; Rust checks thread-safety at compile time).
pub trait Clock: Send + Sync {
    /// Returns the current instant in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The real system clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock frozen at one instant, for tests and parity runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Formats an instant as a catalog timestamp (second precision, `Z` suffix).
///
/// # Arguments
/// * `instant` - the UTC instant to format.
///
/// # Returns
/// The timestamp string, e.g. `2026-10-26T23:01:11Z`.
///
/// # Examples
/// ```
/// use chrono::{TimeZone, Utc};
/// use okf_mcp::clock::format_timestamp;
/// let t = Utc.with_ymd_and_hms(2026, 10, 26, 23, 1, 11).unwrap();
/// assert_eq!(format_timestamp(t), "2026-10-26T23:01:11Z");
/// ```
pub fn format_timestamp(instant: DateTime<Utc>) -> String {
    instant.format(TIMESTAMP_FORMAT).to_string()
}
````

- [ ] **Step 8: Run the tests and check they pass**

Run: `cargo test clock`
Expected: PASS (3 unit tests and 1 doc test).

- [ ] **Step 9: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock .gitignore src/lib.rs src/error.rs src/clock.rs
git commit -m "Add library crate with error type and injectable clock

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: YAML-subset parser and renderer (`okf::yaml`)

Port of `okf.py` lines 67-401 (`_Inline`, `parse_inline`, `parse_value`, `parse_node`, `parse_seq`, `parse_map`, `needs_quote`, `dquote`, `scalar`, `flow_value`, `block_item`). Points to know:
- Python indexes strings by code point, so the inline parser works on a `Vec<char>`. Block parsing slices at indent widths, which count ASCII spaces, so those byte slices are always on character boundaries.
- `IndexMap` keeps insertion order, like a Python `dict`.
- The module doc lists the four deliberate divergences from `okf.py`, all in edge cases.

**Files:**
- Create: `src/okf/mod.rs`, `src/okf/yaml.rs`
- Modify: `src/lib.rs` (add `pub mod okf;`)

- [ ] **Step 1: Register the module.** Add `pub mod okf;` to `src/lib.rs` after `pub mod error;`, and create `src/okf/mod.rs`:

```rust
//! The OKF document model: a port of `tools/okf.py`'s parsing, editing and
//! path rules. Everything here is pure logic over a [`workspace::Workspace`],
//! so it needs neither git nor a real filesystem in tests.

pub mod yaml;
```

(Until Task 4 adds `workspace`, `cargo doc` warns about the unresolved `[`workspace::Workspace`]` link. That's expected; `cargo build` and `clippy` aren't affected.)

- [ ] **Step 2: Write the failing tests.** Create `src/okf/yaml.rs` containing only the tests. The last four are Hegel property tests: render-then-parse round trips for scalars, flow lists and flow maps, and "the parser never panics".

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn lines(texts: &[&'static str]) -> Vec<NumberedLine<'static>> {
        texts.iter().enumerate().map(|(i, t)| (i + 3, *t)).collect()
    }

    #[test]
    fn plain_scalars_and_nulls() {
        assert_eq!(parse_inline("hello world", None).unwrap(), s("hello world"));
        assert_eq!(parse_inline("5", None).unwrap(), s("5"));
        for null in ["", "~", "null", "Null", "NULL"] {
            assert_eq!(parse_inline(null, None).unwrap(), Value::Null, "{null:?}");
        }
    }

    #[test]
    fn plain_scalar_stops_at_comment_and_keeps_inner_hash() {
        assert_eq!(parse_inline("a#b # note", None).unwrap(), s("a#b"));
    }

    #[test]
    fn colon_rules_in_plain_scalars() {
        assert_eq!(
            parse_inline("https://x.y/z", None).unwrap(),
            s("https://x.y/z")
        );
        let err = parse_inline("a: b", Some(7)).unwrap_err();
        assert!(err.message.contains("unquoted ': '"));
        assert_eq!(err.line, Some(7));
    }

    #[test]
    fn double_quoted_escapes() {
        assert_eq!(
            parse_inline(r#""a\"b\\c\nd\x41é\L""#, None).unwrap(),
            s("a\"b\\c\ndA\u{e9}\u{2028}")
        );
        assert!(parse_inline(r#""bad \q""#, None).is_err());
        assert!(parse_inline(r#""\x4""#, None).is_err());
        assert!(parse_inline(r#""open"#, None).is_err());
    }

    #[test]
    fn single_quoted_doubles_quotes() {
        assert_eq!(parse_inline("'it''s'", None).unwrap(), s("it's"));
        assert!(parse_inline("'open", None).is_err());
    }

    #[test]
    fn flow_collections() {
        assert_eq!(
            parse_inline("{ by: claude, at: 2026-10-26T23:01:11Z }", None).unwrap(),
            map(&[("by", s("claude")), ("at", s("2026-10-26T23:01:11Z"))])
        );
        assert_eq!(
            parse_inline("[a, [b, c], {d: }]", None).unwrap(),
            Value::Seq(vec![
                s("a"),
                Value::Seq(vec![s("b"), s("c")]),
                map(&[("d", Value::Null)])
            ])
        );
        assert_eq!(parse_inline("{}", None).unwrap(), map(&[]));
        assert!(parse_inline("{a: 1, a: 2}", None)
            .unwrap_err()
            .message
            .contains("duplicate"));
        assert!(parse_inline("[a, b", None).is_err());
        assert!(parse_inline("[a] trailing", None).is_err());
    }

    #[test]
    fn unsupported_indicators_are_errors() {
        for text in ["&anchor", "*alias", "!tag", "@at", "`tick"] {
            assert!(parse_inline(text, None).is_err(), "{text}");
        }
    }

    #[test]
    fn block_list_of_mappings() {
        let children = lines(&[
            "  - id: a",
            "    resource: r1",
            "  - id: b",
            "    resource: \"r 2\"",
        ]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            Value::Seq(vec![
                map(&[("id", s("a")), ("resource", s("r1"))]),
                map(&[("id", s("b")), ("resource", s("r 2"))]),
            ])
        );
    }

    #[test]
    fn block_list_at_key_indent() {
        let children = lines(&["- one", "- two"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            Value::Seq(vec![s("one"), s("two")])
        );
    }

    #[test]
    fn nested_block_mapping_with_comments() {
        let children = lines(&["  # a comment", "  by: x", "", "  at: y  # trailing"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            map(&[("by", s("x")), ("at", s("y"))])
        );
    }

    #[test]
    fn block_scalars() {
        let children = lines(&["  line one", "    indented", "", "  last"]);
        assert_eq!(
            parse_value("|", &children, 2).unwrap(),
            s("line one\n  indented\n\nlast\n")
        );
        assert_eq!(
            parse_value(">", &children, 2).unwrap(),
            s("line one   indented  last\n")
        );
    }

    #[test]
    fn multi_line_flow_collection() {
        let children = lines(&["  b,", "  c]"]);
        assert_eq!(
            parse_value("[a,", &children, 2).unwrap(),
            Value::Seq(vec![s("a"), s("b"), s("c")])
        );
    }

    #[test]
    fn value_on_key_line_and_children_is_an_error() {
        let children = lines(&["  more"]);
        let err = parse_value("x", &children, 2).unwrap_err();
        assert_eq!(err.line, Some(2));
    }

    #[test]
    fn quoted_map_keys() {
        let children = lines(&["  \"a b\": 1", "  'c': 2"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            map(&[("a b", s("1")), ("c", s("2"))])
        );
    }

    #[test]
    fn bad_indentation_is_reported_with_line() {
        let children = lines(&["  a: 1", " b: 2"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(
            err.message.starts_with("bad indentation"),
            "{}",
            err.message
        );
        assert_eq!(err.line, Some(4));
    }

    #[test]
    fn deeper_line_under_a_scalar_is_an_error_on_the_key_line() {
        let children = lines(&["  a: 1", "   b: 2"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(err.message.contains("value on the key line"));
        assert_eq!(err.line, Some(3));
    }

    #[test]
    fn needs_quote_cases() {
        for text in [
            "", " x", "true", "No", "a: b", "a #b", "end:", "5", "-1.5e3", "#x", "[x",
        ] {
            assert!(needs_quote(text, false), "{text:?} should need quotes");
        }
        for text in [
            "plain",
            "a:b",
            "claude-code/claude-opus-5-5",
            "2026-10-26T23:01:11Z",
        ] {
            assert!(!needs_quote(text, false), "{text:?} should be plain");
        }
        assert!(needs_quote("a,b", true));
        assert!(!needs_quote("a,b", false));
    }

    #[test]
    fn double_quote_escapes_controls() {
        assert_eq!(
            double_quote("a\"b\\c\n\t\u{1}\u{7f}"),
            "\"a\\\"b\\\\c\\n\\t\\x01\\x7f\""
        );
    }

    #[test]
    fn flow_value_renders_like_okf_py() {
        let item = map(&[
            ("by", s("claude-code/claude-opus-5-5")),
            ("at", s("2026-10-26T23:01:11Z")),
        ]);
        assert_eq!(
            flow_value(&item),
            "{ by: claude-code/claude-opus-5-5, at: 2026-10-26T23:01:11Z }"
        );
        assert_eq!(
            flow_value(&Value::Seq(vec![s("a,b"), Value::Null])),
            "[\"a,b\", null]"
        );
        assert_eq!(flow_value(&map(&[])), "{  }");
    }

    #[test]
    fn block_item_shapes() {
        let entry = map(&[
            ("id", s("x")),
            ("resource", s("a: b")),
            ("title", Value::Null),
        ]);
        assert_eq!(
            block_item(&entry, 2),
            vec!["  - id: x", "    resource: \"a: b\"", "    title:"]
        );
        assert_eq!(block_item(&s("q"), 0), vec!["- q"]);
        assert_eq!(block_item(&Value::Seq(vec![s("a")]), 2), vec!["  - [a]"]);
        assert_eq!(block_item(&Value::Null, 2), vec!["  - null"]);
    }

    #[test]
    fn to_json_shapes() {
        let value = map(&[("a", Value::Seq(vec![s("x"), Value::Null]))]);
        assert_eq!(value.to_json(), serde_json::json!({"a": ["x", null]}));
    }

    #[hegel::test]
    fn scalar_round_trips_through_parse_inline(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let rendered = scalar(&text, false, false);
        assert_eq!(parse_inline(&rendered, None).unwrap(), Value::Str(text));
    }

    #[hegel::test]
    fn flow_list_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        let items: Vec<String> = tc.draw(generators::vecs(generators::text()));
        let value = Value::Seq(items.into_iter().map(Value::Str).collect());
        assert_eq!(parse_inline(&flow_value(&value), None).unwrap(), value);
    }

    #[hegel::test]
    fn flow_map_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        let keys: Vec<String> = tc.draw(generators::vecs(generators::text()).unique(true));
        let value = Value::Map(
            keys.into_iter()
                .map(|k| (k.clone(), Value::Str(k)))
                .collect(),
        );
        assert_eq!(parse_inline(&flow_value(&value), None).unwrap(), value);
    }

    #[hegel::test]
    fn parse_inline_never_panics(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let _ = parse_inline(&text, Some(1));
    }
}
```

- [ ] **Step 3: Run the tests and check they fail**

Run: `cargo test --lib okf::yaml`
Expected: compile errors (`cannot find function parse_inline`, `cannot find type Value`, ...).

- [ ] **Step 4: Implement.** Insert above the tests module in `src/okf/yaml.rs`:

````rust
//! A YAML-subset parser and renderer covering what OKF frontmatter uses.
//!
//! Port of the parser in `tools/okf.py`: plain, single- and double-quoted
//! scalars, flow maps and lists, block maps and lists (nested), comments,
//! and `|`/`>` block scalars. Scalars stay strings (`"5"` is not a number).
//!
//! Deliberate divergences from `okf.py`, all in edge cases:
//! - A null key parses as the string `"null"` (Python's `None` key, which
//!   `json.dumps` also writes as `"null"`).
//! - Rendering a null item in a block list writes `null`, not Python's `None`.
//! - A `\u`/`\U` escape naming a surrogate is an error (Python accepts it).
//! - Error messages quote with `"` where Python's `repr` uses `'`.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

/// A parsed YAML value. Every scalar is a string, as in `okf.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `~`, `null`, `Null`, `NULL`, or nothing.
    Null,
    /// Any scalar.
    Str(String),
    /// A flow (`[a, b]`) or block (`- a`) list.
    Seq(Vec<Value>),
    /// A flow (`{ a: b }`) or block mapping, in source order.
    Map(IndexMap<String, Value>),
}

impl Value {
    /// Returns the string if this is a scalar.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Null | Value::Seq(_) | Value::Map(_) => None,
        }
    }

    /// Converts to JSON (strings, arrays, objects, null), the shape the parity
    /// tests compare against `okf.py`'s `json.dumps` output.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::Seq(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_json).collect())
            }
            Value::Map(map) => serde_json::Value::Object(
                map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect(),
            ),
        }
    }
}

/// A frontmatter parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct YamlError {
    /// What went wrong.
    pub message: String,
    /// 1-based file line, when known.
    pub line: Option<usize>,
}

impl YamlError {
    fn new(message: impl Into<String>, line: Option<usize>) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}

/// One frontmatter line with its 1-based file line number.
pub type NumberedLine<'a> = (usize, &'a str);

/// Number of leading spaces (tabs don't count, as in `okf.py`).
pub(crate) fn indent_of(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// `text` without its first `n` characters (Python's `t[n:]`; never panics).
fn skip_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((byte_index, _)) => &text[byte_index..],
        None => "",
    }
}

/// Parser for one line's worth of inline YAML (a scalar or a flow collection).
struct Inline<'a> {
    chars: Vec<char>,
    pos: usize,
    line: Option<usize>,
    source: &'a str,
}

impl<'a> Inline<'a> {
    fn new(source: &'a str, line: Option<usize>) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line,
            source,
        }
    }

    fn err(&self, message: &str) -> YamlError {
        YamlError::new(format!("{message} in {:?}", self.source), self.line)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn node(&mut self, flow: bool) -> Result<Value, YamlError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.flow_map(),
            Some('[') => self.flow_seq(),
            Some('"') => self.double_quoted().map(Value::Str),
            Some('\'') => self.single_quoted().map(Value::Str),
            Some(c @ ('&' | '*' | '!' | '|' | '>' | '%' | '@' | '`')) => {
                Err(self.err(&format!("unsupported YAML indicator {c:?}")))
            }
            _ => Ok(self.plain(flow).map_or(Value::Null, Value::Str)),
        }
    }

    fn double_quoted(&mut self) -> Result<String, YamlError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated double-quoted string"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    let escape = self.chars.get(self.pos + 1).copied();
                    if let Some(decoded) = escape.and_then(simple_escape) {
                        out.push(decoded);
                        self.pos += 2;
                        continue;
                    }
                    let width = match escape {
                        Some('x') => 2,
                        Some('u') => 4,
                        Some('U') => 8,
                        _ => {
                            let shown = escape.map(String::from).unwrap_or_default();
                            return Err(self.err(&format!("invalid escape \\{shown}")));
                        }
                    };
                    let start = self.pos + 2;
                    let hex: String = self.chars.iter().skip(start).take(width).collect();
                    let decoded = (hex.chars().count() == width
                        && hex.chars().all(|h| h.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32);
                    match decoded {
                        Some(ch) => out.push(ch),
                        None => return Err(self.err("bad escape")),
                    }
                    self.pos = start + width;
                }
                other => {
                    out.push(other);
                    self.pos += 1;
                }
            }
        }
    }

    fn single_quoted(&mut self) -> Result<String, YamlError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated single-quoted string"));
            };
            self.pos += 1;
            if c != '\'' {
                out.push(c);
                continue;
            }
            // A doubled quote ('') is an escaped quote; a single one ends the string.
            if self.peek() == Some('\'') {
                out.push('\'');
                self.pos += 1;
                continue;
            }
            return Ok(out);
        }
    }

    /// A plain (unquoted) scalar; `None` for the null spellings.
    fn plain(&mut self, flow: bool) -> Option<String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            let next = self.chars.get(self.pos + 1).copied();
            if c == '#' && self.pos > start && matches!(self.chars[self.pos - 1], ' ' | '\t') {
                break;
            }
            if flow && matches!(c, ',' | ']' | '}') {
                break;
            }
            if c == ':'
                && (matches!(next, None | Some(' ' | '\t'))
                    || (flow && matches!(next, Some(',' | ']' | '}'))))
            {
                break;
            }
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        let text = text.trim_end();
        match text {
            "" | "~" | "null" | "Null" | "NULL" => None,
            other => Some(other.to_string()),
        }
    }

    fn key(&mut self) -> Result<String, YamlError> {
        self.skip_ws();
        let key = match self.peek() {
            Some('"') => self.double_quoted()?,
            Some('\'') => self.single_quoted()?,
            _ => self.plain(true).unwrap_or_else(|| "null".to_string()),
        };
        self.skip_ws();
        if self.peek() != Some(':') {
            return Err(self.err("expected ':' after key"));
        }
        self.pos += 1;
        Ok(key)
    }

    fn flow_map(&mut self) -> Result<Value, YamlError> {
        self.pos += 1;
        let mut map = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek() == Some('}') {
                self.pos += 1;
                return Ok(Value::Map(map));
            }
            let key = self.key()?;
            if map.contains_key(&key) {
                return Err(self.err(&format!("duplicate key {key:?}")));
            }
            self.skip_ws();
            let value = if matches!(self.peek(), Some(',' | '}')) {
                Value::Null
            } else {
                self.node(true)?
            };
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {}
                _ => return Err(self.err("expected ',' or '}' in flow map")),
            }
        }
    }

    fn flow_seq(&mut self) -> Result<Value, YamlError> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(Value::Seq(items));
            }
            items.push(self.node(true)?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {}
                _ => return Err(self.err("expected ',' or ']' in flow list")),
            }
        }
    }

    fn finish(&mut self) -> Result<(), YamlError> {
        self.skip_ws();
        match self.peek() {
            None | Some('#') => Ok(()),
            Some(_) => Err(self.err("unexpected trailing text")),
        }
    }
}

/// The single-character escapes of a double-quoted scalar.
fn simple_escape(c: char) -> Option<char> {
    Some(match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '"' => '"',
        '\\' => '\\',
        '/' => '/',
        '0' => '\0',
        ' ' => ' ',
        'b' => '\u{8}',
        'f' => '\u{c}',
        'e' => '\u{1b}',
        'a' => '\u{7}',
        'v' => '\u{b}',
        'N' => '\u{85}',
        '_' => '\u{a0}',
        'L' => '\u{2028}',
        'P' => '\u{2029}',
        _ => return None,
    })
}

/// Parses one inline value: a scalar or a flow collection on a single line.
///
/// # Arguments
/// * `text` - the value text (after `key:` or `- `).
/// * `line` - its 1-based file line, for error messages.
///
/// # Errors
/// [`YamlError`] on unsupported indicators, unterminated quotes, bad flow
/// syntax, trailing text, or an unquoted `: ` inside a plain value.
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::{parse_inline, Value};
/// assert_eq!(parse_inline("[a, 'b c']", None).unwrap(),
///            Value::Seq(vec![Value::Str("a".into()), Value::Str("b c".into())]));
/// ```
pub fn parse_inline(text: &str, line: Option<usize>) -> Result<Value, YamlError> {
    let mut parser = Inline::new(text, line);
    let value = parser.node(false)?;
    if parser.peek() == Some(':') {
        return Err(YamlError::new(
            format!("unquoted ': ' inside a plain value {text:?}; quote it"),
            line,
        ));
    }
    parser.finish()?;
    Ok(value)
}

/// Strips a value's surrounding whitespace, or all of it when it is a comment.
pub(crate) fn strip_comment(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with('#') {
        ""
    } else {
        trimmed
    }
}

/// `key: value` at the start of a (dedented) block-mapping line.
static MAP_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^([A-Za-z0-9_][\w.\-/ ]*?|"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')[ \t]*:(?:[ \t]+(.*)|[ \t]*)$"#,
    )
    .expect("MAP_KEY regex is valid")
});

/// Lines that carry content (not blank, not a comment).
fn content_lines<'a>(lines: &[NumberedLine<'a>]) -> Vec<NumberedLine<'a>> {
    lines
        .iter()
        .copied()
        .filter(|(_, t)| !t.trim().is_empty() && !t.trim().starts_with('#'))
        .collect()
}

/// True for a block-list item line (`-` or `- ...`), already dedented.
pub(crate) fn is_seq_start(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// Parses a key's value: the text after `key:` plus its indented child lines.
///
/// # Arguments
/// * `inline` - text after the colon on the key's line.
/// * `children` - the following, more-indented lines (blank and comment lines included).
/// * `line` - the key's 1-based file line.
///
/// # Errors
/// [`YamlError`] when the value can't be parsed.
pub fn parse_value(
    inline: &str,
    children: &[NumberedLine<'_>],
    line: usize,
) -> Result<Value, YamlError> {
    let text = strip_comment(inline);
    if let Some(style) = text.chars().next().filter(|c| matches!(c, '|' | '>')) {
        let indent = children
            .iter()
            .filter(|(_, t)| !t.trim().is_empty())
            .map(|(_, t)| indent_of(t))
            .min()
            .unwrap_or(0);
        let body: Vec<&str> = children
            .iter()
            .map(|(_, t)| skip_chars(t, indent))
            .collect();
        let joined = body.join(if style == '|' { "\n" } else { " " });
        return Ok(Value::Str(format!("{}\n", joined.trim_end_matches('\n'))));
    }
    let has_content = !content_lines(children).is_empty();
    if !text.is_empty() {
        if text.starts_with(['[', '{']) && has_content {
            // A flow collection continued over several lines.
            let mut parts = vec![text];
            parts.extend(children.iter().map(|(_, t)| t.trim()));
            return parse_inline(&parts.join(" "), Some(line));
        }
        if has_content {
            return Err(YamlError::new(
                "value on the key line and indented lines below it",
                Some(line),
            ));
        }
        return parse_inline(text, Some(line));
    }
    let content = content_lines(children);
    if content.is_empty() {
        Ok(Value::Null)
    } else {
        parse_node(&content)
    }
}

/// Parses a block node (list or mapping) from content lines.
fn parse_node(content: &[NumberedLine<'_>]) -> Result<Value, YamlError> {
    let indent = indent_of(content[0].1);
    if is_seq_start(&content[0].1[indent..]) {
        parse_seq(content, indent)
    } else {
        parse_map(content, indent)
    }
}

/// Splits block-list lines into (item line, its child lines).
fn split_items<'c, 'a>(
    content: &'c [NumberedLine<'a>],
    indent: usize,
) -> Result<Vec<(NumberedLine<'a>, &'c [NumberedLine<'a>])>, YamlError> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < content.len() {
        let (number, text) = content[i];
        if indent_of(text) != indent || !is_seq_start(&text[indent..]) {
            return Err(YamlError::new(
                format!("bad indentation or structure: {:?}", text.trim()),
                Some(number),
            ));
        }
        let mut j = i + 1;
        while j < content.len() && indent_of(content[j].1) > indent {
            j += 1;
        }
        items.push((content[i], &content[i + 1..j]));
        i = j;
    }
    Ok(items)
}

fn parse_seq(content: &[NumberedLine<'_>], indent: usize) -> Result<Value, YamlError> {
    let mut items = Vec::new();
    for ((number, text), kids) in split_items(content, indent)? {
        let rest = &text[indent + 1..];
        let item = rest.trim();
        if item.is_empty() || item.starts_with('#') {
            items.push(if kids.is_empty() {
                Value::Null
            } else {
                parse_node(kids)?
            });
            continue;
        }
        let offset = indent + 1 + indent_of(rest);
        if !item.starts_with(['[', '{', '"', '\'']) && MAP_KEY.is_match(item) {
            // `- key: value`: a mapping whose first key shares the dash's line.
            let first = format!("{}{item}", " ".repeat(offset));
            let mut lines = vec![(number, first.as_str())];
            lines.extend_from_slice(kids);
            items.push(parse_map(&lines, offset)?);
        } else {
            items.push(parse_value(item, kids, number)?);
        }
    }
    Ok(Value::Seq(items))
}

fn parse_map(content: &[NumberedLine<'_>], indent: usize) -> Result<Value, YamlError> {
    let mut map = IndexMap::new();
    let mut i = 0;
    while i < content.len() {
        let (number, text) = content[i];
        if indent_of(text) != indent {
            return Err(YamlError::new(
                format!("bad indentation: {:?}", text.trim()),
                Some(number),
            ));
        }
        let Some(caps) = MAP_KEY.captures(&text[indent..]) else {
            return Err(YamlError::new(
                format!("expected 'key: value', got {:?}", text.trim()),
                Some(number),
            ));
        };
        let inline = caps.get(2).map_or("", |m| m.as_str());
        let mut j = i + 1;
        // Children are more indented, or a block list at the same indent.
        while j < content.len() && {
            let child = content[j].1;
            indent_of(child) > indent
                || (indent_of(child) == indent
                    && is_seq_start(&child[indent..])
                    && inline.trim().is_empty())
        } {
            j += 1;
        }
        let raw_key = &caps[1];
        let key = if raw_key.starts_with(['"', '\'']) {
            match parse_inline(raw_key, Some(number))? {
                Value::Str(s) => s,
                Value::Null | Value::Seq(_) | Value::Map(_) => raw_key.to_string(),
            }
        } else {
            raw_key.to_string()
        };
        if map.contains_key(&key) {
            return Err(YamlError::new(
                format!("duplicate key {key:?}"),
                Some(number),
            ));
        }
        let value = parse_value(inline, &content[i + 1..j], number)?;
        map.insert(key, value);
        i = j;
    }
    Ok(Value::Map(map))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

static PLAIN_SAFE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_./~+()][^\n\t]*$").expect("PLAIN_SAFE regex is valid")
});
static NUMBER_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(\d[\d_]*)?\.?\d*([eE][-+]?\d+)?$").expect("NUMBER_LIKE regex is valid")
});
const RESERVED_WORDS: [&str; 10] = [
    "true", "false", "yes", "no", "on", "off", "null", "~", "y", "n",
];

/// Would `text` change meaning (or fail to parse) if written unquoted?
///
/// # Arguments
/// * `text` - the scalar.
/// * `flow` - whether it sits inside a flow collection, where `,[]{}` are syntax.
pub fn needs_quote(text: &str, flow: bool) -> bool {
    if text.is_empty() || text != text.trim() || !PLAIN_SAFE.is_match(text) {
        return true;
    }
    if RESERVED_WORDS.contains(&text.to_lowercase().as_str())
        || text.contains(": ")
        || text.contains(" #")
        || text.ends_with(':')
    {
        return true;
    }
    if flow && text.contains([',', '[', ']', '{', '}']) {
        return true;
    }
    NUMBER_LIKE.is_match(text)
}

/// Double-quotes a scalar, escaping quotes, backslashes and control characters.
pub fn double_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Renders a scalar, quoting only when needed (or always, with `force`).
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::scalar;
/// assert_eq!(scalar("plain text", false, false), "plain text");
/// assert_eq!(scalar("a: b", false, false), "\"a: b\"");
/// assert_eq!(scalar("x", false, true), "\"x\"");
/// ```
pub fn scalar(text: &str, flow: bool, force: bool) -> String {
    if force || needs_quote(text, flow) {
        double_quote(text)
    } else {
        text.to_string()
    }
}

/// Renders a value in flow style on one line: `{ by: x, at: y }`, `[a, b]`.
pub fn flow_value(value: &Value) -> String {
    match value {
        Value::Map(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", scalar(k, true, false), flow_value(v)))
                .collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Seq(items) => {
            let items: Vec<String> = items.iter().map(flow_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Null => "null".to_string(),
        Value::Str(s) => scalar(s, true, false),
    }
}

/// Renders one block-list item as lines (no trailing newlines).
///
/// A mapping of scalars becomes `- k: v` with continuation keys aligned
/// under it; any other collection is written in flow style after `- `.
///
/// # Arguments
/// * `value` - the item.
/// * `indent` - spaces before the dash.
pub fn block_item(value: &Value, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    match value {
        Value::Map(map)
            if !map.is_empty()
                && map
                    .values()
                    .all(|v| matches!(v, Value::Null | Value::Str(_))) =>
        {
            map.iter()
                .enumerate()
                .map(|(i, (k, v))| {
                    let lead = if i == 0 {
                        format!("{pad}- ")
                    } else {
                        format!("{pad}  ")
                    };
                    let rendered = v
                        .as_str()
                        .map(|s| scalar(s, false, false))
                        .unwrap_or_default();
                    format!("{lead}{}: {rendered}", scalar(k, false, false))
                        .trim_end()
                        .to_string()
                })
                .collect()
        }
        Value::Map(_) | Value::Seq(_) => vec![format!("{pad}- {}", flow_value(value))],
        Value::Null => vec![format!("{pad}- null")],
        Value::Str(s) => vec![format!("{pad}- {}", scalar(s, false, false))],
    }
}
````

- [ ] **Step 5: Run the tests and check they pass**

Run: `cargo test okf::yaml`
Expected: PASS (25 unit tests, including the 4 property tests, and 2 doc tests). The first Hegel run downloads its server into `.hegel/`.

- [ ] **Step 6: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/lib.rs src/okf/mod.rs src/okf/yaml.rs
git commit -m "Port okf.py's YAML-subset parser and renderer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Catalog paths and the workspace abstraction (`okf::paths`, `okf::workspace`)

The two modules reference each other: `paths::resolve_concept` takes a `&dyn Workspace`, and the workspaces use `paths::normpath` and `walk_order`. That's fine in Rust, but it means they land together. `walk_order` reproduces `os.walk`'s order (files before subdirectories, each sorted), which later reports depend on. Unlike `okf.py`, filesystem paths aren't accepted as concept references, because a server has no meaningful working directory.

**Files:**
- Create: `src/okf/paths.rs`, `src/okf/workspace.rs`
- Modify: `src/okf/mod.rs`

- [ ] **Step 1: Register the modules.** In `src/okf/mod.rs`, make the module list:

```rust
pub mod paths;
pub mod workspace;
pub mod yaml;
```

- [ ] **Step 2: Write the failing tests.** Create `src/okf/paths.rs` containing only its tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn workspace() -> MemWorkspace {
        MemWorkspace::from_files([
            ("index.md", "# Root\n"),
            ("infra/index.md", "# Infra\n"),
            ("infra/time-server.md", "---\ntype: Service\n---\n"),
            ("AGENTS.md", "# Agents\n"),
        ])
    }

    #[test]
    fn normpath_cases() {
        assert_eq!(normpath("a//b/./c/"), "a/b/c");
        assert_eq!(normpath("a/../../b"), "../b");
        assert_eq!(normpath("/a/../.."), "/");
        assert_eq!(normpath("."), ".");
    }

    #[test]
    fn dirname_basename_join() {
        assert_eq!(dirname("a/b/c.md"), "a/b");
        assert_eq!(dirname("c.md"), "");
        assert_eq!(basename("a/b/c.md"), "c.md");
        assert_eq!(join("", "x.md"), "x.md");
        assert_eq!(join("a", "x.md"), "a/x.md");
        assert_eq!(join("a", "/x.md"), "/x.md");
    }

    #[test]
    fn concepts_exclude_reserved_and_machinery_docs() {
        assert!(is_concept("infra/time-server.md"));
        assert!(!is_concept("infra/index.md"));
        assert!(!is_concept("AGENTS.md"));
    }

    #[test]
    fn resolve_accepts_every_spelling() {
        let ws = workspace();
        for reference in [
            "/infra/time-server.md",
            "infra/time-server.md",
            "/infra/time-server",
            "infra/time-server",
        ] {
            assert_eq!(
                resolve_concept(&ws, reference).unwrap(),
                "infra/time-server.md",
                "{reference}"
            );
        }
    }

    #[test]
    fn resolve_refuses_non_concepts_and_missing() {
        let ws = workspace();
        assert!(matches!(
            resolve_concept(&ws, "/infra/index.md"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            resolve_concept(&ws, "/nope"),
            Err(OkfError::NotFound { .. })
        ));
        assert!(matches!(
            resolve_concept(&ws, "../etc/passwd"),
            Err(OkfError::NotFound { .. })
        ));
    }

    #[test]
    fn new_concept_path_rules() {
        let ws = workspace();
        assert_eq!(
            new_concept_path(&ws, "/infra/new-thing").unwrap(),
            ("infra/new-thing.md".to_string(), None)
        );
        let (_, note) = new_concept_path(&ws, "infra/New_Thing.md").unwrap();
        assert!(note.is_some());
        for bad in [
            "../x",
            "infra/index.md",
            "tools/x",
            ".claude/x",
            "a/.hidden/x",
            "/infra/time-server",
        ] {
            assert!(new_concept_path(&ws, bad).is_err(), "{bad}");
        }
    }

    #[hegel::test]
    fn normpath_is_idempotent(tc: hegel::TestCase) {
        use hegel::generators;
        let path: String = tc.draw(generators::text());
        let once = normpath(&path);
        assert_eq!(normpath(&once), once);
    }
}
```

and `src/okf/workspace.rs` containing only its tests. The same contract test runs against both implementations:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FILES: [(&str, &str); 6] = [
        ("index.md", "root"),
        ("b.md", "b"),
        ("infra/index.md", "infra"),
        ("infra/hosts/x.md", "x"),
        (".claude/skill.md", "hidden"),
        ("notes.txt", "not markdown"),
    ];

    fn assert_contract(ws: &dyn Workspace) {
        assert_eq!(
            ws.list_markdown().unwrap(),
            ["b.md", "index.md", "infra/index.md", "infra/hosts/x.md"]
        );
        assert_eq!(ws.read_to_string("/infra/index.md").unwrap(), "infra");
        assert!(ws.is_file("b.md") && !ws.is_dir("b.md"));
        assert!(ws.is_dir("infra") && ws.is_dir("") && !ws.is_file("infra"));
        assert!(!ws.exists("missing.md"));
        assert!(matches!(
            ws.read_to_string("../x"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            ws.read_to_string("missing.md"),
            Err(OkfError::Io { .. })
        ));
        ws.write_atomic("new/dir/c.md", "c").unwrap();
        assert_eq!(ws.read_to_string("new/dir/c.md").unwrap(), "c");
        ws.write_atomic("b.md", "b2").unwrap();
        assert_eq!(ws.read_to_string("b.md").unwrap(), "b2");
        assert!(ws.write_atomic("../escape.md", "x").is_err());
    }

    #[test]
    fn mem_workspace_contract() {
        assert_contract(&MemWorkspace::from_files(FILES));
    }

    #[test]
    fn disk_workspace_contract() {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in FILES {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        let ws = DiskWorkspace::new(dir.path());
        assert_contract(&ws);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
```

- [ ] **Step 3: Run the tests and check they fail**

Run: `cargo test --lib -- okf::paths okf::workspace`
Expected: compile errors (`cannot find function normpath`, `cannot find type MemWorkspace`, ...).

- [ ] **Step 4: Implement `paths`.** Insert above the tests module in `src/okf/paths.rs`:

````rust
//! Catalog path rules: bundle-relative path arithmetic, which files are
//! concepts, and how a caller's concept reference resolves to a file.
//!
//! All paths here are bundle-relative with `/` separators and no leading
//! slash (`infra/time-server.md`); `/` alone is never a filesystem root.
//! Unlike `okf.py`, filesystem paths (absolute or relative to a working
//! directory) are not accepted: a server has no meaningful working directory.

use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::OkfError;
use crate::okf::workspace::Workspace;

/// Markdown files that are never concepts.
pub const NON_CONCEPTS: [&str; 5] = ["index.md", "log.md", "AGENTS.md", "CLAUDE.md", "README.md"];
/// OKF's reserved filenames.
pub const RESERVED: [&str; 2] = ["index.md", "log.md"];
/// Top-level directories that hold repository machinery, not concepts.
pub const MACHINERY_DIRS: [&str; 3] = [".git", ".claude", "tools"];

static KEBAB_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*\.md$").expect("KEBAB_NAME regex is valid"));

/// Python's `posixpath.normpath` for relative paths: collapses `.`, `..` and
/// repeated slashes. An empty result is `.`; leading `..` components are kept.
///
/// # Examples
/// ```
/// use okf_mcp::okf::paths::normpath;
/// assert_eq!(normpath("infra/./hosts/../time-server.md"), "infra/time-server.md");
/// assert_eq!(normpath("../x"), "../x");
/// assert_eq!(normpath(""), ".");
/// ```
pub fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// The directory part of a path (`""` for a top-level file).
pub fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The final component of a path.
pub fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Joins a directory and a path (Python's `os.path.join` for two parts).
pub fn join(dir: &str, path: &str) -> String {
    if path.starts_with('/') || dir.is_empty() {
        path.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{path}")
    } else {
        format!("{dir}/{path}")
    }
}

/// Is this markdown file a concept (not `index.md`, `log.md` or machinery docs)?
pub fn is_concept(path: &str) -> bool {
    !NON_CONCEPTS.contains(&basename(path))
}

/// Orders paths the way `os.walk` with sorted names visits them: in each
/// directory, its files (sorted) come before its subdirectories (sorted).
///
/// # Examples
/// ```
/// use okf_mcp::okf::paths::walk_order;
/// let mut paths = vec!["b/x.md", "a.md", "a/z.md", "c.md"];
/// paths.sort_by(|x, y| walk_order(x, y));
/// assert_eq!(paths, ["a.md", "c.md", "a/z.md", "b/x.md"]);
/// ```
pub fn walk_order(left: &str, right: &str) -> Ordering {
    let left_parts: Vec<&str> = left.split('/').collect();
    let right_parts: Vec<&str> = right.split('/').collect();
    for (i, (l, r)) in left_parts.iter().zip(&right_parts).enumerate() {
        if l == r {
            continue;
        }
        let left_is_file = i == left_parts.len() - 1;
        let right_is_file = i == right_parts.len() - 1;
        return match (left_is_file, right_is_file) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (true, true) | (false, false) => l.cmp(r),
        };
    }
    left_parts.len().cmp(&right_parts.len())
}

/// Resolves a caller's concept reference to a bundle-relative path.
///
/// Accepts `/dir/x.md`, `dir/x.md`, `/dir/x` and `dir/x`.
///
/// # Arguments
/// * `workspace` - the catalog to look in.
/// * `reference` - the caller's reference.
///
/// # Errors
/// * [`OkfError::InvalidPath`] when the file exists but is not a concept.
/// * [`OkfError::NotFound`] when no candidate exists.
pub fn resolve_concept(workspace: &dyn Workspace, reference: &str) -> Result<String, OkfError> {
    let bare = reference.trim_start_matches('/');
    for candidate in [bare.to_string(), format!("{bare}.md")] {
        let candidate = normpath(&candidate);
        if candidate.starts_with("..")
            || !candidate.ends_with(".md")
            || !workspace.is_file(&candidate)
        {
            continue;
        }
        if !is_concept(&candidate) {
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

/// Validates the path for a new concept (`create`, `mv`, `split` targets).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `reference` - the requested path, with or without `.md` and a leading `/`.
///
/// # Returns
/// The normalized path and, when the filename isn't short, lowercase and
/// hyphenated, a note to pass back to the caller.
///
/// # Errors
/// [`OkfError::InvalidPath`] when the path leaves the catalog, uses a
/// reserved name, sits in machinery or a dot-directory, or already exists.
pub fn new_concept_path(
    workspace: &dyn Workspace,
    reference: &str,
) -> Result<(String, Option<String>), OkfError> {
    let invalid = |reason: &str| OkfError::InvalidPath {
        path: reference.to_string(),
        reason: reason.to_string(),
    };
    let mut path = reference.trim().trim_start_matches('/').to_string();
    if !path.ends_with(".md") {
        path.push_str(".md");
    }
    let path = normpath(&path);
    if path.starts_with("..") || path.starts_with('/') {
        return Err(invalid("not a path inside the catalog"));
    }
    let name = basename(&path);
    if NON_CONCEPTS.contains(&name) {
        return Err(invalid(&format!(
            "{name} is a reserved name, not a concept"
        )));
    }
    let first = path.split('/').next().unwrap_or_default();
    if MACHINERY_DIRS.contains(&first) || path.split('/').any(|p| p.starts_with('.')) {
        return Err(invalid(
            "that's repository machinery, not a place for concepts",
        ));
    }
    if workspace.exists(&path) {
        return Err(OkfError::InvalidPath {
            path: path.clone(),
            reason: "already exists".to_string(),
        });
    }
    let note = (!KEBAB_NAME.is_match(name))
        .then(|| format!("{name} isn't short, lowercase and hyphenated"));
    Ok((path, note))
}
````

- [ ] **Step 5: Implement `workspace`.** Insert above the tests module in `src/okf/workspace.rs`:

```rust
//! The filesystem a catalog lives in, behind a trait so the OKF logic can be
//! tested against an in-memory catalog.
//!
//! Paths are bundle-relative (`infra/time-server.md`). Implementations must
//! refuse paths that escape the catalog root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::error::OkfError;
use crate::okf::paths::{normpath, walk_order};

/// Read and write access to one catalog's files.
///
/// `Send + Sync` so a workspace can be shared across tokio tasks.
pub trait Workspace: Send + Sync {
    /// Reads a file as UTF-8.
    ///
    /// # Errors
    /// [`OkfError::Io`] if the file is missing, unreadable or not UTF-8.
    fn read_to_string(&self, path: &str) -> Result<String, OkfError>;

    /// Writes a file atomically (all or nothing), creating parent directories.
    ///
    /// # Errors
    /// [`OkfError::Io`] on any filesystem failure; [`OkfError::InvalidPath`]
    /// if the path escapes the catalog.
    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError>;

    /// Is there a file or directory at `path`?
    fn exists(&self, path: &str) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    /// Is there a regular file at `path`?
    fn is_file(&self, path: &str) -> bool;

    /// Is there a directory at `path` (`""` and `.` are the root)?
    fn is_dir(&self, path: &str) -> bool;

    /// Every `.md` file, skipping dot-directories, in `os.walk` order
    /// (see [`walk_order`]).
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed.
    fn list_markdown(&self) -> Result<Vec<String>, OkfError>;
}

/// Normalizes a bundle-relative path and refuses one that leaves the bundle.
fn checked(path: &str) -> Result<String, OkfError> {
    let normalized = normpath(path.trim_start_matches('/'));
    if normalized.starts_with("..") {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "outside the catalog".to_string(),
        });
    }
    Ok(normalized)
}

/// Is any directory component hidden (starts with `.`)?
fn in_dot_directory(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|p| p.starts_with('.'))
}

/// A catalog on disk, rooted at a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskWorkspace {
    root: PathBuf,
}

impl DiskWorkspace {
    /// Creates a workspace rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn full(&self, path: &str) -> Result<PathBuf, OkfError> {
        let rel = checked(path)?;
        Ok(if rel == "." {
            self.root.clone()
        } else {
            self.root.join(rel)
        })
    }

    fn collect(&self, dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), OkfError> {
        let io_err = |source| OkfError::Io {
            path: prefix.to_string(),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let file_type = entry.file_type().map_err(io_err)?;
            if file_type.is_dir() {
                if !name.starts_with('.') {
                    self.collect(&entry.path(), &rel, out)?;
                }
            } else if name.ends_with(".md") {
                out.push(rel);
            }
        }
        Ok(())
    }
}

impl Workspace for DiskWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        std::fs::read_to_string(self.full(path)?).map_err(|source| OkfError::Io {
            path: path.to_string(),
            source,
        })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let target = self.full(path)?;
        let io_err = |source| OkfError::Io {
            path: path.to_string(),
            source,
        };
        let parent = target.parent().unwrap_or(&self.root).to_path_buf();
        std::fs::create_dir_all(&parent).map_err(io_err)?;
        // Write a sibling temp file, then rename over the target: a reader sees
        // the old file or the new one, never half of it.
        let file_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
        std::fs::write(&temp, contents).map_err(io_err)?;
        std::fs::rename(&temp, &target).map_err(io_err)
    }

    fn is_file(&self, path: &str) -> bool {
        self.full(path).is_ok_and(|p| p.is_file())
    }

    fn is_dir(&self, path: &str) -> bool {
        self.full(path).is_ok_and(|p| p.is_dir())
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        let mut out = Vec::new();
        self.collect(&self.root, "", &mut out)?;
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
}

/// An in-memory catalog for tests. Directories exist implicitly as prefixes
/// of file paths.
#[derive(Debug, Default)]
pub struct MemWorkspace {
    // `RwLock` gives interior mutability: `write_atomic` takes `&self` (like the
    // disk version) yet can still change the map, safely across threads.
    files: RwLock<BTreeMap<String, String>>,
}

impl MemWorkspace {
    /// Builds a workspace from `(path, contents)` pairs.
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let map = files
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        Self {
            files: RwLock::new(map),
        }
    }

    fn files(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, String>> {
        // A poisoned lock means a test panicked mid-write; propagating the panic is right.
        self.files.read().expect("MemWorkspace lock poisoned")
    }
}

impl Workspace for MemWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        let rel = checked(path)?;
        self.files().get(&rel).cloned().ok_or_else(|| OkfError::Io {
            path: path.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let rel = checked(path)?;
        self.files
            .write()
            .expect("MemWorkspace lock poisoned")
            .insert(rel, contents.to_string());
        Ok(())
    }

    fn is_file(&self, path: &str) -> bool {
        checked(path).is_ok_and(|rel| self.files().contains_key(&rel))
    }

    fn is_dir(&self, path: &str) -> bool {
        let Ok(rel) = checked(path) else { return false };
        if rel == "." {
            return true;
        }
        let prefix = format!("{rel}/");
        self.files().keys().any(|k| k.starts_with(&prefix))
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        let mut out: Vec<String> = self
            .files()
            .keys()
            .filter(|k| k.ends_with(".md") && !in_dot_directory(k))
            .cloned()
            .collect();
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
}
```

- [ ] **Step 6: Run the tests and check they pass**

Run: `cargo test okf::paths` and then `cargo test okf::workspace`
Expected: PASS (paths: 7 unit tests including 1 property test, plus 2 doc tests; workspace: 2 unit tests).

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/okf/mod.rs src/okf/paths.rs src/okf/workspace.rs
git commit -m "Add catalog path rules and the Workspace abstraction

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Frontmatter documents and textual edits (`okf::doc`)

Port of `okf.py`'s `Doc` class (lines 408-546) and `append_seq_item` (549-591). The core guarantee: an edit changes only the target key's lines, and every other byte (comments, blank lines, key order, CRLF) survives. Note `parse_frontmatter_lines` numbers child lines `start + 2 + i`, exactly as `okf.py` does, so error lines match.

**Files:**
- Create: `src/okf/doc.rs`
- Modify: `src/okf/mod.rs`

- [ ] **Step 1: Register the module.** In `src/okf/mod.rs`, add `pub mod doc;` as the first entry of the module list.

- [ ] **Step 2: Write the failing tests.** Create `src/okf/doc.rs` containing only the tests. The last two are property tests: `set_block` changes only the target key, and `from_text` then `render` is the identity on any text.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // `concat!` keeps each line's indentation (a `\` line continuation would strip it).
    const SAMPLE: &str = concat!(
        "---\n",
        "type: Note\n",
        "title: Sample # comment kept\n",
        "\n",
        "tags: [a, b]\n",
        "sources:\n",
        "  - id: one\n",
        "    resource: r1\n",
        "---\n",
        "Body text.\n",
    );

    fn doc(text: &str) -> Doc {
        Doc::from_text("x.md", text)
    }

    fn quoted_item(text: &str) -> impl Fn(usize) -> Vec<String> + '_ {
        move |indent| {
            vec![format!(
                "{}- {}",
                " ".repeat(indent),
                crate::okf::yaml::scalar(text, false, true)
            )]
        }
    }

    #[test]
    fn splits_and_renders_byte_for_byte() {
        let d = doc(SAMPLE);
        assert!(d.has_frontmatter());
        assert_eq!(d.frontmatter().len(), 7);
        assert_eq!(d.render(), SAMPLE);
    }

    #[test]
    fn no_frontmatter_without_closing_delimiter() {
        let d = doc("---\ntype: x\nno close\n");
        assert!(!d.has_frontmatter());
        assert_eq!(d.render(), "---\ntype: x\nno close\n");
    }

    #[test]
    fn dots_close_frontmatter_and_crlf_is_kept() {
        let text = "---\r\ntype: Note\r\n...\r\nBody\r\n";
        let d = doc(text);
        assert!(d.has_frontmatter());
        assert_eq!(d.frontmatter(), ["type: Note"]);
        assert_eq!(d.render(), text);
    }

    #[test]
    fn blocks_cover_continuations() {
        let blocks = doc(SAMPLE).blocks();
        let keys: Vec<_> = blocks
            .iter()
            .map(|b| (b.key.as_str(), b.start, b.end))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("type", 0, 1),
                ("title", 1, 2),
                ("tags", 3, 4),
                ("sources", 4, 7)
            ]
        );
    }

    #[test]
    fn parse_gives_ordered_values() {
        let data = doc(SAMPLE).parse().unwrap();
        assert_eq!(
            data.keys().collect::<Vec<_>>(),
            ["type", "title", "tags", "sources"]
        );
        assert_eq!(data["title"], Value::Str("Sample".into()));
    }

    #[test]
    fn parse_reports_file_lines() {
        let err = doc("---\ntype: Note\ntitle: a: b\n---\n")
            .parse()
            .unwrap_err();
        assert_eq!(err.line, Some(3));
        let err = doc("---\ntype: Note\n  stray\n---\n").parse().unwrap_err();
        assert!(err.message.contains("value on the key line"));
        let err = doc("---\ntype: a\ntype: b\n---\n").parse().unwrap_err();
        assert_eq!(err.line, Some(3));
        let err = doc("---\n\"quoted\": x\n---\n").parse().unwrap_err();
        assert!(err.message.contains("not part of any key"));
    }

    #[test]
    fn key_line_is_one_based_file_line() {
        let d = doc(SAMPLE);
        assert_eq!(d.key_line("tags"), 5);
        assert_eq!(d.key_line("missing"), 1);
    }

    #[test]
    fn set_block_replaces_in_place() {
        let mut d = doc(SAMPLE);
        d.set_block("title", vec!["title: New".into()]);
        assert_eq!(
            d.render(),
            SAMPLE.replace("title: Sample # comment kept", "title: New")
        );
    }

    #[test]
    fn set_block_inserts_by_key_order() {
        let mut d = doc(SAMPLE);
        d.set_block("generated", vec!["generated: { by: x, at: y }".into()]);
        // After tags (rank 5), before sources (rank 12).
        assert_eq!(d.frontmatter()[4], "generated: { by: x, at: y }");
        d.set_block("description", vec!["description: D".into()]);
        assert_eq!(d.frontmatter()[2], "description: D");
        d.set_block("custom", vec!["custom: c".into()]);
        assert_eq!(d.frontmatter().last().unwrap(), "custom: c");
    }

    #[test]
    fn set_block_keeps_trailing_blank_lines_last() {
        let mut d = doc("---\nx_custom: 1\n\n---\n");
        d.set_block("other", vec!["other: 2".into()]);
        assert_eq!(d.frontmatter(), ["x_custom: 1", "other: 2", ""]);
    }

    #[test]
    fn delete_block_removes_continuations() {
        let mut d = doc(SAMPLE);
        d.delete_block("sources");
        assert_eq!(d.frontmatter().len(), 4);
        d.delete_block("missing");
        assert_eq!(d.frontmatter().len(), 4);
    }

    #[test]
    fn seq_items_finds_spans() {
        let d = doc("---\nq:\n  - \"one\"\n  - two\n    cont\n---\n");
        assert_eq!(d.seq_items("q"), Some((2, vec![(1, 2), (2, 4)])));
        assert_eq!(doc("---\nq: [a]\n---\n").seq_items("q"), None);
        assert_eq!(doc("---\nq:\n  a: b\n---\n").seq_items("q"), None);
    }

    #[test]
    fn append_creates_missing_key() {
        let mut d = doc("---\ntype: Note\n---\n");
        d.append_seq_item(
            "open_questions",
            quoted_item("Why?"),
            None,
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["type: Note", "open_questions:", "  - \"Why?\""]
        );
    }

    #[test]
    fn append_extends_block_list() {
        let mut d = doc(SAMPLE);
        let current = d.parse().unwrap()["sources"].clone();
        let entry = Value::Map(
            [("id".to_string(), Value::Str("two".into()))]
                .into_iter()
                .collect(),
        );
        d.append_seq_item(
            "sources",
            |i| block_item(&entry, i),
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(d.frontmatter()[7], "  - id: two");
        assert!(d.render_checked().is_ok());
    }

    #[test]
    fn append_converts_flow_list() {
        let mut d = doc("---\nverified: [{ by: a, at: b }]\n---\n");
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- {{ by: c, at: d }}", " ".repeat(i))],
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            [
                "verified:",
                "  - by: a",
                "    at: b",
                "  - { by: c, at: d }"
            ]
        );
    }

    #[test]
    fn append_converts_bare_mappings() {
        let mut d = doc("---\nverified: { by: a, at: b }\n---\n");
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["verified:", "  - { by: a, at: b }", "  - new"]
        );

        let mut d = doc("---\nverified:\n  by: a\n  at: b\n---\n");
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["verified:", "  - by: a", "    at: b", "  - new"]
        );
        assert!(d.render_checked().is_ok());
    }

    #[test]
    fn append_refuses_scalar() {
        let mut d = doc("---\nsources: nope\n---\n");
        let current = d.parse().unwrap()["sources"].clone();
        let err = d
            .append_seq_item("sources", |_| vec![], Some(&current), render_block_item)
            .unwrap_err();
        assert!(matches!(err, OkfError::Shape { .. }));
    }

    #[test]
    fn render_checked_refuses_broken_frontmatter() {
        let mut d = doc(SAMPLE);
        d.set_block("title", vec!["title: a: b".into()]);
        assert!(matches!(
            d.render_checked(),
            Err(OkfError::FrontmatterParse { line: 3, .. })
        ));
    }

    #[hegel::test]
    fn set_block_changes_only_the_target_key(tc: hegel::TestCase) {
        use hegel::generators;
        let values: Vec<String> =
            tc.draw(generators::vecs(generators::text()).min_size(1).max_size(8));
        let target = tc.draw(generators::integers::<usize>().max_value(values.len() - 1));
        let replacement: String = tc.draw(generators::text());
        let lines: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("k{i}: {}", crate::okf::yaml::scalar(v, false, false)))
            .collect();
        let mut d = doc(&format!("---\n{}\n---\nbody\n", lines.join("\n")));
        let new_line = format!(
            "k{target}: {}",
            crate::okf::yaml::scalar(&replacement, false, false)
        );
        d.set_block(&format!("k{target}"), vec![new_line.clone()]);
        for (i, line) in d.frontmatter().iter().enumerate() {
            let expected = if i == target { &new_line } else { &lines[i] };
            assert_eq!(line, expected);
        }
        let parsed = d.parse().unwrap();
        assert_eq!(parsed[&format!("k{target}")], Value::Str(replacement));
    }

    #[hegel::test]
    fn from_text_render_is_identity(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        assert_eq!(doc(&text).render(), text);
    }
}
```

- [ ] **Step 3: Run the tests and check they fail**

Run: `cargo test --lib okf::doc`
Expected: compile errors (`cannot find type Doc`, ...).

- [ ] **Step 4: Implement.** Insert above the tests module in `src/okf/doc.rs`:

````rust
//! A markdown document split into frontmatter lines and everything else, with
//! textual edits that change only the key being written.
//!
//! Port of `okf.py`'s `Doc` and `append_seq_item`. Every byte outside the
//! edited key's lines is preserved, including comments, blank lines, key
//! order and the file's newline style.
//!
//! Divergence from `okf.py`: only `\n` and `\r\n` end lines. Python's
//! `splitlines` also splits on `\x0b`, `\x0c`, `\x1c`-`\x1e`, `\x85`,
//! U+2028 and U+2029, which would corrupt a document containing them.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::error::OkfError;
use crate::okf::yaml::{
    block_item, indent_of, is_seq_start, parse_value, strip_comment, NumberedLine, Value, YamlError,
};

/// Preferred position of top-level keys, used only when a key is created.
/// Existing keys are never moved.
pub const KEY_ORDER: [&str; 13] = [
    "type",
    "title",
    "description",
    "resource",
    "aliases",
    "tags",
    "status",
    "generated",
    "stale_after",
    "verified",
    "reviewed",
    "open_questions",
    "sources",
];

/// A top-level `key:` line (the start of a frontmatter block).
static TOP_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z_][\w.\-]*)[ \t]*:(?:[ \t]|$)").expect("TOP_KEY regex is valid")
});

/// One top-level key's span in [`Doc::frontmatter`]: `lines[start]` is the
/// `key:` line and `lines[start + 1..end]` its continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The key.
    pub key: String,
    /// Index of the `key:` line.
    pub start: usize,
    /// One past the last continuation line.
    pub end: usize,
}

/// A parsed markdown file: opening delimiter, frontmatter lines, and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    path: String,
    head: Vec<String>,
    frontmatter: Vec<String>,
    tail: Vec<String>,
    newline: &'static str,
    has_frontmatter: bool,
}

/// Splits text into lines that keep their `\n` (Python's `splitlines(keepends=True)`
/// restricted to `\n` and `\r\n`).
fn lines_with_endings(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_string).collect()
}

fn strip_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

impl Doc {
    /// Splits `text` into frontmatter and the rest.
    ///
    /// Frontmatter is recognized when the first line is `---` and a later
    /// line is `---` or `...`; otherwise the whole text is the tail.
    ///
    /// # Arguments
    /// * `path` - catalog-relative path, used in error messages.
    /// * `text` - the file's full contents.
    ///
    /// # Examples
    /// ```
    /// use okf_mcp::okf::doc::Doc;
    /// let doc = Doc::from_text("a.md", "---\ntype: Note\n---\nBody\n");
    /// assert!(doc.has_frontmatter());
    /// assert_eq!(doc.render(), "---\ntype: Note\n---\nBody\n");
    /// ```
    pub fn from_text(path: &str, text: &str) -> Self {
        let lines = lines_with_endings(text);
        let newline = if lines.first().is_some_and(|l| l.ends_with("\r\n")) {
            "\r\n"
        } else {
            "\n"
        };
        let mut doc = Self {
            path: path.to_string(),
            head: Vec::new(),
            frontmatter: Vec::new(),
            tail: lines.clone(),
            newline,
            has_frontmatter: false,
        };
        if lines.first().map(|l| strip_line_ending(l)) != Some("---") {
            return doc;
        }
        let closing = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, l)| matches!(strip_line_ending(l).trim_end(), "---" | "..."));
        if let Some((j, _)) = closing {
            doc.has_frontmatter = true;
            doc.head = lines[..1].to_vec();
            doc.frontmatter = lines[1..j]
                .iter()
                .map(|l| strip_line_ending(l).to_string())
                .collect();
            doc.tail = lines[j..].to_vec();
        }
        doc
    }

    /// The catalog-relative path given to [`Doc::from_text`].
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether the document has a frontmatter block.
    pub fn has_frontmatter(&self) -> bool {
        self.has_frontmatter
    }

    /// The frontmatter lines, without line endings or delimiters.
    pub fn frontmatter(&self) -> &[String] {
        &self.frontmatter
    }

    /// The top-level key blocks, in file order.
    pub fn blocks(&self) -> Vec<Block> {
        let lines = &self.frontmatter;
        let mut blocks = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let Some(caps) = TOP_KEY.captures(&lines[i]) else {
                i += 1;
                continue;
            };
            let mut last = i;
            let mut j = i + 1;
            while j < lines.len() {
                let line = &lines[j];
                if line.trim().is_empty() {
                    j += 1;
                    continue;
                }
                if line.starts_with([' ', '\t']) || is_seq_start(line) {
                    last = j;
                    j += 1;
                    continue;
                }
                break;
            }
            blocks.push(Block {
                key: caps[1].to_string(),
                start: i,
                end: last + 1,
            });
            i = last + 1;
        }
        blocks
    }

    /// The block for `key`, if present.
    pub fn block(&self, key: &str) -> Option<Block> {
        self.blocks().into_iter().find(|b| b.key == key)
    }

    /// Parses the frontmatter into an ordered map.
    ///
    /// # Errors
    /// [`YamlError`] for a duplicate key, an unparseable value, or a line
    /// that belongs to no key. Line numbers are 1-based file lines.
    pub fn parse(&self) -> Result<IndexMap<String, Value>, YamlError> {
        parse_frontmatter_lines(&self.frontmatter)
    }

    /// Parses the frontmatter, converting a failure into [`OkfError::FrontmatterParse`].
    ///
    /// # Errors
    /// [`OkfError::FrontmatterParse`] with this document's path.
    pub fn parse_checked(&self) -> Result<IndexMap<String, Value>, OkfError> {
        self.parse().map_err(|e| self.parse_error(e))
    }

    fn parse_error(&self, error: YamlError) -> OkfError {
        OkfError::FrontmatterParse {
            path: self.path.clone(),
            line: error.line.unwrap_or(1),
            message: error.message,
        }
    }

    /// The 1-based file line of `key` (1 when absent), for lint messages.
    pub fn key_line(&self, key: &str) -> usize {
        self.block(key).map_or(1, |b| b.start + 2)
    }

    /// Reassembles the file text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.extend(self.head.iter().map(String::as_str));
        for line in &self.frontmatter {
            out.push_str(line);
            out.push_str(self.newline);
        }
        out.extend(self.tail.iter().map(String::as_str));
        out
    }

    /// Reassembles the file text after checking the frontmatter still parses.
    ///
    /// # Errors
    /// [`OkfError::FrontmatterParse`] if an edit broke the frontmatter; the
    /// caller must then not write the file.
    pub fn render_checked(&self) -> Result<String, OkfError> {
        self.parse_checked()?;
        Ok(self.render())
    }

    /// Replaces `key`'s lines, or inserts them at the [`KEY_ORDER`] position.
    ///
    /// # Arguments
    /// * `key` - the top-level key.
    /// * `new_lines` - the replacement lines, starting with `key:`.
    pub fn set_block(&mut self, key: &str, new_lines: Vec<String>) {
        let blocks = self.blocks();
        if let Some(b) = blocks.iter().find(|b| b.key == key) {
            self.frontmatter.splice(b.start..b.end, new_lines);
            return;
        }
        let mut position = self.frontmatter.len();
        // Trailing blank lines at the end of the frontmatter stay at the end.
        while position > 0 && self.frontmatter[position - 1].trim().is_empty() {
            position -= 1;
        }
        if let Some(rank) = KEY_ORDER.iter().position(|k| *k == key) {
            let rank_of = |k: &str| KEY_ORDER.iter().position(|o| *o == k);
            let before = blocks
                .iter()
                .filter(|b| rank_of(&b.key).is_some_and(|r| r < rank))
                .map(|b| b.end)
                .max();
            let after = blocks
                .iter()
                .filter(|b| rank_of(&b.key).is_some_and(|r| r > rank))
                .map(|b| b.start)
                .min();
            if let Some(end) = before {
                position = end;
            } else if let Some(start) = after {
                position = start;
            }
        }
        self.frontmatter.splice(position..position, new_lines);
    }

    /// Removes `key`'s lines, if present.
    pub fn delete_block(&mut self, key: &str) {
        if let Some(b) = self.block(key) {
            self.frontmatter.drain(b.start..b.end);
        }
    }

    /// For a block-list key: the item indent and each item's `(start, end)` span.
    ///
    /// Returns `None` when the key is absent, has an inline value, or its
    /// continuation isn't a plain block list.
    pub fn seq_items(&self, key: &str) -> Option<(usize, Vec<(usize, usize)>)> {
        let b = self.block(key)?;
        let inline = self.frontmatter[b.start]
            .split_once(':')
            .map_or("", |(_, v)| v);
        if !strip_comment(inline).is_empty() {
            return None;
        }
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut item_indent: Option<usize> = None;
        for i in b.start + 1..b.end {
            let line = &self.frontmatter[i];
            if line.trim().is_empty() || line.trim().starts_with('#') {
                continue;
            }
            // `indent` counts ASCII spaces, so slicing at it is always a char boundary.
            let indent = indent_of(line);
            let expected = match item_indent {
                Some(expected) => expected,
                None => {
                    if !is_seq_start(&line[indent..]) {
                        return None;
                    }
                    item_indent = Some(indent);
                    indent
                }
            };
            if indent == expected && is_seq_start(&line[indent..]) {
                spans.push((i, i + 1));
            } else if indent > expected {
                // A continuation of the last item; `?` returns None when there is none.
                spans.last_mut()?.1 = i + 1;
            } else {
                return None;
            }
        }
        Some((item_indent.unwrap_or(2), spans))
    }

    /// Appends an item to a list-valued key, creating or normalizing the key.
    ///
    /// Port of `okf.py`'s `append_seq_item`. A missing or null key becomes a
    /// new block list; a block list gets the item after its last item; a bare
    /// mapping becomes the first item of a new list; a flow list is
    /// rewritten as a block list (each old item via `render_item`).
    ///
    /// # Arguments
    /// * `key` - the list key (`verified`, `sources`, `open_questions`).
    /// * `item_lines` - returns the new item's lines for a given dash indent.
    /// * `current` - the key's parsed value (`None` if absent or null).
    /// * `render_item` - renders an existing item when a flow list is rewritten.
    ///
    /// # Errors
    /// [`OkfError::Shape`] when the key is a scalar.
    pub fn append_seq_item(
        &mut self,
        key: &str,
        item_lines: impl Fn(usize) -> Vec<String>,
        current: Option<&Value>,
        render_item: impl Fn(&Value, usize) -> Vec<String>,
    ) -> Result<(), OkfError> {
        let block = self.block(key);
        let (Some(b), Some(value)) = (block, current.filter(|v| **v != Value::Null)) else {
            let mut lines = vec![format!("{key}:")];
            lines.extend(item_lines(2));
            self.set_block(key, lines);
            return Ok(());
        };
        if let (Some((indent, spans)), Value::Seq(_)) = (self.seq_items(key), value) {
            let end = spans.last().map_or(b.end, |s| s.1);
            self.frontmatter.splice(end..end, item_lines(indent));
            return Ok(());
        }
        let inline = strip_comment(
            self.frontmatter[b.start]
                .split_once(':')
                .map_or("", |(_, v)| v),
        )
        .to_string();
        match value {
            Value::Map(_) => {
                let mut lines = vec![format!("{key}:")];
                let new_indent = if inline.is_empty() {
                    // A bare block mapping: indent it under `- `.
                    let kids = &self.frontmatter[b.start + 1..b.end];
                    let indent = kids
                        .iter()
                        .filter(|t| !t.trim().is_empty())
                        .map(|t| indent_of(t))
                        .min()
                        .unwrap_or(2);
                    let mut first = true;
                    for line in kids {
                        if line.trim().is_empty() {
                            lines.push(line.clone());
                        } else if first && indent_of(line) == indent {
                            lines.push(format!("{}- {}", " ".repeat(indent), &line[indent..]));
                            first = false;
                        } else {
                            lines.push(format!("  {line}"));
                        }
                    }
                    indent
                } else {
                    // A bare flow mapping: keep its text as the first item.
                    lines.push(format!("  - {inline}"));
                    2
                };
                lines.extend(item_lines(new_indent));
                self.frontmatter.splice(b.start..b.end, lines);
                Ok(())
            }
            Value::Seq(items) => {
                let mut lines = vec![format!("{key}:")];
                for item in items {
                    lines.extend(render_item(item, 2));
                }
                lines.extend(item_lines(2));
                self.frontmatter.splice(b.start..b.end, lines);
                Ok(())
            }
            Value::Null | Value::Str(_) => Err(OkfError::Shape {
                path: self.path.clone(),
                key: key.to_string(),
            }),
        }
    }
}

/// [`block_item`] as a `render_item` callback for [`Doc::append_seq_item`].
pub fn render_block_item(value: &Value, indent: usize) -> Vec<String> {
    block_item(value, indent)
}

/// Parses frontmatter lines (no delimiters) the way `Doc.parse` does.
fn parse_frontmatter_lines(lines: &[String]) -> Result<IndexMap<String, Value>, YamlError> {
    let doc = Doc {
        path: String::new(),
        head: Vec::new(),
        frontmatter: lines.to_vec(),
        tail: Vec::new(),
        newline: "\n",
        has_frontmatter: true,
    };
    let mut data = IndexMap::new();
    let mut covered = vec![false; lines.len()];
    for b in doc.blocks() {
        if data.contains_key(&b.key) {
            return Err(YamlError {
                message: format!("duplicate key {:?}", b.key),
                line: Some(b.start + 2),
            });
        }
        let inline = lines[b.start].split_once(':').map_or("", |(_, v)| v);
        let children: Vec<NumberedLine<'_>> = lines[b.start + 1..b.end]
            .iter()
            .enumerate()
            .map(|(i, t)| (b.start + 2 + i, t.as_str()))
            .collect();
        let value = parse_value(inline, &children, b.start + 2)?;
        data.insert(b.key.clone(), value);
        covered[b.start..b.end].iter_mut().for_each(|c| *c = true);
    }
    for (i, line) in lines.iter().enumerate() {
        if !covered[i] && !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            return Err(YamlError {
                message: format!("line is not part of any key: {:?}", line.trim()),
                line: Some(i + 2),
            });
        }
    }
    Ok(data)
}
````

- [ ] **Step 5: Run the tests and check they pass**

Run: `cargo test okf::doc`
Expected: PASS (20 unit tests, including 2 property tests, and 1 doc test).

- [ ] **Step 6: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/okf/mod.rs src/okf/doc.rs
git commit -m "Port okf.py's Doc: frontmatter parsing and byte-preserving edits

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Markdown structure (`okf::markdown`)

Port of `okf.py`'s `LINK_RE`, `split_frontmatter`, `headings`, `slugify`, `heading_slugs`, `anchors_in`, `body_split`, `footnote_defs` and `resolve_link` (lines 820-930). `LINK_RE` and the emphasis pattern in `slugify` need lookbehind, so they use `fancy_regex`; everything else uses `regex`. Filtering image matches after a plain-regex search would not be equivalent: Python retries at the next position and finds links nested inside image text (the test `find_links_skips_images_and_keeps_inner_links` checks this).

**Files:**
- Create: `src/okf/markdown.rs`
- Modify: `src/okf/mod.rs`

- [ ] **Step 1: Register the module.** In `src/okf/mod.rs`, make the module list (alphabetical):

```rust
pub mod doc;
pub mod markdown;
pub mod paths;
pub mod workspace;
pub mod yaml;
```

- [ ] **Step 2: Write the failing tests.** Create `src/okf/markdown.rs` containing only the tests:

````rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    #[test]
    fn find_links_skips_images_and_keeps_inner_links() {
        let links = find_links("![a [b](c)](d) and [e](f \"title\") [g\\]h](i)");
        let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["c", "f", "i"]);
        assert_eq!(links[2].text, "g\\]h");
        let line = "x [t](/a.md) y";
        let link = &find_links(line)[0];
        assert_eq!(&line[link.range.clone()], "[t](/a.md)");
        assert_eq!(&line[link.target_range.clone()], "/a.md");
    }

    #[test]
    fn split_frontmatter_cases() {
        assert_eq!(
            split_frontmatter("---\na: 1\n---\nbody\n"),
            ("a: 1", "body\n", 3)
        );
        assert_eq!(split_frontmatter("---\na: 1\n---"), ("a: 1", "", 2));
        assert_eq!(split_frontmatter("no fm\n"), ("", "no fm\n", 0));
    }

    #[test]
    fn body_split_cases() {
        let (head, body) = body_split("---\na: 1\n...\nbody");
        assert_eq!(head, ["---", "a: 1", "..."]);
        assert_eq!(body, ["body"]);
        let (head, body) = body_split("plain\ntext");
        assert!(head.is_empty());
        assert_eq!(body, ["plain", "text"]);
    }

    #[test]
    fn headings_skip_fences_and_number_repeats() {
        let lines = [
            "# Intro",
            "```",
            "# not a heading",
            "~~~",
            "# still code",
            "```",
            "## Intro ##",
            "### Intro",
            "#nospace",
            "####### seven",
        ];
        let found = headings(&lines);
        let summary: Vec<_> = found
            .iter()
            .map(|h| (h.index, h.level, h.slug.as_str()))
            .collect();
        assert_eq!(
            summary,
            [(0, 1, "intro"), (6, 2, "intro-1"), (7, 3, "intro-2")]
        );
        assert_eq!(found[1].text, "Intro");
    }

    #[test]
    fn repeat_numbering_avoids_existing_slugs() {
        let found = headings(&["# x-1", "# x", "# x"]);
        let slugs: Vec<_> = found.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(slugs, ["x-1", "x", "x-1"]);
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(
            slugify("Plan: CLI first, MCP later"),
            "plan-cli-first-mcp-later"
        );
        assert_eq!(slugify("<em>Tagged</em> heading"), "tagged-heading");
        assert_eq!(slugify("__dunder__ name"), "dunder-name");
        assert_eq!(slugify("Ünïcode Wörds"), "ünïcode-wörds");
    }

    #[test]
    fn anchors_include_html() {
        let anchors =
            anchors_in("---\nt: x\n---\n# Top\n<a name=\"custom\"></a>\n<A ID='Other'>\n");
        assert_eq!(
            anchors.into_iter().collect::<Vec<_>>(),
            ["Other", "custom", "top"]
        );
    }

    #[test]
    fn footnotes_with_continuations() {
        let lines = [
            "Text[^a].",
            "",
            "[^a]: first",
            "    more",
            "[^b]: other",
            "not indented",
            "\tlate",
        ];
        let defs = footnote_defs(&lines);
        assert_eq!(defs["a"], [2, 3]);
        assert_eq!(defs["b"], [4]);
        assert_eq!(defs.len(), 2);
    }

    #[test]
    fn resolve_link_cases() {
        let ws = MemWorkspace::from_files([("infra/index.md", ""), ("infra/x.md", "")]);
        let resolve = |link: &str, src: &str| resolve_link(&ws, link, src);
        assert_eq!(
            resolve("/infra/x.md#a", "p/q.md"),
            Some(ResolvedLink {
                path: "infra/x.md".into(),
                absolute: true
            })
        );
        assert_eq!(
            resolve("x.md", "infra/index.md"),
            Some(ResolvedLink {
                path: "infra/x.md".into(),
                absolute: false
            })
        );
        assert_eq!(
            resolve("../infra/", "people/y.md").unwrap().path,
            "infra/index.md"
        );
        assert_eq!(resolve("/infra", "y.md").unwrap().path, "infra/index.md");
        assert_eq!(resolve("https://example.com", "y.md"), None);
        assert_eq!(resolve("mailto:a@b", "y.md"), None);
        assert_eq!(resolve("#frag", "y.md"), None);
    }

    #[hegel::test]
    fn slugify_output_has_no_spaces_or_uppercase_ascii(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let slug = slugify(&text);
        assert!(!slug.contains(' '));
        assert!(!slug.chars().any(|c| c.is_ascii_uppercase()));
    }
}
````

- [ ] **Step 3: Run the tests and check they fail**

Run: `cargo test --lib okf::markdown`
Expected: compile errors (`cannot find function find_links`, ...).

- [ ] **Step 4: Implement.** Insert above the tests module in `src/okf/markdown.rs`:

````rust
//! Markdown structure the catalog tools need: frontmatter/body splits, ATX
//! headings and their GitHub anchor slugs, HTML anchors, footnotes, and
//! links with their resolution to bundle paths.
//!
//! Port of the corresponding helpers in `tools/okf.py`. Patterns that need
//! lookaround use `fancy_regex`; the rest use the faster `regex` crate.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::okf::paths::{dirname, join, normpath};
use crate::okf::workspace::Workspace;

/// A markdown link `[text](target "title")`, not an image (`![...]`).
static LINK: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r#"(?<!!)\[((?:[^\]\\]|\\.)*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#)
        .expect("LINK regex is valid")
});
static HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$").expect("HEADING regex is valid")
});
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{0,3}(```|~~~)").expect("FENCE regex is valid"));
static FOOTNOTE_DEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\^([^\]]+)\]:").expect("FOOTNOTE_DEF regex is valid"));
static HTML_ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<a\s[^>]*(?:name|id)=["']([^"']+)["']"#).expect("HTML_ANCHOR regex is valid")
});
static URI_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("URI_SCHEME regex is valid"));
static SLUG_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!?\[((?:[^\]\\]|\\.)*)\]\([^)]*\)").expect("SLUG_LINK regex is valid")
});
static SLUG_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("SLUG_TAG regex is valid"));
static SLUG_EMPHASIS: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"\*+|(?<!\w)_+|_+(?!\w)").expect("SLUG_EMPHASIS regex is valid")
});
static SLUG_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\w\- ]").expect("SLUG_PUNCTUATION regex is valid"));

/// One link occurrence in a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMatch {
    /// The link text between the brackets.
    pub text: String,
    /// The raw target between the parentheses.
    pub target: String,
    /// Byte range of the whole `[text](target)` in the line.
    pub range: std::ops::Range<usize>,
    /// Byte range of the target within the line.
    pub target_range: std::ops::Range<usize>,
}

/// Finds every markdown link (not image) in `text`, left to right.
///
/// # Examples
/// ```
/// use okf_mcp::okf::markdown::find_links;
/// let links = find_links("See [the server](/infra/x.md#ports) and ![img](a.png).");
/// assert_eq!(links.len(), 1);
/// assert_eq!(links[0].target, "/infra/x.md#ports");
/// ```
pub fn find_links(text: &str) -> Vec<LinkMatch> {
    LINK.captures_iter(text)
        // fancy_regex reports backtracking-limit failures per match; `flatten`
        // drops those (a link too pathological to parse is not a link).
        .flatten()
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            let label = caps.get(1)?;
            let target = caps.get(2)?;
            Some(LinkMatch {
                text: label.as_str().to_string(),
                target: target.as_str().to_string(),
                range: whole.range(),
                target_range: target.range(),
            })
        })
        .collect()
}

/// Splits a document's text into `(frontmatter, body, number of lines before the body)`.
///
/// Port of `okf.py`'s `split_frontmatter`: frontmatter needs a `---\n`
/// first line and a later line starting with `---`.
pub fn split_frontmatter(text: &str) -> (&str, &str, usize) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---").map(|p| p + 4) {
            let body_start = text[end + 4..]
                .find('\n')
                .map_or(text.len(), |p| end + 4 + p + 1);
            let offset = text[..body_start].matches('\n').count();
            return (&text[4..end], &text[body_start..], offset);
        }
    }
    ("", text, 0)
}

/// Splits a document's text on `\n` into (frontmatter lines with delimiters, body lines).
///
/// Port of `okf.py`'s `body_split`. Without frontmatter the first part is empty.
pub fn body_split(text: &str) -> (Vec<&str>, Vec<&str>) {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first().map(|l| l.trim_end_matches('\r')) == Some("---") {
        if let Some(j) = (1..lines.len())
            .find(|&j| matches!(lines[j].trim_end_matches('\r').trim_end(), "---" | "..."))
        {
            return (lines[..=j].to_vec(), lines[j + 1..].to_vec());
        }
    }
    (Vec::new(), lines)
}

/// An ATX heading found in a list of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Index of the heading's line.
    pub index: usize,
    /// 1 to 6.
    pub level: usize,
    /// The heading text, trimmed, without closing `#`s.
    pub text: String,
    /// Its GitHub anchor slug, numbered for repeats (`x`, `x-1`, `x-2`).
    pub slug: String,
}

/// The ATX headings in `lines`, skipping fenced code, with their anchor slugs.
pub fn headings(lines: &[&str]) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut fence: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(caps) = FENCE.captures(line) {
            let marker = caps.get(1).map_or("", |m| m.as_str());
            // A fence closes only with the same marker that opened it.
            fence = match fence {
                Some(open) if open == marker => None,
                Some(open) => Some(open),
                None => Some(marker),
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some(caps) = HEADING.captures(line) {
            let level = caps[1].len();
            let text = caps.get(2).map_or("", |m| m.as_str()).trim().to_string();
            found.push(Heading {
                index,
                level,
                text,
                slug: String::new(),
            });
        }
    }
    let mut seen: IndexMap<String, usize> = IndexMap::new();
    for heading in &mut found {
        let base = slugify(&heading.text);
        let slug = match seen.get(&base) {
            Some(count) => format!("{base}-{count}"),
            None => base.clone(),
        };
        *seen.entry(base).or_insert(0) += 1;
        seen.entry(slug.clone()).or_insert(1);
        heading.slug = slug;
    }
    found
}

/// GitHub's heading anchor: rendered text, lowercased, punctuation dropped,
/// spaces turned into `-`.
///
/// # Examples
/// ```
/// use okf_mcp::okf::markdown::slugify;
/// assert_eq!(slugify("What it *wouldn't* change"), "what-it-wouldnt-change");
/// assert_eq!(slugify("Use `okf.py` [here](/x.md)"), "use-okfpy-here");
/// assert_eq!(slugify("snake_case stays"), "snake_case-stays");
/// ```
pub fn slugify(text: &str) -> String {
    let text = SLUG_LINK.replace_all(text, "$1");
    let text = SLUG_TAG.replace_all(&text, "").replace('`', "");
    let text = SLUG_EMPHASIS.replace_all(&text, "");
    let lowered = text.trim().to_lowercase();
    SLUG_PUNCTUATION.replace_all(&lowered, "").replace(' ', "-")
}

/// Every fragment a link may target in a document: heading slugs plus HTML
/// `<a name|id>` anchors.
pub fn anchors_in(text: &str) -> BTreeSet<String> {
    let (_, body, _) = split_frontmatter(text);
    let lines: Vec<&str> = body.split('\n').collect();
    let mut anchors: BTreeSet<String> = headings(&lines).into_iter().map(|h| h.slug).collect();
    anchors.extend(HTML_ANCHOR.captures_iter(body).map(|c| c[1].to_string()));
    anchors
}

/// Footnote definitions: label to the line indexes of its definition and
/// indented continuation lines, in first-definition order.
pub fn footnote_defs(lines: &[&str]) -> IndexMap<String, Vec<usize>> {
    let mut defs: IndexMap<String, Vec<usize>> = IndexMap::new();
    let mut current: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(caps) = FOOTNOTE_DEF.captures(line) {
            let label = caps[1].to_string();
            defs.entry(label.clone()).or_default().push(index);
            current = Some(label);
        } else if let (Some(label), true) =
            (&current, line.starts_with("    ") || line.starts_with('\t'))
        {
            if !line.trim().is_empty() {
                defs.entry(label.clone()).or_default().push(index);
            } else {
                current = None;
            }
        } else {
            current = None;
        }
    }
    defs
}

/// Where a link points inside the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    /// Bundle-relative target path (a directory link becomes its `index.md`).
    pub path: String,
    /// Was the link written bundle-absolute (`/dir/x.md`)?
    pub absolute: bool,
}

/// Resolves a link target written in `source` to a bundle path.
///
/// # Arguments
/// * `workspace` - the catalog, to tell directories from files.
/// * `link` - the raw link target, possibly with a `#fragment`.
/// * `source` - bundle-relative path of the document containing the link.
///
/// # Returns
/// `None` for external links (any URI scheme) and pure `#fragment` links.
pub fn resolve_link(workspace: &dyn Workspace, link: &str, source: &str) -> Option<ResolvedLink> {
    let target = link.split('#').next().unwrap_or_default();
    if target.is_empty() || URI_SCHEME.is_match(target) {
        return None;
    }
    let absolute = target.starts_with('/');
    let mut path = if absolute {
        target.trim_start_matches('/').to_string()
    } else {
        normpath(&join(dirname(source), target))
    };
    if target.ends_with('/') || workspace.is_dir(&path) {
        path = join(&path, "index.md");
    }
    Some(ResolvedLink {
        path: normpath(&path),
        absolute,
    })
}
````

- [ ] **Step 5: Run the tests and check they pass**

Run: `cargo test okf::markdown`
Expected: PASS (10 unit tests, including 1 property test, and 2 doc tests).

- [ ] **Step 6: Lint, check the docs, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
cargo doc --no-deps   # expected: no warnings now that every intra-doc link resolves
git add src/okf/mod.rs src/okf/markdown.rs
git commit -m "Port okf.py's markdown helpers: links, headings, slugs, footnotes

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Parity harness against `okf.py`

The shim loads `okf.py` from `$OKF_PY` and prints its internals as JSON. It sets `sys.dont_write_bytecode` so it never leaves a `__pycache__` in a catalog checkout. Golden files are generated **from `okf.py`**, never from the Rust code, so they are an independent oracle. The real catalog is private and is never copied into this repository; the live tests read it from `OKF_PARITY_CATALOG`.

The shim is a stdlib-only test helper run with the system `python3 -I` (isolated mode), the same interpreter `okf.py` targets. The project's PyO3/`uv` rules cover Python packages built from this crate, which this isn't.

**Files:**
- Create: `tests/parity/okf_shim.py`, `tests/parity.rs`, the fixture corpus under `tests/fixtures/catalog/`, and `tests/golden/{frontmatter,headings,render}.json` (generated)

- [ ] **Step 1: Create the shim** `tests/parity/okf_shim.py`:

```python
#!/usr/bin/env python3
"""Expose tools/okf.py internals as JSON for the Rust parity tests.

Loads okf.py from the path in $OKF_PY (standard library only, no install).

    okf_shim.py frontmatter ROOT     every .md under ROOT: parsed frontmatter or error line
    okf_shim.py headings ROOT        every .md under ROOT: body headings with slugs
    okf_shim.py render               stdin: JSON list of values; stdout: flow/block renderings
    okf_shim.py run --now TS ARG...  run okf.py's CLI with now_utc() frozen at TS (later milestones)
"""
import importlib.util
import json
import os
import sys
from datetime import datetime


def load_okf():
    # Never leave a __pycache__ next to okf.py in someone's catalog checkout.
    sys.dont_write_bytecode = True
    path = os.environ.get("OKF_PY")
    if not path or not os.path.isfile(path):
        sys.exit("okf_shim: set OKF_PY to the path of tools/okf.py")
    spec = importlib.util.spec_from_file_location("okf", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def frontmatter(okf, root):
    out = []
    for rel in okf.walk_md(root):
        doc = okf.Doc(os.path.join(root, rel))
        entry = {"path": rel, "has_frontmatter": doc.has_fm}
        if doc.has_fm:
            try:
                entry["value"] = doc.parse()
            except okf.YamlError as ex:
                entry["error_line"] = ex.line
        out.append(entry)
    return out


def headings(okf, root):
    out = []
    for rel in okf.walk_md(root):
        with open(os.path.join(root, rel), encoding="utf-8") as fh:
            text = fh.read()
        _, body = okf.body_split(text)
        out.append({"path": rel,
                    "headings": [[i, level, t, slug] for i, level, t, slug in okf.heading_slugs(body)]})
    return out


def render(okf, values):
    return [{"flow": okf.flow_value(v), "block": okf.block_item(v, 2)} for v in values]


def main():
    okf = load_okf()
    command, args = sys.argv[1], sys.argv[2:]
    if command == "frontmatter":
        result = frontmatter(okf, args[0])
    elif command == "headings":
        result = headings(okf, args[0])
    elif command == "render":
        result = render(okf, json.load(sys.stdin))
    elif command == "run":
        frozen = datetime.fromisoformat(args[1].replace("Z", "+00:00"))
        okf.now_utc = lambda: frozen
        okf.main(args[2:])
        return
    else:
        sys.exit(f"okf_shim: unknown command {command!r}")
    json.dump(result, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Create the synthetic corpus.** Every file below is a parity case: flow and block collections, quoting and escapes, block scalars, comments, CRLF, `...` closers, multibyte text, a hidden directory that must be skipped, and one file for each parse error.

`tests/fixtures/catalog/.claude/hidden.md`:

```markdown
---
type: Hidden
---
```

`tests/fixtures/catalog/AGENTS.md`:

```markdown
# Agents

Machinery, no frontmatter.
```

`tests/fixtures/catalog/index.md`:

```markdown
---
okf_version: "0.2"
---

# Fixture catalog

* [Infra](infra/) - systems
```

`tests/fixtures/catalog/infra/hosts/crlf-host.md` (CRLF line endings, so it is written with `printf`):

```bash
printf -- '---\r\ntype: Host\r\ntitle: CRLF host\r\n...\r\nBody with CRLF.\r\n' \
    > tests/fixtures/catalog/infra/hosts/crlf-host.md
```

`tests/fixtures/catalog/infra/hosts/no-frontmatter.md`:

```markdown
Just a body, no frontmatter.

# Heading `code` and *emphasis*
```

`tests/fixtures/catalog/infra/index.md`:

```markdown
# Infra

# Concepts

* [Time server](time-server.md) - GPS clock
```

`tests/fixtures/catalog/infra/time-server.md`:

````markdown
---
type: Service
title: Time server # trailing comment
description: "GPS stratum 1: the network's clock"
resource: ntp://mosaic.internal
aliases: [mosaic, 'gps clock', "the \"clock\""]
tags:
  - time
  - gps
status: stable
generated: { by: claude-code/claude-opus-5-5, at: 2026-10-26T23:01:11Z }
verified:
  - { by: "human:pgrace", at: 2026-10-25T10:00:00Z }
  - by: claude-code/claude-opus-5-5
    at: 2026-10-26T01:02:03Z
reviewed:
  by: claude-code/claude-opus-5-5
  at: 2026-10-26T02:00:00Z
  outcome: verified
  note: "Checked chrony: offset < 1 ms"
open_questions:
  - "Is the antenna on the roof?"
sources:
  - id: chrony
    resource: chronyc tracking
    title: Live check
  - id: empty-title
    resource: x
    title:
custom_null: ~
custom_block: |
  line one
    indented

  last
custom_folded: >
  folded
  text
custom_flow_multi: [a,
  b, c]
escapes: "tab\thereé\x41 \\ end"
single: 'it''s'
url: https://example.com/a:b
---

# Overview

The [clock](/infra/time-server.md#overview) feeds [hosts](/infra/hosts/).
See [the root](../index.md) and ![a diagram](d.png).

## Overview

```
# not a heading
```

### Plan: CLI first, MCP later

<a name="custom-anchor"></a>

Footnoted claim.[^chrony]

[^chrony]: chronyc tracking
    continued here
````

`tests/fixtures/catalog/log.md`:

```markdown
# Log

## 2026-10-26

* **Creation:** Added fixtures.
```

`tests/fixtures/catalog/people/bad-colon.md`:

```markdown
---
type: Person
title: Bad: colon in plain value
---
```

`tests/fixtures/catalog/people/duplicate-key.md`:

```markdown
---
type: Person
type: Again
---
```

`tests/fixtures/catalog/people/flow-error.md`:

```markdown
---
type: Person
tags: [a, b
---
```

`tests/fixtures/catalog/people/quoted-top-key.md`:

```markdown
---
type: Person
"quoted": no
---
```

`tests/fixtures/catalog/people/stray-line.md`:

```markdown
---
type: Person
  stray: indented
---
```

`tests/fixtures/catalog/people/unicode.md`:

```markdown
---
type: Person
title: Zoë Ünïcode
description: "emoji-free multibyte: 日本語"
---

# Ünïcode Wörds

# Ünïcode Wörds
```

`tests/fixtures/catalog/people/unterminated.md`:

```markdown
---
type: Person
title: "never closed
---
```

- [ ] **Step 3: Create the parity tests** `tests/parity.rs`:

```rust
//! Differential tests against `tools/okf.py`, the implementation being replaced.
//!
//! Two layers:
//! - Golden tests (always run): the Rust output for the synthetic corpus in
//!   `tests/fixtures/catalog` must equal `tests/golden/*.json`, which were
//!   produced by `okf.py`. These keep working after `okf.py` is retired.
//! - Live tests (`cargo test --test parity -- --ignored`): run `okf.py`
//!   through `tests/parity/okf_shim.py` on the synthetic corpus and, when
//!   `OKF_PARITY_CATALOG` names a catalog checkout, on that real catalog too.
//!   Set `OKF_PY` to okf.py's path (default
//!   `~/repos/pg_knowledge_catalog/tools/okf.py`). With `UPDATE_GOLDEN=1`
//!   they rewrite the golden files from okf.py's output.
//!
//! The real catalog is never copied into this repository: it is private.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use okf_mcp::okf::doc::Doc;
use okf_mcp::okf::markdown::{body_split, headings};
use okf_mcp::okf::workspace::{DiskWorkspace, Workspace};
use okf_mcp::okf::yaml::{block_item, flow_value, Value};
use serde_json::{json, Value as Json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog");
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const SHIM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/okf_shim.py");

// ---------------------------------------------------------------------------
// The Rust side, in the shim's JSON shapes
// ---------------------------------------------------------------------------

fn markdown_files(root: &Path) -> (DiskWorkspace, Vec<String>) {
    let workspace = DiskWorkspace::new(root);
    let files = workspace.list_markdown().expect("list corpus");
    (workspace, files)
}

fn rust_frontmatter(root: &Path) -> Json {
    let (workspace, files) = markdown_files(root);
    let entries = files.iter().map(|path| {
        let doc = Doc::from_text(
            path,
            &workspace.read_to_string(path).expect("read corpus file"),
        );
        let mut entry = json!({"path": path, "has_frontmatter": doc.has_frontmatter()});
        if doc.has_frontmatter() {
            match doc.parse() {
                Ok(map) => entry["value"] = Value::Map(map).to_json(),
                Err(error) => entry["error_line"] = json!(error.line),
            }
        }
        entry
    });
    Json::Array(entries.collect())
}

fn rust_headings(root: &Path) -> Json {
    let (workspace, files) = markdown_files(root);
    let entries = files.iter().map(|path| {
        let text = workspace.read_to_string(path).expect("read corpus file");
        let (_, body) = body_split(&text);
        let found: Vec<Json> = headings(&body)
            .into_iter()
            .map(|h| json!([h.index, h.level, h.text, h.slug]))
            .collect();
        json!({"path": path, "headings": found})
    });
    Json::Array(entries.collect())
}

/// Values whose renderings are compared. A top-level null is left out on
/// purpose: okf.py renders it as `- None`, a bug the port fixes.
fn render_inputs() -> Json {
    json!([
        "plain",
        "needs: quotes",
        "true",
        "12",
        "",
        " padded",
        "with, comma",
        "multi\nline\ttab",
        "claude-code/claude-opus-5-5",
        ["a", "b c", "d,e", null, ["nested"]],
        {"by": "human:pgrace", "at": "2026-10-26T23:01:11Z"},
        {"id": "x", "resource": "a: b", "title": null},
        {"nested": {"k": "v"}},
        {},
        []
    ])
}

fn json_to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::Seq(items.iter().map(json_to_value).collect()),
        Json::Object(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
        Json::Bool(_) | Json::Number(_) => panic!("render inputs are strings, lists and maps only"),
    }
}

fn rust_render(inputs: &Json) -> Json {
    let items = inputs
        .as_array()
        .expect("render inputs are a list")
        .iter()
        .map(|input| {
            let value = json_to_value(input);
            json!({"flow": flow_value(&value), "block": block_item(&value, 2)})
        });
    Json::Array(items.collect())
}

// ---------------------------------------------------------------------------
// Comparison helpers
// ---------------------------------------------------------------------------

/// Compares two lists entry by entry so a failure names the file at fault.
fn assert_entries_equal(rust: &Json, python: &Json, what: &str) {
    let (rust, python) = (
        rust.as_array().expect("rust list"),
        python.as_array().expect("python list"),
    );
    for (r, p) in rust.iter().zip(python) {
        assert_eq!(r, p, "{what}: Rust (left) and okf.py (right) disagree");
    }
    assert_eq!(
        rust.len(),
        python.len(),
        "{what}: different number of entries"
    );
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(GOLDEN).join(name)
}

fn read_golden(name: &str) -> Json {
    let text = std::fs::read_to_string(golden_path(name)).unwrap_or_else(|e| {
        panic!("{name}: {e}; regenerate with UPDATE_GOLDEN=1 cargo test --test parity -- --ignored")
    });
    serde_json::from_str(&text).expect("golden file is JSON")
}

// ---------------------------------------------------------------------------
// Golden tests (no Python needed)
// ---------------------------------------------------------------------------

#[test]
fn frontmatter_matches_golden() {
    assert_entries_equal(
        &rust_frontmatter(Path::new(FIXTURES)),
        &read_golden("frontmatter.json"),
        "frontmatter",
    );
}

#[test]
fn headings_match_golden() {
    assert_entries_equal(
        &rust_headings(Path::new(FIXTURES)),
        &read_golden("headings.json"),
        "headings",
    );
}

#[test]
fn render_matches_golden() {
    assert_entries_equal(
        &rust_render(&render_inputs()),
        &read_golden("render.json"),
        "render",
    );
}

// ---------------------------------------------------------------------------
// Live tests against okf.py
// ---------------------------------------------------------------------------

fn okf_py() -> PathBuf {
    let path = std::env::var_os("OKF_PY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME is set");
            Path::new(&home).join("repos/pg_knowledge_catalog/tools/okf.py")
        });
    assert!(
        path.is_file(),
        "okf.py not found at {}; set OKF_PY",
        path.display()
    );
    path
}

/// Runs the shim (`python3 -I`: isolated mode, no user site or PYTHON* env).
fn shim(args: &[&str], stdin: Option<&Json>) -> Json {
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(SHIM)
        .args(args)
        .env("OKF_PY", okf_py())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 is installed");
    {
        // The block ends the borrow of stdin, closing the pipe so Python sees EOF.
        let mut pipe = child.stdin.take().expect("stdin is piped");
        if let Some(input) = stdin {
            pipe.write_all(input.to_string().as_bytes())
                .expect("write shim stdin");
        }
    }
    let output = child.wait_with_output().expect("shim runs");
    assert!(output.status.success(), "okf_shim.py {args:?} failed");
    serde_json::from_slice(&output.stdout).expect("shim prints JSON")
}

fn corpora() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(FIXTURES)];
    if let Some(real) = std::env::var_os("OKF_PARITY_CATALOG") {
        roots.push(PathBuf::from(real));
    }
    roots
}

fn update_golden(name: &str, python: &Json) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let text = serde_json::to_string_pretty(python).expect("serialize golden");
        std::fs::create_dir_all(GOLDEN).expect("create golden dir");
        std::fs::write(golden_path(name), format!("{text}\n")).expect("write golden");
    }
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_frontmatter_parity() {
    for root in corpora() {
        let python = shim(&["frontmatter", &root.to_string_lossy()], None);
        if root == Path::new(FIXTURES) {
            update_golden("frontmatter.json", &python);
        }
        assert_entries_equal(
            &rust_frontmatter(&root),
            &python,
            &format!("frontmatter of {}", root.display()),
        );
    }
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_headings_parity() {
    for root in corpora() {
        let python = shim(&["headings", &root.to_string_lossy()], None);
        if root == Path::new(FIXTURES) {
            update_golden("headings.json", &python);
        }
        assert_entries_equal(
            &rust_headings(&root),
            &python,
            &format!("headings of {}", root.display()),
        );
    }
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_render_parity() {
    let inputs = render_inputs();
    let python = shim(&["render"], Some(&inputs));
    update_golden("render.json", &python);
    assert_entries_equal(&rust_render(&inputs), &python, "render");
}
```

- [ ] **Step 4: Run the golden tests and check they fail** (no golden files yet)

Run: `cargo test --test parity`
Expected: 3 FAILED, each panicking with `No such file or directory ... regenerate with UPDATE_GOLDEN=1 ...`.

- [ ] **Step 5: Generate the golden files from `okf.py`, checking the real catalog in the same run**

```bash
UPDATE_GOLDEN=1 \
OKF_PY=~/repos/pg_knowledge_catalog/tools/okf.py \
OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
cargo test --test parity -- --ignored
```

Expected: `3 passed`. `tests/golden/` now holds `frontmatter.json`, `headings.json` and `render.json`. Run `git -C ~/repos/pg_knowledge_catalog status --short`: it must show no `tools/__pycache__/`.

- [ ] **Step 6: Inspect the golden frontmatter.** It must record the expected outcome for each fixture:

```bash
python3 -c "import json
for e in json.load(open('tests/golden/frontmatter.json')):
    print(e['path'], e.get('error_line', 'ok' if 'value' in e else 'no-fm'))"
```

Expected output:

```
AGENTS.md no-fm
index.md ok
log.md no-fm
infra/index.md no-fm
infra/time-server.md ok
infra/hosts/crlf-host.md ok
infra/hosts/no-frontmatter.md no-fm
people/bad-colon.md 3
people/duplicate-key.md 3
people/flow-error.md 3
people/quoted-top-key.md 3
people/stray-line.md 2
people/unicode.md ok
people/unterminated.md 3
```

(`.claude/hidden.md` must be absent.)

- [ ] **Step 7: Run the whole suite**

Run: `cargo test`
Expected: lib 67 passed; doc tests 8 passed; parity 3 passed, 3 ignored.

- [ ] **Step 8: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add tests/
git commit -m "Add okf.py parity harness with synthetic corpus and golden files

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Changelog entry and final verification

**Files:**
- Create: `changelog/<UTC timestamp>-m1-core-library.md`

- [ ] **Step 1: Take the timestamp from the clock**

Run: `date -u +%Y-%m-%dT%H%M%SZ`. Use its output as the filename prefix: `changelog/<output>-m1-core-library.md`.

- [ ] **Step 2: Write the changelog entry** with this content (put the real timestamp in the heading):

```markdown
# M1: core library (<timestamp>)

First iteration of the OKF MCP server (spec:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`). No server yet; this
milestone ports `tools/okf.py`'s document model to a Rust library.

## Added

- `okf::yaml`: the YAML-subset parser and renderer (scalars stay strings).
- `okf::doc`: frontmatter split, top-level blocks, parsing with file line numbers,
  and byte-preserving edits (`set_block`, `delete_block`, `append_seq_item`).
- `okf::markdown`: links (images excluded), headings and GitHub slugs, HTML anchors,
  footnotes, frontmatter/body splits, link resolution.
- `okf::paths`: `normpath`, walk order, concept rules, concept resolution and
  new-concept validation.
- `okf::workspace`: the `Workspace` trait with disk (atomic writes) and in-memory
  implementations.
- `clock`: injectable UTC clock and the catalog timestamp format.
- `error::OkfError`.
- Parity harness: `tests/parity/okf_shim.py`, a synthetic corpus, golden files from
  `okf.py`, and live tests against a real catalog (`--ignored`).

## Divergences from okf.py (deliberate)

- Only `\n` and `\r\n` end lines (Python's `splitlines` also splits on form feeds,
  U+2028 and others).
- A null item in a block list renders as `null`, not `None`.
- A surrogate `\u` escape is an error.
- Concept references are bundle paths only, never filesystem paths.
- Error messages quote with `"` where Python's `repr` uses `'`.

## Verification

- `cargo test`: 67 unit (including property tests), 8 doc, 3 golden.
- Live parity with the real catalog: 169 documents, 142 with frontmatter, all
  matching `okf.py` (frontmatter values, error lines, heading slugs, rendering).
```

- [ ] **Step 3: Run the full pre-commit checklist from the project's CLAUDE.md**

```bash
cargo build 2>&1 | grep -c warning        # expected: 0
cargo clippy --all-targets -- -D warnings  # expected: clean
cargo fmt --check                          # expected: no output
cargo test                                 # expected: all pass (3 ignored)
cargo doc --no-deps 2>&1 | grep -c warning # expected: 0
OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored  # expected: 3 passed
```

(The project builds no Python or WASM package, so those checklist items don't apply.)

- [ ] **Step 4: Commit**

```bash
git add changelog/
git commit -m "Add M1 changelog entry

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Hand off.** Tell the user M1 is complete, with the test counts. Next step: write the M2 plan (analysis: lint, links, oldest, sprawl, secret scan, snapshot, search).
