//! The content ops (design "Write tools"): `create_concept`,
//! `edit_document`, `replace_body` and `set_frontmatter`. `okf.py` has no
//! counterpart for them. They reuse its path rules (`new_concept_path`), its
//! renderers (`okf::yaml`) and its textual frontmatter edits (`Doc`), and
//! word their refusals as `okf.py` would.

use serde_json::{Map, Value as Json};

use crate::error::OkfError;
use crate::okf::doc::{is_top_key, Doc, KEY_ORDER};
use crate::okf::ops::log_index::{list_concept, Listing};
use crate::okf::ops::replay::{insert_index_bullet_text, missing_heading, new_index_text};
use crate::okf::ops::{
    ensure_index, load_concept, refused, save, stamp_value, LoadedConcept, OpContext, OpOutcome,
};
use crate::okf::paths::{basename, dirname, is_concept, join, new_concept_path, resolve_concept};
use crate::okf::pystr::{py_repr_str, py_strip};
use crate::okf::rules::check_path;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{block_item, flow_value, scalar, ScalarStyle, Value};

/// Keys a dedicated tool owns. `set_frontmatter` refuses all five;
/// `create_concept` refuses the first three, the stamps the server writes.
const MANAGED_KEYS: [&str; 5] = [
    "generated",
    "verified",
    "reviewed",
    "open_questions",
    "sources",
];

/// A concept to create (`create_concept`'s arguments).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewConcept<'a> {
    /// The requested path, with or without `.md` and a leading `/`.
    pub path: &'a str,
    /// The frontmatter keys and values; must hold a `type`, and must not
    /// hold `generated`, `verified` or `reviewed`.
    pub frontmatter: &'a Map<String, Json>,
    /// Everything after the closing `---`, written as given.
    pub body: &'a str,
    /// With a summary, the concept is listed in its folder's `index.md`.
    pub index_summary: Option<&'a str>,
    /// The heading to list it under (`index`'s `section`); refused without
    /// `index_summary`.
    pub index_section: Option<&'a str>,
}

/// A JSON value as frontmatter. Every scalar is a string in `okf.py`'s
/// model, so numbers and booleans are stored as their JSON text (and come
/// back quoted where YAML would read them as something else).
pub(crate) fn yaml_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Str(flag.to_string()),
        Json::Number(number) => Value::Str(number.to_string()),
        Json::String(text) => Value::Str(text.clone()),
        Json::Array(items) => Value::Seq(items.iter().map(yaml_value).collect()),
        Json::Object(map) => Value::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), yaml_value(value)))
                .collect(),
        ),
    }
}

/// A top-level key's lines. A scalar is quoted only when it must be; a
/// mapping, or a list of scalars, is one flow line (`{ by: x, at: y }`,
/// `[a, b]`, as stamps and tags are written); a list holding collections is
/// a block list of [`block_item`]s.
pub(crate) fn key_lines(key: &str, value: &Value) -> Vec<String> {
    match value {
        Value::Null => vec![format!("{key}:")],
        Value::Str(text) => vec![format!("{key}: {}", scalar(text, ScalarStyle::Block))],
        Value::Seq(items)
            if items
                .iter()
                .any(|item| matches!(item, Value::Seq(_) | Value::Map(_))) =>
        {
            let mut lines = vec![format!("{key}:")];
            lines.extend(items.iter().flat_map(|item| block_item(item, 2)));
            lines
        }
        Value::Seq(_) | Value::Map(_) => vec![format!("{key}: {}", flow_value(value))],
    }
}

/// `value` as a string that is not blank (Python's `str.strip()`), the
/// rule for a concept's `type` and for the `title` a listing uses.
///
/// # Returns
/// The string, or `None` for a missing, non-string or blank value.
fn non_blank(value: Option<&Json>) -> Option<&str> {
    // `and_then` chains two `Option` steps, like `x and f(x)` in Python.
    value
        .and_then(Json::as_str)
        .filter(|text| !py_strip(text).is_empty())
}

/// Refuses a key `Doc` could not find again.
fn checked_key(key: &str) -> Result<(), OkfError> {
    if is_top_key(key) {
        Ok(())
    } else {
        Err(refused(format!(
            "{} is not a valid frontmatter key",
            py_repr_str(key)
        )))
    }
}

