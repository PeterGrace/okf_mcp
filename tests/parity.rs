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
//!   `~/repos/pg_knowledge_catalog/tools/okf.py`). `live_goldens_come_from_this_okf_py`
//!   fails when okf.py's SHA-256 differs from `tests/golden/SOURCE`.
//! - Regeneration: `UPDATE_GOLDEN=1 cargo test --test parity -- --ignored`
//!   rewrites every golden file and `SOURCE` from okf.py (`regenerate_goldens`).
//!
//! The analyses (`links`, `oldest`, `sprawl`, `lint`; `analysis.json`) run
//! with a frozen "now" ([`FIXED_NOW`]) and the commit times in
//! `tests/fixtures/commit-times.json`. The secret rules are covered by a
//! corpus written to a temp directory at test time ([`secrets_corpus`];
//! `secrets.json`), because a secret-shaped string must never be committed.
//! The live test also runs every analysis on `OKF_PARITY_CATALOG` with real
//! git on both sides. okf.py's lint and sprawl assume Python 3.12 or later
//! (`sum()` of floats is compensated from 3.12 on; `src/okf/analysis/sprawl.rs`).
//!
//! The ops (`ops.json`) run `tests/parity/ops_script.json` through
//! `okf_mcp::okf::ops` on an in-memory copy of the corpus and through
//! okf.py's `cmd_*` functions on a temp copy, both with "now" frozen at
//! [`FIXED_NOW`], and compare every step's output or error and every file
//! that changed, byte for byte. For `mv` and `split` the output also carries
//! okf.py's touched list, rebuilt from `OpOutcome::touched` by
//! [`print_touched`] (the op's message leaves it out, since the tool returns
//! `touched` itself). The script's error steps are refusals the
//! port words as okf.py does (`OkfError::Refused`); the variants it words
//! differently (`NotFound`, `FrontmatterParse`, `Shape`) are unit-tested.
//! The live test runs on the synthetic corpus only: the script names its paths.
//!
//! JSON is compared as parsed values, never as text: okf.py's `json.dumps`
//! escapes non-ASCII (`\u2082`) where serde_json writes it raw, and the
//! shim prints ASCII-only JSON either way. Numbers are compared as serde_json
//! parses them, so an integer never equals a float (`18` is not `18.0`), and
//! object key order is checked separately (`Map` equality ignores it).
//!
//! Fixture rules: never add a documented divergence (the lists in the
//! `okf` module docs) as a fixture. `infra/hosts/lone-cr.md` in particular
//! must keep having no frontmatter (see `lone_cr_fixture_has_no_frontmatter`).
//! `tests/fixtures/catalog/AGENTS.md` is itself a corpus file, not a readme.
//! Never add a symlinked `.md` file: `list_markdown` deliberately skips
//! dangling and outside links that `okf.py`'s `os.walk` lists (the listing
//! contract on `DiskWorkspace`), and git checkouts may not keep symlinks.
//!
//! The real catalog is never copied into this repository: it is private.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, FixedOffset, Utc};
use okf_mcp::clock::format_timestamp;
use okf_mcp::error::OkfError;
use okf_mcp::okf::analysis::links::links_report;
use okf_mcp::okf::analysis::lint::{lint, CommitTimes};
use okf_mcp::okf::analysis::oldest::{oldest, OldestQuery};
use okf_mcp::okf::analysis::sprawl::{sprawl, SprawlQuery};
use okf_mcp::okf::doc::Doc;
use okf_mcp::okf::markdown::{body_split, footnote_defs, headings};
use okf_mcp::okf::ops::{
    ensure_index, index, log, move_concept, question, review, source, split_concept, stamp,
    unquestion, LogKind, OpContext, OpOutcome, SourceEntry, Split, DEFAULT_SPLIT_TYPE,
};
use okf_mcp::okf::snapshot::Snapshot;
use okf_mcp::okf::timestamp::parse_iso;
use okf_mcp::okf::workspace::{DiskWorkspace, MemWorkspace, StoredFile, Workspace};
use okf_mcp::okf::yaml::{block_item, flow_value, Value};
use serde_json::{json, Value as Json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog");
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const SHIM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/okf_shim.py");
const COMMIT_TIMES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/commit-times.json"
);
const OPS_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/ops_script.json");
/// The frozen "now" of the analysis goldens.
const FIXED_NOW: &str = "2026-10-27T12:00:00Z";

