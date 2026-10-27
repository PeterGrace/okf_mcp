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
//! Fixture rules: never add a documented divergence (the lists in the
//! `okf` module docs) as a fixture. `infra/hosts/lone-cr.md` in particular
//! must keep having no frontmatter (see `lone_cr_fixture_has_no_frontmatter`).
//! `tests/fixtures/catalog/AGENTS.md` is itself a corpus file, not a readme.
//! Never add a symlinked `.md` file: `list_markdown` deliberately skips
//! dangling and outside links that `okf.py`'s `os.walk` lists (the listing
//! contract on `DiskWorkspace`), and git checkouts may not keep symlinks.
//!
//! The real catalog is never copied into this repository: it is private.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use okf_mcp::okf::doc::Doc;
use okf_mcp::okf::markdown::{body_split, footnote_defs, headings};
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

// ---------------------------------------------------------------------------
// Comparison helpers
// ---------------------------------------------------------------------------

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
        ("SOURCE", source_record(&python_sha256())),
    ];
    std::fs::create_dir_all(GOLDEN).expect("create golden dir");
    for (name, contents) in &files {
        write_golden(name, contents);
    }
}
