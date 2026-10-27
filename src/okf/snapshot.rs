//! A catalog read into memory once: every markdown document with its
//! frontmatter, analysis text, links (resolved, with existence), anchors and
//! backlinks, plus the secret scan of every file.
//!
//! The design's "In-memory snapshot". M3 keeps one per catalog behind an
//! `RwLock<Arc<Snapshot>>` and rebuilds it after each fetch or publish; the
//! analyses in [`crate::okf::analysis`] (and M2's `okf::search`) read it and
//! never touch the disk.
//!
//! Ports `okf.py`'s `build_docs` and `_fm_fallback`, and replaces its repeated
//! file reads: each file is read once, and the analysis view (universal
//! newlines) is derived from that read.
//!
//! # Known divergences from `okf.py`
//!
//! * `build_docs` skips the root `AGENTS.md`, `CLAUDE.md` and `README.md`;
//!   the snapshot keeps them as documents (lint reads them) and marks them
//!   outside the link graph ([`SnapshotDoc::in_link_graph`]).
//! * A link whose normalized target escapes the catalog root is broken here
//!   (`target_exists` is false); `okf.py` asks the OS about the outside path.
//! * Files are listed per the M1 listing contract (`DiskWorkspace` skips
//!   dangling and outside symlinks, special files and non-UTF-8 names).
//! * [`Snapshot::resolve_concept`] only finds snapshot documents, so a
//!   concept-named file in a dot-directory (`.claude/x.md`) is not found;
//!   `okf.py`'s `resolve_concept` asks the OS and would accept it.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use indexmap::IndexMap;
// `rayon::prelude` brings the `par_iter` extension method into scope (Rust
// methods from traits are only callable when the trait is imported).
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

/// `_fm_fallback`'s block-list item pattern, `^\s+-\s+(.+)$` (multi-line).
// `LazyLock` compiles the regex on first use and caches it, like a
// module-level `re.compile` in Python.
static FALLBACK_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[\s\x1c-\x1f]+-[\s\x1c-\x1f]+(.+)$").expect("FALLBACK_ITEM regex is valid")
});

/// What a markdown file is, by name (`okf.py`'s `build_docs` "kind").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// A concept: any `.md` not named in [`crate::okf::paths::NON_CONCEPTS`].
    Concept,
    /// An `index.md`.
    Index,
    /// A `log.md`.
    Log,
    /// `AGENTS.md`, `CLAUDE.md` or `README.md`.
    Machinery,
}

impl DocKind {
    /// The kind of the markdown file at `path`.
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
    /// The parsed mapping.
    ///
    /// # Returns
    /// `Some` for [`Frontmatter::Parsed`], `None` if absent or invalid.
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

/// A link into a document from another one (a record, so its fields are public).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// The linking document's path.
    pub source: String,
    /// The 1-based line of the link in it.
    pub line: usize,
}