// ---------------------------------------------------------------------------
// The Rust side, in the shim's JSON shapes
// ---------------------------------------------------------------------------

fn markdown_files(root: &Path) -> (DiskWorkspace, Vec<String>) {
    let workspace = DiskWorkspace::new(root).expect("corpus root exists");
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
                Err(error) => {
                    entry["error_line"] = json!(error.line);
                    // The whole message: the port quotes source text with
                    // Python `repr` (`okf::pystr::py_repr_str`), as okf.py does.
                    entry["error_message"] = json!(error.message);
                }
            }
        }
        entry
    });
    Json::Array(entries.collect())
}

fn rust_markdown(root: &Path) -> Json {
    let (workspace, files) = markdown_files(root);
    let entries = files.iter().map(|path| {
        // Analysis reads use universal newlines, as okf.py's text-mode reads do.
        let text = workspace.read_text(path).expect("read corpus file");
        let (_, body) = body_split(&text);
        let found: Vec<Json> = headings(&body)
            .into_iter()
            .map(|h| json!([h.index, h.level, h.text, h.slug]))
            .collect();
        let footnotes: serde_json::Map<String, Json> = footnote_defs(&body)
            .into_iter()
            .map(|(label, lines)| (label, json!(lines)))
            .collect();
        json!({"path": path, "headings": found, "footnotes": footnotes})
    });
    Json::Array(entries.collect())
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
        Json::Bool(_) | Json::Number(_) => {
            panic!("render inputs are strings, lists and maps only, got {json}")
        }
    }
}

/// Renders each input value the way `okf_shim.py render` does.
fn rust_render(inputs: &[Json]) -> Json {
    let items = inputs.iter().map(|input| {
        let value = json_to_value(input);
        json!({"input": input, "flow": flow_value(&value), "block": block_item(&value, 2)})
    });
    Json::Array(items.collect())
}

/// The values whose renderings are compared: the `input` of each entry in
/// `render.json`, so the golden file describes itself. To compare a new
/// value, add an entry with just its `input` (any `flow`/`block`) and
/// regenerate. A top-level null is left out on purpose: okf.py renders it as
/// `- None`, a bug the port fixes.
fn render_inputs() -> Vec<Json> {
    let golden = read_golden("render.json");
    let entries = golden.as_array().expect("render.json is a list");
    entries
        .iter()
        .map(|entry| {
            entry
                .get("input")
                .cloned()
                .expect("render entry has an input")
        })
        .collect()
}

/// An aware timestamp as a UTC instant (panics on anything else: test input).
fn instant(text: &str) -> DateTime<Utc> {
    aware(text).with_timezone(&Utc)
}

/// An aware timestamp with the offset it was written with, as okf.py's
/// `parse_ts` returns it (panics on anything else: test input).
fn aware(text: &str) -> DateTime<FixedOffset> {
    parse_iso(text)
        .and_then(|t| t.aware())
        .unwrap_or_else(|| panic!("{text} is an aware timestamp"))
}

/// Reads a commit-times file (`{"times": {path: stamp}, "dirty": [path]}`),
/// the same file the shim hands okf.py's `last_commit_times`.
fn commit_times_from_file(path: &str) -> CommitTimes {
    let text = std::fs::read_to_string(path).expect("read commit times");
    let data: Json = serde_json::from_str(&text).expect("commit times are JSON");
    let times: HashMap<String, _> = data["times"]
        .as_object()
        .expect("times is an object")
        .iter()
        .map(|(path, at)| {
            // Offset kept, exactly as the shim's `okf.parse_ts` keeps it.
            let at = aware(at.as_str().expect("a time is a string"));
            (path.clone(), at)
        })
        .collect();
    let dirty: HashSet<String> = data["dirty"]
        .as_array()
        .expect("dirty is a list")
        .iter()
        .map(|p| p.as_str().expect("a dirty path is a string").to_string())
        .collect();
    CommitTimes::new(times, dirty)
}