/// Creates a concept (design "Write tools", `create_concept`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `ctx` - the "now" and actor for the `generated` stamp.
/// * `new` - the path, frontmatter, body and optional listing.
///
/// # Returns
/// The outcome: the concept (a free edit), then any folder indexes created
/// and listings added, each with its replay ops. The message is
/// `created <path>`, plus the listing's line; the notes carry the
/// non-kebab-case warning and `ensure_index`'s "created" lines.
///
/// # Errors
/// [`OkfError::InvalidPath`] from [`new_concept_path`] (outside the catalog,
/// reserved, machinery, taken); [`OkfError::Refused`] without a non-blank
/// string `type`, for a stamp key, an invalid key, for `index_section`
/// without `index_summary`, or a listing without a non-blank `title`;
/// [`OkfError::FrontmatterParse`] if the rendered frontmatter would not
/// parse (a guard: the renderers always write parseable YAML); the
/// listing's refusals (see `index`).
///
/// Every refusal above comes before anything is written: the listing's
/// folder index (or, when `ensure_index` will create it, the text it will
/// have) is checked for `index_section` first, and a missing folder index
/// needs a root `index.md` to be listed under. Only a read or write failure
/// past that point leaves files written; as for [`ensure_index`], run the
/// op on a scratch copy, as the write path does, and drop the copy on
/// error.
pub fn create_concept(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    new: &NewConcept<'_>,
) -> Result<OpOutcome, OkfError> {
    let (path, note) = new_concept_path(workspace, new.path)?;
    if non_blank(new.frontmatter.get("type")).is_none() {
        return Err(refused(format!("{path}: frontmatter needs a type")));
    }
    let title = non_blank(new.frontmatter.get("title"));
    let listed_title = match (new.index_summary, new.index_section, title) {
        (None, Some(_), _) => {
            return Err(refused(format!(
                "{path}: index_section needs index_summary"
            )))
        }
        (Some(_), _, None) => {
            return Err(refused(format!(
                "{path} has no title to list it by; add one or leave out index_summary"
            )))
        }
        (_, _, title) => title,
    };
    let mut entries: Vec<(&str, Value)> = Vec::with_capacity(new.frontmatter.len() + 1);
    for (key, value) in new.frontmatter {
        checked_key(key)?;
        if MANAGED_KEYS[..3].contains(&key.as_str()) {
            return Err(refused(format!(
                "{key} is written by the server (stamp, review); leave it out"
            )));
        }
        entries.push((key.as_str(), yaml_value(value)));
    }
    entries.push(("generated", stamp_value(ctx)));
    // `sort_by_key` is stable: `KEY_ORDER` keys first, in that order, then
    // the others in the caller's order.
    entries.sort_by_key(|(key, _)| {
        KEY_ORDER
            .iter()
            .position(|k| k == key)
            .unwrap_or(KEY_ORDER.len())
    });
    let mut text = String::from("---\n");
    for line in entries
        .iter()
        .flat_map(|(key, value)| key_lines(key, value))
    {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str("---\n");
    text.push_str(new.body);
    Doc::from_text(&path, &text).parse_checked()?;
    if new.index_summary.is_some() {
        check_listing(workspace, &path, new.index_section)?;
    }
    workspace.write_atomic(&path, &text)?;
    let mut outcome = OpOutcome::default();
    outcome.touch(&path);
    // `Option` is iterable (zero or one item), so `extend` adds the note if any.
    outcome
        .notes
        .extend(note.map(|note| format!("note: {note}")));
    let mut lines = vec![format!("created {path}")];
    if let (Some(summary), Some(title)) = (new.index_summary, listed_title) {
        ensure_index(workspace, dirname(&path), &mut outcome)?;
        let listing = Listing {
            concept: &path,
            title,
            summary,
            section: new.index_section,
        };
        lines.push(list_concept(workspace, &listing, &mut outcome)?);
    }
    outcome.message = lines.join("\n");
    Ok(outcome)
}

/// Checks, before anything is written, that `list_concept` will find the
/// folder's `index.md` and its `section`: the existing index, or the text
/// `ensure_index` will write for a new folder.
///
/// # Errors
/// [`OkfError::Refused`] with `list_concept`'s wording: no root `index.md`
/// when the folder's index is missing (`ensure_index` creates every other
/// folder index, but needs the root's to list them in), or a `section`
/// that is not a heading of the index.
fn check_listing(
    workspace: &dyn Workspace,
    concept: &str,
    section: Option<&str>,
) -> Result<(), OkfError> {
    let folder = dirname(concept);
    let index_path = join(folder, "index.md");
    let text = if workspace.exists(&index_path) {
        workspace.read_text(&index_path)?
    } else if !workspace.exists("index.md") {
        // `ensure_index` creates every missing folder index below the root,
        // then lists the topmost new folder in the root's: so the root
        // `index.md` (this one, for a root concept) must already exist.
        return Err(refused("no index.md in /; create it first".to_string()));
    } else {
        new_index_text(folder)
    };
    // The bullet is irrelevant here: `None` means only "no such heading".
    match section {
        Some(section) if insert_index_bullet_text(&text, "", Some(section), false).is_none() => {
            Err(missing_heading(&index_path, section))
        }
        Some(_) | None => Ok(()),
    }
}

/// Refuses an edit that changes the frontmatter: its presence, or any of
/// its lines. Without this, text written into a document without
/// frontmatter could become frontmatter (`---`, `generated: ...`, `---`),
/// forging the stamps only `stamp` and `review` write.
///
/// # Arguments
/// * `before` - the document as read.
/// * `after` - the full text the edit would write.
///
/// # Errors
/// [`OkfError::Refused`]: `okf.py`'s "has no frontmatter; add it with your
/// editor first" when the document had none, else a pointer to
/// `set_frontmatter` and the bookkeeping tools.
fn keeps_frontmatter(before: &Doc, after: &str) -> Result<(), OkfError> {
    let edited = Doc::from_text(before.path(), after);
    // Comparing slices compares every line, like `==` on Python lists.
    if edited.has_frontmatter() == before.has_frontmatter()
        && edited.frontmatter() == before.frontmatter()
    {
        return Ok(());
    }
    let path = before.path();
    Err(refused(if before.has_frontmatter() {
        format!(
            "{path}: the edit would change the frontmatter; \
             use set_frontmatter or the bookkeeping tools"
        )
    } else {
        format!("{path} has no frontmatter; add it with your editor first")
    }))
}

/// An `InvalidPath` refusal of `path`.
fn invalid(path: &str, reason: &str) -> OkfError {
    OkfError::InvalidPath {
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

/// The normalized path `edit_document` may edit: a concept or an `index.md`.
///
/// [`check_path`] refuses a path outside the catalog and machinery (with
/// [`crate::okf::rules::is_machinery`]); `log.md` and every other file are
/// refused here.
fn editable_path(path: &str) -> Result<String, OkfError> {
    let normalized = check_path(path)?;
    let name = basename(&normalized);
    if name == "log.md" {
        return Err(invalid(&normalized, "is written by the log tool only"));
    }
    if !normalized.ends_with(".md") || (name != "index.md" && !is_concept(&normalized)) {
        return Err(invalid(
            &normalized,
            "edit_document edits concepts and index.md files only",
        ));
    }
    Ok(normalized)
}

/// Exact text replacement in a concept or an `index.md` (design "Write
/// tools", `edit_document`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `path` - the document's path (a leading `/` is allowed).
/// * `old` - the exact text to find; not empty.
/// * `new` - its replacement.
/// * `replace_all` - replace every occurrence instead of requiring one.
///
/// # Returns
/// The outcome (a free edit, no replay); nothing is touched when `new`
/// equals `old`.
///
/// # Errors
/// [`OkfError::InvalidPath`] for `log.md`, machinery, any other file, or a
/// path outside the catalog; [`OkfError::NotFound`] for a missing file;
/// [`OkfError::Refused`] for an empty `old`, a match that touches the
/// frontmatter, or a result whose frontmatter differs (one added to a
/// document without it, say); [`OkfError::EditMismatch`] unless `old` occurs exactly once
/// (at least once with `replace_all`).
pub fn edit_document(
    workspace: &dyn Workspace,
    path: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<OpOutcome, OkfError> {
    let path = editable_path(path)?;
    if old.is_empty() {
        return Err(refused(format!("{path}: the old text is empty")));
    }
    // Byte-preserving read: every byte outside the matches is kept.
    let text = workspace.read_to_string(&path)?;
    // `match_indices` finds non-overlapping matches left to right, as
    // Python's `str.count` and `str.replace` do.
    let starts: Vec<usize> = text.match_indices(old).map(|(start, _)| start).collect();
    if starts.is_empty() || (!replace_all && starts.len() != 1) {
        return Err(OkfError::EditMismatch {
            path,
            matches: starts.len(),
        });
    }
    // The frontmatter, with both delimiters, is everything before the body.
    let doc = Doc::from_text(&path, &text);
    let body_start = text.len() - doc.body().len();
    if let Some(&hit) = starts.iter().find(|&&start| start < body_start) {
        let line = text[..hit].matches('\n').count() + 1;
        return Err(refused(format!(
            "{path}: the match on line {line} touches the frontmatter; \
             use set_frontmatter or the bookkeeping tools"
        )));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    if updated == text {
        return Ok(OpOutcome::unchanged(format!("{path}: no change")));
    }
    keeps_frontmatter(&doc, &updated)?;
    workspace.write_atomic(&path, &updated)?;
    let mut outcome = OpOutcome {
        message: format!("{path}: replaced {} occurrence(s)", starts.len()),
        ..OpOutcome::default()
    };
    outcome.touch(&path);
    Ok(outcome)
}

/// Replaces a concept's body, keeping its frontmatter byte for byte
/// (design "Write tools", `replace_body`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `concept` - the concept reference.
/// * `body` - the new text after the closing `---` (the whole text for a
///   document without frontmatter).
///
/// # Returns
/// The outcome (a free edit); nothing is touched when the body is the same.
///
/// # Errors
/// As [`resolve_concept`]; [`OkfError::Refused`] when the new text would
/// change the frontmatter (a body starting with a `---` block on a document
/// without frontmatter); the workspace's read or write error.
pub fn replace_body(
    workspace: &dyn Workspace,
    concept: &str,
    body: &str,
) -> Result<OpOutcome, OkfError> {
    let path = resolve_concept(workspace, concept)?;
    let text = workspace.read_to_string(&path)?;
    let doc = Doc::from_text(&path, &text);
    let mut updated = text[..text.len() - doc.body().len()].to_string();
    // A closing `---` at the very end has no newline to end it yet.
    if doc.has_frontmatter() && !updated.ends_with('\n') && !body.is_empty() {
        updated.push('\n');
    }
    updated.push_str(body);
    if updated == text {
        return Ok(OpOutcome::unchanged(format!("{path}: body unchanged")));
    }
    keeps_frontmatter(&doc, &updated)?;
    workspace.write_atomic(&path, &updated)?;
    let mut outcome = OpOutcome {
        message: format!("{path}: body replaced"),
        ..OpOutcome::default()
    };
    outcome.touch(&path);
    Ok(outcome)
}

/// Sets or deletes one descriptive frontmatter key with a textual edit
/// (design "Write tools", `set_frontmatter`): an existing key is replaced
/// where it is, a new one goes to its `KEY_ORDER` place, and every other
/// byte is kept.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `concept` - the concept reference.
/// * `key` - the top-level key.
/// * `value` - the new value; `None` or JSON null deletes the key.
///
/// # Returns
/// The outcome; nothing is touched when the key already reads that way.
///
/// # Errors
/// [`OkfError::Refused`] for a key a dedicated tool owns, for deleting
/// `type` or setting it to anything but a non-blank string, for an invalid
/// key, and for a document without frontmatter;
/// [`OkfError::FrontmatterParse`] when the frontmatter does not parse, before
/// or after the edit; as [`resolve_concept`] otherwise.
pub fn set_frontmatter(
    workspace: &dyn Workspace,
    concept: &str,
    key: &str,
    value: Option<&Json>,
) -> Result<OpOutcome, OkfError> {
    checked_key(key)?;
    if MANAGED_KEYS.contains(&key) {
        return Err(refused(format!(
            "{key} has its own tool (stamp, review, question, unquestion, add_source); \
             set_frontmatter does not write it"
        )));
    }
    let value = value.filter(|v| !v.is_null());
    if key == "type" {
        // `create_concept`'s rule: `type` stays a non-blank string.
        match value {
            None => {
                return Err(refused(
                    "type cannot be deleted; every concept needs one".to_string(),
                ))
            }
            Some(kind) if non_blank(Some(kind)).is_none() => {
                return Err(refused(
                    "type must be a non-blank string; every concept needs one".to_string(),
                ))
            }
            Some(_) => {}
        }
    }
    let LoadedConcept { path, mut doc, .. } = load_concept(workspace, concept)?;
    // The key's current lines, borrowed from the document (no copy).
    let current = doc.block(key).map(|block| block.start..block.end);
    let verb = match value {
        None if current.is_none() => {
            return Ok(OpOutcome::unchanged(format!("{path}: {key} is not set")));
        }
        None => {
            doc.delete_block(key);
            "deleted"
        }
        Some(json) => {
            let lines = key_lines(key, &yaml_value(json));
            // `is_some_and` borrows `doc` only inside the closure, before the
            // `set_block` below needs it mutably.
            if current.is_some_and(|span| doc.frontmatter()[span] == lines[..]) {
                return Ok(OpOutcome::unchanged(format!("{path}: {key} unchanged")));
            }
            doc.set_block(key, lines);
            "set"
        }
    };
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: {verb} {key}");
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::{replay, ReplayOp};
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    const ROOT_INDEX: &str = "# Root\n\n* [Infra](infra/) - systems\n";
    const EDIT_DOC: &str =
        "---\ntype: Note\ntitle: Same words\n---\nSame words here.\r\nAnd Same words there.\n";
    const SET_DOC: &str = "---\ntype: Note\n# a comment\ntitle: Old\nx_custom: 1\n---\nBody\n";

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn object(value: Json) -> Map<String, Json> {
        match value {
            Json::Object(map) => map,
            other => panic!("expected an object, got {other}"),
        }
    }

    fn concept<'a>(
        path: &'a str,
        frontmatter: &'a Map<String, Json>,
        body: &'a str,
    ) -> NewConcept<'a> {
        NewConcept {
            path,
            frontmatter,
            body,
            index_summary: None,
            index_section: None,
        }
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn read(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn create_concept_renders_frontmatter_in_key_order() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({
            "tags": ["time", "ntp"],
            "x_owner": "ops: team",
            "title": "NTP pool",
            "type": "Service",
            "description": "Pool servers.",
        }));

        let outcome = create_concept(
            &ws,
            &ctx(),
            &concept("/infra/ntp-pool", &frontmatter, "# NTP pool\n"),
        )
        .unwrap();

        assert_eq!(
            read(&ws, "infra/ntp-pool.md"),
            concat!(
                "---\ntype: Service\ntitle: NTP pool\ndescription: Pool servers.\n",
                "tags: [time, ntp]\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n",
                "x_owner: \"ops: team\"\n---\n# NTP pool\n",
            )
        );
        assert_eq!(outcome.touched, ["infra/ntp-pool.md"]);
        assert!(outcome.replay.is_empty());
        assert!(outcome.notes.is_empty());
        assert_eq!(outcome.message, "created infra/ntp-pool.md");
    }

    #[test]
    fn create_concept_lists_it_creating_folder_indexes() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({ "type": "Note", "title": "Thing" }));
        let new = NewConcept {
            index_summary: Some("a  thing"),
            ..concept("ops/new/thing", &frontmatter, "")
        };

        let outcome = create_concept(&ws, &ctx(), &new).unwrap();

        assert_eq!(
            outcome.touched,
            [
                "ops/new/thing.md",
                "ops/new/index.md",
                "ops/index.md",
                "index.md"
            ]
        );
        assert_eq!(
            read(&ws, "ops/new/index.md"),
            "# New\n\nTODO: say what this folder holds.\n\n# Concepts\n\n* [Thing](thing.md) - a thing\n"
        );
        assert_eq!(
            read(&ws, "ops/index.md"),
            concat!(
                "# Ops\n\nTODO: say what this folder holds.\n\n# Sections\n\n",
                "* [New](new/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            read(&ws, "index.md"),
            format!("{ROOT_INDEX}\n# Sections\n\n* [Ops](ops/) - TODO: describe this folder\n")
        );
        assert_eq!(
            outcome.message,
            "created ops/new/thing.md\nops/new/index.md: * [Thing](thing.md) - a thing"
        );
        assert_eq!(outcome.notes.len(), 2);
        assert_eq!(
            outcome.replay["ops/new/index.md"].last(),
            Some(&ReplayOp::ListBullet {
                target: "ops/new/thing.md".to_string(),
                bullet: "* [Thing](thing.md) - a thing".to_string(),
                section: None,
                create_section: false,
            })
        );
        // The concept is a free edit; every index edit replays onto the original.
        assert!(!outcome.replay.contains_key("ops/new/thing.md"));
        for path in ["ops/new/index.md", "ops/index.md", "index.md"] {
            let original = (path == "index.md").then_some(ROOT_INDEX);
            assert_eq!(
                replay(path, original, &outcome.replay[path]).unwrap(),
                read(&ws, path),
                "{path}"
            );
        }
    }

    #[test]
    fn create_concept_lists_it_under_the_named_section() {
        let infra = "# Infra\n\n# Concepts\n\n# Hosts\n\n* [A](a.md) - a\n";
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX), ("infra/index.md", infra)]);
        let frontmatter = object(json!({ "type": "Host", "title": "B" }));
        let listed = |path, section| NewConcept {
            index_summary: Some("b"),
            index_section: Some(section),
            ..concept(path, &frontmatter, "")
        };

        let outcome = create_concept(&ws, &ctx(), &listed("infra/b", "Hosts")).unwrap();
        let missing = create_concept(&ws, &ctx(), &listed("infra/c", "Nope")).unwrap_err();
        let new_folder = create_concept(&ws, &ctx(), &listed("ops/c", "Hosts")).unwrap_err();
        let rootless = MemWorkspace::from_files([("infra/index.md", infra)]);
        let no_root = create_concept(&rootless, &ctx(), &listed("c", "Hosts")).unwrap_err();
        let nested = NewConcept {
            index_section: None,
            ..listed("ops/new/c", "Hosts")
        };
        let nested_no_root = create_concept(&rootless, &ctx(), &nested).unwrap_err();

        assert_eq!(
            read(&ws, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n# Hosts\n\n* [A](a.md) - a\n* [B](b.md) - b\n"
        );
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::ListBullet {
                target: "infra/b.md".to_string(),
                bullet: "* [B](b.md) - b".to_string(),
                section: Some("Hosts".to_string()),
                create_section: false,
            }]
        );
        // Each listing refusal comes before anything is written.
        assert_eq!(refusal(missing), "no heading 'Nope' in infra/index.md");
        assert_eq!(refusal(new_folder), "no heading 'Hosts' in ops/index.md");
        assert_eq!(refusal(no_root), "no index.md in /; create it first");
        assert!(!ws.exists("infra/c.md"));
        assert!(!ws.exists("ops/c.md") && !ws.exists("ops/index.md"));
        assert_eq!(read(&ws, "index.md"), ROOT_INDEX);
        assert_eq!(refusal(nested_no_root), "no index.md in /; create it first");
        assert!(!rootless.exists("c.md"));
        assert!(!rootless.exists("ops/new/c.md") && !rootless.exists("ops/new/index.md"));
    }

    #[test]
    fn create_concept_notes_a_name_that_is_not_kebab_case() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({ "type": "Note" }));

        let outcome = create_concept(&ws, &ctx(), &concept("Big_Name", &frontmatter, "")).unwrap();

        assert_eq!(
            outcome.notes,
            ["note: Big_Name.md isn't short, lowercase and hyphenated"]
        );
    }

    #[test]
    fn create_concept_refusals() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX), ("infra/x.md", "x\n")]);
        let untyped = object(json!({ "title": "x" }));
        let blank_type = object(json!({ "type": " " }));
        let bad_key = object(json!({ "type": "Note", "bad key": "x" }));
        let untitled = object(json!({ "type": "Note" }));

        let errors = [
            create_concept(&ws, &ctx(), &concept("a", &untyped, "")).unwrap_err(),
            create_concept(&ws, &ctx(), &concept("a", &blank_type, "")).unwrap_err(),
            create_concept(&ws, &ctx(), &concept("a", &bad_key, "")).unwrap_err(),
        ];
        let listed = NewConcept {
            index_summary: Some("s"),
            ..concept("a", &untitled, "")
        };
        let no_title = create_concept(&ws, &ctx(), &listed).unwrap_err();
        let blank = object(json!({ "type": "Note", "title": " " }));
        let blank_title = create_concept(
            &ws,
            &ctx(),
            &NewConcept {
                frontmatter: &blank,
                ..listed
            },
        )
        .unwrap_err();
        let unlisted = NewConcept {
            index_section: Some("Concepts"),
            ..concept("a", &untitled, "")
        };
        let section_only = create_concept(&ws, &ctx(), &unlisted).unwrap_err();
        let taken = create_concept(&ws, &ctx(), &concept("infra/x", &untitled, "")).unwrap_err();
        let machinery =
            create_concept(&ws, &ctx(), &concept("tools/x", &untitled, "")).unwrap_err();

        assert_eq!(
            errors.map(refusal),
            [
                "a.md: frontmatter needs a type".to_string(),
                "a.md: frontmatter needs a type".to_string(),
                "'bad key' is not a valid frontmatter key".to_string(),
            ]
        );
        assert_eq!(
            refusal(no_title),
            "a.md has no title to list it by; add one or leave out index_summary"
        );
        assert_eq!(
            refusal(blank_title),
            "a.md has no title to list it by; add one or leave out index_summary"
        );
        assert_eq!(
            refusal(section_only),
            "a.md: index_section needs index_summary"
        );
        assert!(!ws.exists("a.md"));
        assert!(matches!(taken, OkfError::InvalidPath { .. }));
        assert!(matches!(machinery, OkfError::InvalidPath { .. }));
        for key in ["generated", "verified", "reviewed"] {
            let mut stamped = untitled.clone();
            stamped.insert(key.to_string(), json!("x"));
            let error = create_concept(&ws, &ctx(), &concept("a", &stamped, "")).unwrap_err();
            assert_eq!(
                refusal(error),
                format!("{key} is written by the server (stamp, review); leave it out")
            );
        }
    }

    #[test]
    fn edit_replaces_one_match_and_keeps_every_other_byte() {
        let ws = MemWorkspace::from_files([("a.md", EDIT_DOC)]);

        let outcome = edit_document(&ws, "/a.md", " here", " HERE", false).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            "---\ntype: Note\ntitle: Same words\n---\nSame words HERE.\r\nAnd Same words there.\n"
        );
        assert_eq!(outcome.touched, ["a.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(outcome.message, "a.md: replaced 1 occurrence(s)");
    }

    #[test]
    fn edit_needs_exactly_one_match_unless_replace_all() {
        let ws = MemWorkspace::from_files([
            ("a.md", EDIT_DOC),
            ("b.md", "---\ntype: Note\n---\nx x x\n"),
        ]);

        let many = edit_document(&ws, "a.md", "Same words", "S", false).unwrap_err();
        let none = edit_document(&ws, "a.md", "absent", "S", true).unwrap_err();
        let all = edit_document(&ws, "b.md", "x", "y", true).unwrap();
        let same = edit_document(&ws, "b.md", "y y", "y y", false).unwrap();
        let empty = edit_document(&ws, "b.md", "", "y", true).unwrap_err();

        assert!(matches!(many, OkfError::EditMismatch { matches: 3, .. }));
        assert!(matches!(none, OkfError::EditMismatch { matches: 0, .. }));
        assert_eq!(read(&ws, "b.md"), "---\ntype: Note\n---\ny y y\n");
        assert_eq!(all.message, "b.md: replaced 3 occurrence(s)");
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "b.md: no change");
        assert_eq!(refusal(empty), "b.md: the old text is empty");
    }

    #[test]
    fn edit_refuses_a_match_that_touches_the_frontmatter() {
        let ws = MemWorkspace::from_files([("a.md", EDIT_DOC)]);

        let every = edit_document(&ws, "a.md", "Same words", "S", true).unwrap_err();
        let across = edit_document(&ws, "a.md", "words\n---\nSame", "x", false).unwrap_err();

        let expected = "a.md: the match on line 3 touches the frontmatter; \
                        use set_frontmatter or the bookkeeping tools";
        assert_eq!(refusal(every), expected);
        assert_eq!(refusal(across), expected);
        assert_eq!(read(&ws, "a.md"), EDIT_DOC);
    }

    #[test]
    fn edit_works_on_index_md_and_refuses_log_md_and_machinery() {
        let ws = MemWorkspace::from_files([
            ("index.md", "# Root\n\n* [A](a.md) - old\n"),
            ("log.md", "# Log\n"),
            ("AGENTS.md", "# Agents\n"),
            ("tools/notes.md", "old\n"),
            ("install/setup.md", "old\n"),
            ("notes.txt", "old\n"),
        ]);

        let index = edit_document(&ws, "index.md", "old", "new", false).unwrap();
        let refused: Vec<OkfError> = [
            "log.md",
            "AGENTS.md",
            "tools/notes.md",
            "install/setup.md",
            "notes.txt",
            "../x.md",
        ]
        .iter()
        .map(|path| edit_document(&ws, path, "old", "new", false).unwrap_err())
        .collect();
        let missing = edit_document(&ws, "nope.md", "old", "new", false).unwrap_err();

        assert_eq!(index.touched, ["index.md"]);
        assert_eq!(read(&ws, "index.md"), "# Root\n\n* [A](a.md) - new\n");
        assert!(
            refused
                .iter()
                .all(|e| matches!(e, OkfError::InvalidPath { .. })),
            "{refused:?}"
        );
        assert!(matches!(
            &refused[0],
            OkfError::InvalidPath { reason, .. } if reason == "is written by the log tool only"
        ));
        // Machinery is refused by `rules::check_path`, with its reason.
        assert!(matches!(
            &refused[2],
            OkfError::InvalidPath { path, reason }
                if path == "tools/notes.md" && reason.starts_with("is repository machinery")
        ));
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }

    #[test]
    fn edit_and_replace_body_refuse_to_add_frontmatter() {
        const PLAIN: &str = "Hello\n";
        const INDEX: &str = "# Root\n";
        let ws = MemWorkspace::from_files([("n.md", PLAIN), ("index.md", INDEX)]);
        let forged = "---\ngenerated: { by: forged, at: 2026-01-01T00:00:00Z }\n---\nHello";

        let edited = edit_document(&ws, "n.md", "Hello", forged, false).unwrap_err();
        let replaced = replace_body(&ws, "n", &format!("{forged}\n")).unwrap_err();
        let index = edit_document(
            &ws,
            "index.md",
            "# Root",
            "---\ntype: Note\n---\n# Root",
            false,
        )
        .unwrap_err();
        let index_body = replace_body(&ws, "index.md", forged).unwrap_err();

        assert_eq!(
            [edited, replaced].map(refusal),
            ["n.md has no frontmatter; add it with your editor first"; 2].map(String::from)
        );
        assert_eq!(
            refusal(index),
            "index.md has no frontmatter; add it with your editor first"
        );
        assert!(matches!(index_body, OkfError::InvalidPath { .. }));
        assert_eq!(
            (read(&ws, "n.md"), read(&ws, "index.md")),
            (PLAIN.into(), INDEX.into())
        );
    }

    #[test]
    fn replace_body_keeps_the_frontmatter_byte_for_byte() {
        let ws = MemWorkspace::from_files([
            (
                "c.md",
                "---\r\ntype: Note # kept\r\n\r\n---\r\nOld body\r\n",
            ),
            ("n.md", "old\n"),
            ("e.md", "---\ntype: Note\n---"),
            ("index.md", "# Root\n"),
        ]);

        let outcome = replace_body(&ws, "c", "New body\n").unwrap();
        replace_body(&ws, "n", "new\n").unwrap();
        replace_body(&ws, "e", "Text\n").unwrap();
        let same = replace_body(&ws, "c", "New body\n").unwrap();
        let index = replace_body(&ws, "index.md", "x").unwrap_err();

        assert_eq!(
            read(&ws, "c.md"),
            "---\r\ntype: Note # kept\r\n\r\n---\r\nNew body\n"
        );
        assert_eq!(outcome.message, "c.md: body replaced");
        assert_eq!(read(&ws, "n.md"), "new\n");
        assert_eq!(read(&ws, "e.md"), "---\ntype: Note\n---\nText\n");
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "c.md: body unchanged");
        assert!(matches!(index, OkfError::InvalidPath { .. }));
    }

    #[test]
    fn set_frontmatter_inserts_at_the_key_order_place_and_replaces_in_place() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC), ("t.md", SET_DOC)]);

        let outcome = set_frontmatter(&ws, "s", "status", Some(&json!("draft"))).unwrap();
        set_frontmatter(&ws, "t", "title", Some(&json!("New: value"))).unwrap();
        let same = set_frontmatter(&ws, "t", "title", Some(&json!("New: value"))).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\n# a comment\ntitle: Old\nstatus: draft\nx_custom: 1\n---\nBody\n"
        );
        assert_eq!(outcome.message, "s.md: set status");
        assert_eq!(
            read(&ws, "t.md"),
            "---\ntype: Note\n# a comment\ntitle: \"New: value\"\nx_custom: 1\n---\nBody\n"
        );
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "t.md: title unchanged");
    }

    #[test]
    fn set_frontmatter_renders_lists_and_converts_scalars() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC)]);

        set_frontmatter(&ws, "s", "tags", Some(&json!(["a", "b c"]))).unwrap();
        set_frontmatter(
            &ws,
            "s",
            "x_links",
            Some(&json!([{ "id": "a", "url": "u" }])),
        )
        .unwrap();
        set_frontmatter(&ws, "s", "x_custom", Some(&json!(2))).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            concat!(
                "---\ntype: Note\n# a comment\ntitle: Old\ntags: [a, b c]\n",
                "x_custom: \"2\"\nx_links:\n  - id: a\n    url: u\n---\nBody\n",
            )
        );
    }

    #[test]
    fn set_frontmatter_null_deletes_the_key() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC)]);

        let deleted = set_frontmatter(&ws, "s", "x_custom", None).unwrap();
        let again = set_frontmatter(&ws, "s", "x_custom", Some(&Json::Null)).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\n# a comment\ntitle: Old\n---\nBody\n"
        );
        assert_eq!(deleted.message, "s.md: deleted x_custom");
        assert!(again.touched.is_empty());
        assert_eq!(again.message, "s.md: x_custom is not set");
    }

    #[test]
    fn set_frontmatter_refusals() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC), ("n.md", "No frontmatter.\n")]);

        let no_type = set_frontmatter(&ws, "s", "type", None).unwrap_err();
        let bad_key = set_frontmatter(&ws, "s", "bad key", Some(&json!("x"))).unwrap_err();
        let bare = set_frontmatter(&ws, "n", "title", Some(&json!("x"))).unwrap_err();
        let bad_types = [json!(""), json!(" "), json!(["a"]), json!(2)]
            .map(|kind| refusal(set_frontmatter(&ws, "s", "type", Some(&kind)).unwrap_err()));

        for key in MANAGED_KEYS {
            let error = set_frontmatter(&ws, "s", key, Some(&json!("x"))).unwrap_err();
            assert_eq!(
                refusal(error),
                format!(
                    "{key} has its own tool (stamp, review, question, unquestion, add_source); \
                     set_frontmatter does not write it"
                )
            );
        }
        assert_eq!(
            refusal(no_type),
            "type cannot be deleted; every concept needs one"
        );
        assert_eq!(
            bad_types,
            ["type must be a non-blank string; every concept needs one"; 4].map(String::from)
        );
        assert_eq!(refusal(bad_key), "'bad key' is not a valid frontmatter key");
        assert_eq!(
            refusal(bare),
            "n.md has no frontmatter; add it with your editor first"
        );
        assert_eq!(read(&ws, "s.md"), SET_DOC);
    }
}