/// One markdown document.
#[derive(Debug, Clone, PartialEq)]
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
    ///
    /// # Returns
    /// A new `String` each call; the snapshot does not store the raw text twice.
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

    /// Does `okf.py`'s link analysis see this file?
    ///
    /// # Returns
    /// `true` for everything except the root `AGENTS.md`, `CLAUDE.md` and
    /// `README.md` (`build_docs` skips them).
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

    /// The document at a catalog-relative path, or `None` if there is none.
    pub fn get(&self, path: &str) -> Option<&SnapshotDoc> {
        self.by_path.get(path).map(|&index| &self.docs[index])
    }

    /// The concept documents, in `os.walk` order (`okf.py`'s `concepts(root)`).
    pub fn concepts(&self) -> impl Iterator<Item = &SnapshotDoc> {
        self.docs.iter().filter(|d| d.kind == DocKind::Concept)
    }

    /// Lowercased anchors of a markdown file.
    ///
    /// # Returns
    /// The anchors of a snapshot document, or of another markdown file some
    /// fragment link targets; `None` if the file was not read.
    pub fn anchors_of(&self, path: &str) -> Option<&BTreeSet<String>> {
        self.get(path)
            .map(SnapshotDoc::anchors)
            .or_else(|| self.extra_anchors.get(path))
    }

    /// Links into `path` from documents in the link graph (in-page links
    /// excluded), in walk order.
    ///
    /// # Returns
    /// An empty slice when nothing links to `path`.
    pub fn backlinks(&self, path: &str) -> &[Backlink] {
        // `&[Backlink]` implements `Default` (the empty slice).
        self.backlinks
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Secret-scan findings for every file except `tools/okf.py`, in walk
    /// order, then line order (unsorted; `lint` sorts its whole report).
    pub fn secret_findings(&self) -> &[Finding] {
        &self.secrets
    }

    /// Resolves `/dir/x.md`, `dir/x`, `/dir/x` or `dir/x.md` to a concept path,
    /// as [`crate::okf::paths::resolve_concept`] does on a workspace, but
    /// looking only at snapshot documents.
    ///
    /// # Returns
    /// The concept's catalog-relative path.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] when the document exists but is not a concept;
    /// [`OkfError::NotFound`] otherwise.
    pub fn resolve_concept(&self, reference: &str) -> Result<String, OkfError> {
        let bare = reference.trim_start_matches('/');
        for candidate in [normpath(bare), normpath(&format!("{bare}.md"))] {
            if escapes_root(&candidate) || !candidate.ends_with(".md") {
                continue;
            }
            // `let ... else` binds on a match and runs the `else` block otherwise.
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
        Cow::Owned(normalized) => normalized,
        Cow::Borrowed(_) => raw,
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
///
/// Whether a file has frontmatter is decided by [`Doc`] (raw text, any line
/// ending, `---` or `...` closer), but the fallback re-splits the analysis
/// text with `split_frontmatter` (`---\n` opener, `\n---` closer). The two
/// can disagree, e.g. on a `...` closer, which leaves the fallback an empty
/// string and no title. `okf.py` does exactly the same (`Doc(path).parse()`,
/// then `_fm_fallback(fm, ...)` on `split_frontmatter(text)`), so the port
/// keeps the re-split rather than reusing `Doc`'s lines.
fn title_and_aliases(frontmatter: &Frontmatter, text: &str) -> (Option<String>, Vec<String>) {
    let data = match frontmatter {
        Frontmatter::Invalid(_) => {
            let (fm, _, _) = split_frontmatter(text);
            return (
                fm_fallback_scalar(fm, "title"),
                fm_fallback_list(fm, "aliases"),
            );
        }
        Frontmatter::Absent | Frontmatter::Parsed(_) => frontmatter.data(),
    };
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

/// `{a.lower() for a in anchors_in(text)}`.
fn lowercase_anchors(text: &str) -> BTreeSet<String> {
    anchors_in(text)
        .into_iter()
        .map(|a| a.to_lowercase())
        .collect()
}

/// Anchors for fragment-link targets that are markdown files but not
/// snapshot documents (for example in a dot-directory). Unreadable targets
/// are left out; `okf.py` would crash on them.
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

/// Target path to the links into it, from documents in the link graph.
fn backlinks_of(docs: &[SnapshotDoc]) -> HashMap<String, Vec<Backlink>> {
    let mut backlinks: HashMap<String, Vec<Backlink>> = HashMap::new();
    for doc in docs.iter().filter(|d| d.in_link_graph()) {
        for link in doc.links.iter().filter(|l| !l.in_page && l.target_exists) {
            // `entry(..).or_default()` is Python's `setdefault(key, [])`.
            backlinks
                .entry(link.target.clone())
                .or_default()
                .push(Backlink {
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
///
/// # Returns
/// The value, or `None` when no line starts with `key:` and a value.
pub(crate) fn fm_fallback_scalar(fm: &str, key: &str) -> Option<String> {
    // Only runs for documents whose frontmatter does not parse, so compiling
    // the pattern per call costs nothing in the normal case.
    let pattern = format!(
        r"(?m)^{}:[\s\x1c-\x1f]*(.+?)[\s\x1c-\x1f]*$",
        regex::escape(key)
    );
    let found = Regex::new(&pattern).ok()?.captures(fm)?;
    Some(strip_quotes(py_strip(&found[1])).to_string())
}

/// `_fm_fallback(fm, key, True)`: a flow list on the key line, else a block
/// list below it, each item stripped of whitespace and quote characters.
///
/// # Returns
/// The items; empty when neither form is found.
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
    let block = format!(r"(?m)^{key}:[\s\x1c-\x1f]*\n((?:[\s\x1c-\x1f]+-[\s\x1c-\x1f]+.+\n?)+)");
    let Some(found) = Regex::new(&block).ok().and_then(|r| r.captures(fm)) else {
        return Vec::new();
    };
    FALLBACK_ITEM
        .captures_iter(&found[1])
        .map(|item| strip_quotes(py_strip(&item[1])).to_string())
        .collect()
}

/// Python's `s.strip("\"'")`.
fn strip_quotes(text: &str) -> &str {
    text.trim_matches(['"', '\''])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn catalog() -> MemWorkspace {
        MemWorkspace::from_files([
            (
                "index.md",
                "---\nokf_version: \"0.2\"\n---\n* [Infra](infra/)\n",
            ),
            ("AGENTS.md", "# Agents\n[x](/nowhere.md)\n"),
            ("log.md", "# Log\n"),
            ("infra/README.md", "# Infra\n"),
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
        let kinds: Vec<(&str, DocKind)> = snapshot
            .docs()
            .iter()
            .map(|d| (d.path(), d.kind()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("AGENTS.md", DocKind::Machinery),
                ("index.md", DocKind::Index),
                ("log.md", DocKind::Log),
                ("infra/README.md", DocKind::Machinery),
                ("infra/a.md", DocKind::Concept),
                ("infra/b.md", DocKind::Concept),
                ("infra/index.md", DocKind::Index),
            ]
        );
        assert!(!snapshot.get("AGENTS.md").unwrap().in_link_graph());
        // Only the root machinery files are outside the graph (`build_docs`
        // keeps a nested README.md).
        assert!(snapshot.get("infra/README.md").unwrap().in_link_graph());
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
    fn scalar_and_missing_aliases_follow_build_docs() {
        let workspace = MemWorkspace::from_files([
            ("one.md", "---\ntitle: [x]\naliases: solo\n---\n"),
            ("none.md", "# No frontmatter\n"),
            ("empty.md", "---\naliases: \"\"\n---\n"),
        ]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let one = snapshot.get("one.md").unwrap();
        assert_eq!(
            (one.title(), one.aliases()),
            (None, &["solo".to_string()][..])
        );
        let none = snapshot.get("none.md").unwrap();
        assert_eq!(none.frontmatter(), &Frontmatter::Absent);
        assert_eq!((none.title(), none.aliases().len()), (None, 0));
        assert!(snapshot.get("empty.md").unwrap().aliases().is_empty());
    }

    #[test]
    fn the_fallback_uses_split_frontmatter_like_okf_py() {
        // `Doc` sees frontmatter closed by `...`; `split_frontmatter` needs a
        // `---` closer, so okf.py's fallback gets "" and finds no title.
        let workspace =
            MemWorkspace::from_files([("dots.md", "---\ntitle: [x\naliases: [a]\n...\nbody\n")]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let dots = snapshot.get("dots.md").unwrap();
        assert!(matches!(dots.frontmatter(), Frontmatter::Invalid(_)));
        assert_eq!((dots.title(), dots.aliases().len()), (None, 0));
    }

    #[test]
    fn links_are_resolved_with_existence() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        let links = snapshot.get("infra/a.md").unwrap().links();
        let summary: Vec<_> = links
            .iter()
            .map(|l| {
                (
                    l.line,
                    l.raw.as_str(),
                    l.target.as_str(),
                    l.absolute,
                    l.fragment.as_deref(),
                    l.target_exists,
                )
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
    fn links_outside_or_missing_are_not_existing() {
        let workspace = MemWorkspace::from_files([
            ("a.md", "[up](../a.md) [gone](gone.md) [dir](sub)\n"),
            ("sub/index.md", "x\n"),
        ]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let links: Vec<(&str, bool, bool)> = snapshot
            .get("a.md")
            .unwrap()
            .links()
            .iter()
            .map(|l| (l.target.as_str(), l.target_exists, l.target_is_file))
            .collect();
        assert_eq!(
            links,
            [
                ("../a.md", false, false),
                ("gone.md", false, false),
                ("sub/index.md", true, true),
            ]
        );
    }

    #[test]
    fn anchors_are_lowercased_slugs_and_html_anchors() {
        let workspace = MemWorkspace::from_files([
            (
                "a.md",
                "# Big Title\n<a name=\"MixedCase\"></a>\n[h](.hidden/x.md#Deep)\n",
            ),
            (".hidden/x.md", "## Deep\n"),
        ]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let anchors: Vec<&str> = snapshot
            .anchors_of("a.md")
            .unwrap()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(anchors, ["big-title", "mixedcase"]);
        // A dot-directory target is not a document, but its anchors are read.
        assert!(snapshot.get(".hidden/x.md").is_none());
        assert!(snapshot
            .anchors_of(".hidden/x.md")
            .unwrap()
            .contains("deep"));
        assert!(Snapshot::build(&catalog())
            .unwrap()
            .anchors_of("infra/b.md")
            .unwrap()
            .contains("ports"));
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
        // In-page links are not backlinks.
        let into_a: Vec<&str> = snapshot
            .backlinks("infra/a.md")
            .iter()
            .map(|b| b.source.as_str())
            .collect();
        assert_eq!(into_a, ["infra/index.md"]);
    }

    #[test]
    fn resolve_concept_accepts_okf_py_spellings() {
        let snapshot = Snapshot::build(&catalog()).unwrap();
        for reference in ["/infra/a.md", "infra/a", "/infra/a", "infra/a.md"] {
            assert_eq!(snapshot.resolve_concept(reference).unwrap(), "infra/a.md");
        }
        assert!(matches!(
            snapshot.resolve_concept("index"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            snapshot.resolve_concept("nope"),
            Err(OkfError::NotFound { .. })
        ));
        assert!(matches!(
            snapshot.resolve_concept("../x"),
            Err(OkfError::NotFound { .. })
        ));
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
        let files: Vec<&str> = snapshot
            .secret_findings()
            .iter()
            .map(|f| f.file.as_str())
            .collect();
        assert_eq!(files, ["notes.txt", ".claude/x.md"]);
    }

    #[test]
    fn a_markdown_secret_is_found_from_the_document_text() {
        let line = format!("token: Ab{}", "c1".repeat(4));
        let text = format!("# Doc\r\n{line}\r\n");
        let workspace = MemWorkspace::from_files([("doc.md", text.as_str())]);
        let snapshot = Snapshot::build(&workspace).unwrap();
        let found: Vec<(&str, usize)> = snapshot
            .secret_findings()
            .iter()
            .map(|f| (f.file.as_str(), f.line))
            .collect();
        assert_eq!(found, [("doc.md", 2)]);
    }

    #[test]
    fn fallback_lists_follow_okf_py() {
        assert_eq!(
            fm_fallback_list("aliases: [a, 'b', , \"c\"]", "aliases"),
            ["a", "b", "c"]
        );
        assert_eq!(
            fm_fallback_list("aliases:\n  - x\n  - 'y'\nnext: 1", "aliases"),
            ["x", "y"]
        );
        assert!(fm_fallback_list("title: t", "aliases").is_empty());
        assert_eq!(
            fm_fallback_scalar("title:\n  next line", "title").as_deref(),
            Some("next line")
        );
        assert_eq!(
            fm_fallback_scalar("type: x\ntitle:  \"Quoted\"  ", "title").as_deref(),
            Some("Quoted")
        );
        assert_eq!(fm_fallback_scalar("type: x", "title"), None);
    }
}