/// okf.py's `last_commit_times(root)`, run through git the same way.
///
/// `None` when `git log` fails (okf.py's "git unavailable"). A failing
/// `git status` is not checked, as in okf.py: its empty output gives no
/// dirty paths. Paths are stripped with `str::trim` where okf.py uses
/// `str.strip`; they differ only on control characters and non-ASCII
/// spaces, which git quotes (`core.quotePath`), so no git path tells them
/// apart.
fn git_commit_times(root: &Path) -> Option<CommitTimes> {
    // A closure (Python lambda) capturing `root`; `.ok()` turns a failure to
    // start git into `None`.
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
    };
    // `?` on an Option returns `None` from the function early.
    let log = git(&["log", "--format=%x00%cI", "--name-only", "--no-renames"])
        .filter(|output| output.status.success())?;
    let mut times = HashMap::new();
    let mut current = None;
    for line in String::from_utf8_lossy(&log.stdout).split('\n') {
        if let Some(stamp) = line.strip_prefix('\0') {
            current = parse_iso(stamp).and_then(|t| t.aware());
        } else if let (false, Some(at)) = (line.trim().is_empty(), current) {
            // `entry().or_insert` is `dict.setdefault`: the newest commit
            // (git log's first) wins.
            times.entry(line.trim().to_string()).or_insert(at);
        }
    }
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    let dirty = String::from_utf8_lossy(&status.stdout)
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        // `l[3:]`: the porcelain status columns are ASCII, so bytes = chars.
        .filter_map(|line| line.get(3..))
        .map(|rest| {
            let renamed_to = rest.rsplit(" -> ").next().unwrap_or(rest);
            renamed_to.trim_matches('"').to_string()
        })
        .collect();
    Some(CommitTimes::new(times, dirty))
}

/// Every analysis of `root`, in the shim's `analysis` JSON shape.
fn rust_analysis(root: &Path, now: DateTime<Utc>, commits: Option<&CommitTimes>) -> Json {
    let workspace = DiskWorkspace::new(root).expect("corpus root exists");
    let snapshot = Snapshot::build(&workspace).expect("snapshot builds");
    let all = OldestQuery {
        n: 1_000_000,
        open_questions_only: false,
    };
    // `..Default::default()` fills the remaining fields (Python's
    // `dataclasses.replace` the other way round).
    let questions = OldestQuery {
        open_questions_only: true,
        ..OldestQuery::default()
    };
    let every_concept = SprawlQuery {
        concept: None,
        n: 1_000_000,
    };
    let rows = sprawl(&snapshot, every_concept).expect("sprawl of every concept");
    // `impl Serialize` accepts any report type (a generic, resolved at
    // compile time); `to_value` builds the same tree `json.loads` would.
    fn value(report: impl serde::Serialize) -> Json {
        serde_json::to_value(report).expect("report serializes")
    }
    json!({
        "links": value(links_report(&snapshot, true)),
        "oldest": value(oldest(&snapshot, now, all)),
        "oldest_open_questions": value(oldest(&snapshot, now, questions)),
        "sprawl": value(rows),
        "lint": value(lint(&snapshot, now, commits)),
    })
}

/// A corpus holding one hit for every secret rule, plus near misses.
///
/// Built in a temp directory at test time: secret-shaped strings never go
/// into a committed file (GitHub push protection, and the catalog's own
/// lint). Each one is assembled with `format!` so this source file does not
/// hold it either.
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
        (
            "index.md",
            "---\nokf_version: \"0.2\"\n---\n* [Keys](/keys.md)\n".to_string(),
        ),
        (
            "keys.md",
            format!(
                "---\ntype: Note\ntitle: Keys\ndescription: d\n---\n{github}\n{pat}\n{slack}\n{sk}\n\
                 id {aws}\n{pem}\npassword: \"{value}\" token={value} secret: changeme\n\
                 x{github}\n{aws}Q\n"
            ),
        ),
        ("notes.txt", format!("api_key = {value}\n")),
        (".claude/settings.md", format!("CLIENT_SECRET: {value}\n")),
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

