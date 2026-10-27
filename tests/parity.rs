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
