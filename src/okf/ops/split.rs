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
/// - For the parent, as `okf.py`'s `load_concept`: [`OkfError::NotFound`] or
///   [`OkfError::InvalidPath`] from
///   [`resolve_concept`](crate::okf::paths::resolve_concept),
///   [`OkfError::Refused`] without frontmatter, [`OkfError::FrontmatterParse`].
///   For `to`, as [`new_concept_path`].
/// - [`OkfError::Refused`] with `okf.py`'s words: no such heading (listing
///   the headings), several matches (listing their slugs), a section with
///   no content, an unknown `--source` id, or generated frontmatter that
///   would not parse.
/// - The workspace's read or write error, including [`OkfError::NotFound`]
///   for a new concept at the catalog root when there is no root `index.md`.
///
/// Every [`OkfError::Refused`] comes before anything is written, but later
/// errors do not: that `NotFound` and any read or write error after the
/// parent and the new concept are written leave them in place. Run the op
/// on a scratch copy, as the write path does, and drop the copy on any error.
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
    let given = split
        .description
        .filter(|description| !description.is_empty());
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
    let index_path = join(folder, "index.md");
    // How many replay ops the folder index holds before this op's rewrite
    // (`ensure_index`'s `CreateIndex`, or none): the listing goes there.
    let listing_at = outcome.replay.get(&index_path).map_or(0, Vec::len);
    let changed = rewrite_links(workspace, &redirect, Some(moved_file))?;
    outcome.record_rewrites(&changed, &redirect);
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
    // `okf.py` rewrites the links before it inserts the bullet, but the
    // replay must list first: once the index's `Relink` has run, a link it
    // redirected already reaches the new concept, and `ListBullet`'s replay
    // skips an index that links its target (plan Decision 15), losing the
    // bullet. The two commute on the op's own text (a `Relink` changes link
    // targets inside lines, never which lines are headings or bullets), so
    // the bullet moves ahead of the `Relink`. `rotate_right(1)` on the tail
    // slice moves its last item (the bullet) to its front.
    if let Some(ops) = outcome.replay.get_mut(&index_path) {
        ops[listing_at..].rotate_right(1);
    }

    let ids = chosen
        .iter()
        .map(|source| field(source, "id").map_or_else(|| "None".to_string(), py_str))
        .collect::<Vec<_>>()
        .join(", ");
    let ids = if ids.is_empty() {
        "none".to_string()
    } else {
        ids
    };
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
        sections
            .iter()
            .filter(|heading| heading.slug == slug)
            .collect()
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
        let known: BTreeSet<&str> = sources
            .iter()
            .filter_map(|source| source_id(source))
            .collect();
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
        format!(
            "description: {}",
            scalar(header.description, ScalarStyle::Block)
        ),
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
/// its end, as `cmd_split`'s `drop`/`keep_defs` loop decides:
/// - a label the section cites and the rest of the parent does not goes
///   with the section (it is dropped here; [`carried_footnotes`] copies it);
/// - any other definition whose lines all sat inside the section stays in
///   the parent, moved to its end. That covers a definition still cited
///   elsewhere and an orphan (cited nowhere) inside the section alike;
/// - every other definition, orphans outside the section included, stays
///   where it is.
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
            [
                "splits/parent.md",
                "splits/background.md",
                "splits/user.md",
                "splits/index.md"
            ]
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
        assert!(
            other.ends_with("Unrelated.[^n2]\n\n[^n2]: Two.\n"),
            "{other}"
        );
        let parent = text(&workspace, "splits/parent.md");
        assert!(
            parent
                .contains("## Other\n\nMoved away. See [Other \\[things\\]](/splits/other.md).\n"),
            "{parent}"
        );
        // `[^n2]` moved with the only section citing it.
        assert!(
            parent.ends_with("[^n0]: Zero.\n[^s1]: From s1.\n"),
            "{parent}"
        );
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
    fn links_from_an_index_and_a_log_into_the_section_replay_as_relink() {
        // `splits/index.md` and `log.md` link into the section's subsection,
        // so both are rewritten by a `Relink`; the index also gets the
        // listing's `ListBullet` afterwards.
        let index = concat!(
            "# Splits\n\nStart at [the detail](parent.md#detail).\n\n",
            "# Concepts\n\n* [Parent](parent.md) - the parent\n",
        );
        let log = "# Log\n\n## 2026-11-02\n\n* Update: wrote [detail](/splits/parent.md#detail).\n";
        let files = [
            ("splits/index.md", index),
            ("splits/parent.md", PARENT),
            ("index.md", "# Root\n"),
            ("log.md", log),
        ];
        let workspace = MemWorkspace::from_files(files);
        let original: std::collections::BTreeMap<&str, &str> = files.into_iter().collect();

        let outcome = split_concept(
            &workspace,
            &ctx(),
            &split("Background", "splits/background.md"),
        )
        .unwrap();

        assert_eq!(
            text(&workspace, "log.md"),
            log.replace("/splits/parent.md#detail", "/splits/background.md#detail")
        );
        assert_eq!(
            text(&workspace, "splits/index.md"),
            concat!(
                "# Splits\n\nStart at [the detail](background.md#detail).\n\n",
                "# Concepts\n\n* [Parent](parent.md) - the parent\n",
                "* [Background](background.md) - Background, split out of Parent.\n",
            )
        );
        let redirect = Redirect {
            from: "splits/parent.md".to_string(),
            to: "splits/background.md".to_string(),
            anchors: Some(BTreeMap::from([
                ("background".to_string(), None),
                ("detail".to_string(), Some("detail".to_string())),
            ])),
        };
        let relink = ReplayOp::Relink { redirect };
        // `slice::from_ref` views one value as a one-item slice, without a copy.
        assert_eq!(outcome.replay["log.md"], std::slice::from_ref(&relink));
        // The listing replays ahead of the rewrite: after the `Relink`, the
        // index already links `background.md`, and a `ListBullet` replayed
        // then would skip it.
        assert!(matches!(
            outcome.replay["splits/index.md"][0],
            ReplayOp::ListBullet { .. }
        ));
        assert_eq!(outcome.replay["splits/index.md"][1], relink);
        for (path, ops) in &outcome.replay {
            let before = original.get(path.as_str()).copied();
            assert_eq!(
                replay(path, before, ops).unwrap(),
                text(&workspace, path),
                "{path}"
            );
        }
    }

    #[test]
    fn every_bookkeeping_edit_replays_onto_the_text_it_was_made_on() {
        let workspace = MemWorkspace::from_files(FILES);
        let original: std::collections::BTreeMap<&str, &str> = FILES.into_iter().collect();

        let outcome = split_concept(
            &workspace,
            &ctx(),
            &split("last-bit", "splits/deeper/last.md"),
        )
        .unwrap();

        assert_eq!(outcome.replay.len(), 2, "{:?}", outcome.replay.keys());
        for (path, ops) in &outcome.replay {
            let before = original.get(path.as_str()).copied();
            assert_eq!(
                replay(path, before, ops).unwrap(),
                text(&workspace, path),
                "{path}"
            );
        }
    }
}