/// The ops script, `{"setup": {path: text}, "steps": [...]}`.
fn ops_script() -> Json {
    let text = std::fs::read_to_string(OPS_SCRIPT).expect("read ops script");
    serde_json::from_str(&text).expect("ops script is JSON")
}

/// The corpus as stored files, plus the script's setup files: the state
/// both sides start from.
fn ops_corpus(root: &Path, script: &Json) -> BTreeMap<String, StoredFile> {
    let workspace = DiskWorkspace::new(root).expect("corpus root exists");
    let mut files: BTreeMap<String, StoredFile> = workspace
        .list_files()
        .expect("list corpus")
        .into_iter()
        .map(|path| {
            let content = std::fs::read(root.join(&path)).expect("read corpus file");
            let file = StoredFile {
                content,
                executable: false,
            };
            (path, file)
        })
        .collect();
    let setup = script["setup"].as_object().expect("setup is an object");
    for (path, text) in setup {
        let content = text
            .as_str()
            .expect("setup text is a string")
            .as_bytes()
            .to_vec();
        let file = StoredFile {
            content,
            executable: false,
        };
        files.insert(path.clone(), file);
    }
    files
}

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

/// Runs one script step through `okf_mcp::okf::ops`. Missing fields fall
/// back to the shim's `OPS_DEFAULTS` (`by` is "fixture").
fn run_op(
    workspace: &MemWorkspace,
    now: DateTime<Utc>,
    step: &Json,
) -> Result<OpOutcome, OkfError> {
    let text = |key: &str| {
        step[key]
            .as_str()
            .unwrap_or_else(|| panic!("step {step} needs a string {key}"))
    };
    let optional = |key: &str| step.get(key).and_then(Json::as_str);
    let ctx = OpContext {
        now,
        by: optional("by").unwrap_or("fixture"),
    };
    match text("cmd") {
        "stamp" => {
            let field = serde_json::from_value(step["field"].clone()).expect("a stamp field");
            stamp(workspace, &ctx, text("concept"), field)
        }
        "review" => {
            let questions: Vec<String> = step
                .get("question")
                .map(|q| serde_json::from_value(q.clone()).expect("a list of questions"))
                .unwrap_or_default();
            review(
                workspace,
                &ctx,
                text("concept"),
                text("outcome"),
                text("note"),
                &questions,
            )
        }
        "question" => question(workspace, &ctx, text("concept"), text("question")),
        "unquestion" => unquestion(workspace, &ctx, text("concept"), text("question")),
        "source" => {
            let entry = SourceEntry {
                id: text("id"),
                resource: text("resource"),
                title: optional("title"),
                author: optional("author"),
            };
            source(workspace, &ctx, text("concept"), &entry)
        }
        "log" => {
            let kind = step.get("kind").map_or(LogKind::Update, |k| {
                serde_json::from_value(k.clone()).expect("a log kind")
            });
            log(workspace, &ctx, kind, text("text"))
        }
        "index" => index(
            workspace,
            &ctx,
            text("concept"),
            text("summary"),
            optional("section"),
            optional("title"),
        ),
        "ensure_index" => {
            let mut outcome = OpOutcome::default();
            ensure_index(workspace, text("folder"), &mut outcome)?;
            // The shim prints the files ensure_index wrote, in order.
            outcome.message = outcome.touched.join("\n");
            Ok(outcome)
        }
        "mv" => move_concept(workspace, text("old"), text("new")).map(with_touched),
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
        other => panic!("unknown ops command {other}"),
    }
}

/// The ops script's result, in `okf_shim.py ops`'s JSON shape.
fn rust_ops(root: &Path, now: DateTime<Utc>, script: &Json) -> Json {
    let before = ops_corpus(root, script);
    let workspace = MemWorkspace::from_stored(before.clone()).expect("corpus loads");
    let steps: Vec<Json> = script["steps"]
        .as_array()
        .expect("steps is a list")
        .iter()
        .map(|step| match run_op(&workspace, now, step) {
            Ok(outcome) => {
                let mut entry = json!({ "output": outcome.message });
                if !outcome.notes.is_empty() {
                    entry["notes"] = json!(outcome.notes);
                }
                entry
            }
            Err(error) => json!({ "error": error.to_string() }),
        })
        .collect();
    let after = workspace.into_stored();
    // Sorted by path (a `BTreeSet`), as the shim sorts.
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut files = serde_json::Map::new();
    for path in paths {
        let (old, new) = (before.get(path), after.get(path));
        if old.map(|f| &f.content) != new.map(|f| &f.content) {
            let text = new.map(|f| String::from_utf8(f.content.clone()).expect("ops write UTF-8"));
            files.insert(path.clone(), json!(text));
        }
    }
    json!({ "steps": steps, "files": files })
}

// ---------------------------------------------------------------------------
// Comparison helpers
// ---------------------------------------------------------------------------

/// Recursive comparison of parsed JSON that names the path of the first
/// difference and checks object key order (serde_json's `Map` equality
/// ignores it). Values are compared after parsing, so how either side
/// escaped non-ASCII text does not matter.
fn assert_json_equal(rust: &Json, python: &Json, what: &str) {
    match (rust, python) {
        (Json::Object(r), Json::Object(p)) => {
            let (rk, pk): (Vec<&String>, Vec<&String>) = (r.keys().collect(), p.keys().collect());
            assert_eq!(
                rk, pk,
                "{what}: keys or key order (Rust left, okf.py right)"
            );
            for (key, value) in r {
                assert_json_equal(value, &p[key], &format!("{what}.{key}"));
            }
        }
        (Json::Array(r), Json::Array(p)) => {
            // `zip` stops at the shorter list, so compare the common prefix
            // first (naming the first differing element), then the lengths.
            for (i, (a, b)) in r.iter().zip(p).enumerate() {
                assert_json_equal(a, b, &format!("{what}[{i}]"));
            }
            assert_eq!(r.len(), p.len(), "{what}: length (Rust left, okf.py right)");
        }
        _ => assert_eq!(rust, python, "{what} (Rust left, okf.py right)"),
    }
}

/// Compares two lists entry by entry so a failure names the file at fault.
fn assert_entries_equal(rust: &Json, python: &Json, what: &str) {
    let (rust, python) = (
        rust.as_array().expect("rust list"),
        python.as_array().expect("python list"),
    );
    // Same files in the same order first, so a listing difference is reported
    // as such rather than as a confusing mismatch of two different files.
    let paths = |entries: &[Json]| -> Vec<Json> {
        entries
            .iter()
            .map(|entry| entry.get("path").cloned().unwrap_or(Json::Null))
            .collect()
    };
    assert_eq!(
        paths(rust),
        paths(python),
        "{what}: Rust (left) and okf.py (right) list different files or orders"
    );
    for (r, p) in rust.iter().zip(python) {
        assert_eq!(r, p, "{what}: Rust (left) and okf.py (right) disagree");
        // `Map` equality ignores key order, even with serde_json's
        // `preserve_order`; the serialized text does not, so compare it too.
        assert_eq!(
            serde_json::to_string(r).expect("serialize Rust entry"),
            serde_json::to_string(p).expect("serialize okf.py entry"),
            "{what}: same content, different key order (Rust left, okf.py right)"
        );
    }
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

/// `infra/hosts/lone-cr.md` must stay frontmatter-free: `Doc` ends lines
/// only at `\n`/`\r\n` (a documented divergence in `src/okf/doc.rs`), so a
/// lone-`\r` file with frontmatter would compare a known difference.
#[test]
fn lone_cr_fixture_has_no_frontmatter() {
    let golden = read_golden("frontmatter.json");
    let entry = golden
        .as_array()
        .expect("frontmatter.json is a list")
        .iter()
        .find(|entry| entry["path"] == "infra/hosts/lone-cr.md")
        .expect("lone-cr.md is in the corpus");
    assert_eq!(
        entry["has_frontmatter"],
        json!(false),
        "lone-cr.md must not have frontmatter: lone-\\r line endings in frontmatter are \
         a documented divergence (see the module docs of src/okf/doc.rs)"
    );
}

#[test]
fn markdown_matches_golden() {
    assert_entries_equal(
        &rust_markdown(Path::new(FIXTURES)),
        &read_golden("markdown.json"),
        "markdown",
    );
}

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

#[test]
fn render_matches_golden() {
    assert_entries_equal(
        &rust_render(&render_inputs()),
        &read_golden("render.json"),
        "render",
    );
}

#[test]
fn ops_match_golden() {
    assert_json_equal(
        &rust_ops(Path::new(FIXTURES), instant(FIXED_NOW), &ops_script()),
        &read_golden("ops.json"),
        "ops",
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
///
/// Arguments are `OsStr`s (the OS's own string type, which need not be
/// UTF-8), so a corpus path reaches Python byte for byte instead of through a
/// lossy UTF-8 conversion.
fn shim(args: &[&OsStr], stdin: Option<&Json>) -> Json {
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(SHIM)
        .args(args)
        .env("OKF_PY", okf_py())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    // Reads stdout and stderr to the end (concurrently, so neither pipe fills).
    let output = child.wait_with_output().expect("shim runs");
    assert!(
        output.status.success(),
        "okf_shim.py {args:?} failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("shim prints JSON")
}

fn corpora() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(FIXTURES)];
    if let Some(real) = std::env::var_os("OKF_PARITY_CATALOG") {
        roots.push(PathBuf::from(real));
    }
    roots
}

fn python_frontmatter(root: &Path) -> Json {
    shim(&[OsStr::new("frontmatter"), root.as_os_str()], None)
}

fn python_markdown(root: &Path) -> Json {
    shim(&[OsStr::new("markdown"), root.as_os_str()], None)
}

fn python_render(inputs: &[Json]) -> Json {
    shim(&[OsStr::new("render")], Some(&Json::from(inputs.to_vec())))
}

fn python_analysis(root: &Path, now: &str, commits: &str) -> Json {
    shim(
        &[
            OsStr::new("analysis"),
            root.as_os_str(),
            OsStr::new(now),
            OsStr::new(commits),
        ],
        None,
    )
}

fn python_ops(root: &Path, now: &str, script: &Json) -> Json {
    shim(
        &[OsStr::new("ops"), root.as_os_str(), OsStr::new(now)],
        Some(script),
    )
}

fn python_sha256() -> String {
    let digest = shim(&[OsStr::new("sha256")], None);
    digest.as_str().expect("sha256 is a string").to_string()
}

/// Is `UPDATE_GOLDEN` exactly `1`? (Merely being set, or `0`, is not enough.)
fn updating_goldens() -> bool {
    std::env::var_os("UPDATE_GOLDEN").as_deref() == Some(OsStr::new("1"))
}

/// The content of `tests/golden/SOURCE` for an okf.py with this digest.
fn source_record(sha256: &str) -> String {
    format!("okf.py sha256 {sha256}\n")
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_goldens_come_from_this_okf_py() {
    if updating_goldens() {
        return; // `regenerate_goldens` is rewriting SOURCE right now.
    }
    let recorded = std::fs::read_to_string(golden_path("SOURCE")).unwrap_or_default();
    let current = source_record(&python_sha256());
    assert_eq!(
        recorded.trim(),
        current.trim(),
        "okf.py ({}) differs from the one the goldens were generated from \
         (tests/golden/SOURCE): the goldens may be stale. Check the okf.py diff, \
         then regenerate with UPDATE_GOLDEN=1 cargo test --test parity -- --ignored",
        okf_py().display()
    );
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_frontmatter_parity() {
    for root in corpora() {
        assert_entries_equal(
            &rust_frontmatter(&root),
            &python_frontmatter(&root),
            &format!("frontmatter of {}", root.display()),
        );
    }
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_markdown_parity() {
    for root in corpora() {
        assert_entries_equal(
            &rust_markdown(&root),
            &python_markdown(&root),
            &format!("markdown of {}", root.display()),
        );
    }
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_render_parity() {
    let inputs = render_inputs();
    assert_entries_equal(&rust_render(&inputs), &python_render(&inputs), "render");
}

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
    // `let ... else`: bind the value or leave the function (the real
    // catalog is optional).
    let Some(real) = std::env::var_os("OKF_PARITY_CATALOG").map(PathBuf::from) else {
        return;
    };
    // One shared "now", to the second, for both sides.
    let now_text = format_timestamp(Utc::now());
    let now = instant(&now_text);
    let started = std::time::Instant::now();
    let rust = rust_analysis(&real, now, git_commit_times(&real).as_ref());
    eprintln!(
        "Rust analysis of {} took {:?}",
        real.display(),
        started.elapsed()
    );
    let started = std::time::Instant::now();
    let python = python_analysis(&real, &now_text, "git");
    eprintln!(
        "okf.py analysis of {} took {:?}",
        real.display(),
        started.elapsed()
    );
    assert_json_equal(&rust, &python, &format!("analysis of {}", real.display()));
    let lint = &rust["lint"];
    eprintln!(
        "lint of {}: {} errors, {} warnings",
        real.display(),
        lint["errors"].as_array().map_or(0, Vec::len),
        lint["warnings"].as_array().map_or(0, Vec::len)
    );
}

#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_ops_parity() {
    let fixtures = Path::new(FIXTURES);
    let script = ops_script();
    assert_json_equal(
        &rust_ops(fixtures, instant(FIXED_NOW), &script),
        &python_ops(fixtures, FIXED_NOW, &script),
        "ops on the synthetic corpus",
    );
}

/// Writes `contents` to `tests/golden/<name>` through the port's own atomic
/// writer: a reader never sees half a file, an existing golden keeps its
/// mode, and a new one gets the usual umask mode.
fn write_golden(name: &str, contents: &str) {
    DiskWorkspace::new(GOLDEN)
        .expect("golden dir exists")
        .write_atomic(name, contents)
        .unwrap_or_else(|e| panic!("write golden {name}: {e}"));
}

fn pretty(json: &Json) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(json).expect("serialize golden")
    )
}

/// With `UPDATE_GOLDEN=1`, regenerates every golden file and `SOURCE` from
/// okf.py. Everything is computed before anything is written, and each file
/// is replaced atomically. Run the live tests afterwards to see the diff's
/// effect on Rust.
#[test]
#[ignore = "needs python3 and okf.py; run with UPDATE_GOLDEN=1 and --ignored"]
fn regenerate_goldens() {
    if !updating_goldens() {
        return;
    }
    let root = Path::new(FIXTURES);
    let files = [
        ("frontmatter.json", pretty(&python_frontmatter(root))),
        ("markdown.json", pretty(&python_markdown(root))),
        ("render.json", pretty(&python_render(&render_inputs()))),
        (
            "analysis.json",
            pretty(&python_analysis(root, FIXED_NOW, COMMIT_TIMES)),
        ),
        (
            "secrets.json",
            pretty(&python_analysis(secrets_corpus().path(), FIXED_NOW, "none")["lint"]),
        ),
        (
            "ops.json",
            pretty(&python_ops(root, FIXED_NOW, &ops_script())),
        ),
        ("SOURCE", source_record(&python_sha256())),
    ];
    std::fs::create_dir_all(GOLDEN).expect("create golden dir");
    for (name, contents) in &files {
        write_golden(name, contents);
    }
}
