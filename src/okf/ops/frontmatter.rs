//! The concept commands `stamp`, `review`, `question`, `unquestion` and
//! `source` (`okf.py:653-750`): textual edits of one concept's frontmatter.
//! [`remove_source`] has no `okf.py` counterpart: it is the server's own
//! inverse of `source`.
//!
//! Divergences from `okf.py` (none reachable from the parity script):
//! - `unquestion` reads only ASCII digits as an index; Python's `isdigit`
//!   also accepts other Unicode digits;
//! - `review` refuses an outcome outside [`OUTCOMES`] itself (argparse
//!   refuses it in `okf.py`).

use indexmap::IndexMap;

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::okf::markdown::{footnote_defs, footnote_refs};
use crate::okf::ops::{
    load_concept, refused, save, stamp_value, LoadedConcept, OpContext, OpOutcome, SourceEntry,
    StampField,
};
use crate::okf::pystr::{py_repr_str, py_str};
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{block_item, flow_value, scalar, ScalarStyle, Value};

/// `okf.py`'s `OUTCOMES`: what a review may conclude.
pub const OUTCOMES: [&str; 4] = ["verified", "updated", "partial", "unverifiable"];

const OPEN_QUESTIONS: &str = "open_questions";
const SOURCES: &str = "sources";

/// One always-quoted list item: `" " * ind + "- " + scalar(q, force=True)`.
fn quoted_item(text: &str, indent: usize) -> String {
    format!(
        "{}- {}",
        " ".repeat(indent),
        scalar(text, ScalarStyle::Quoted)
    )
}

/// `okf.py`'s `render=lambda v, ind: [... scalar(v, force=True)]`: an
/// existing item re-rendered when a flow list becomes a block list. Python's
/// `scalar` calls `str(v)` first, so a mapping item becomes its `repr` text.
fn render_quoted(value: &Value, indent: usize) -> Vec<String> {
    vec![quoted_item(&py_str(value), indent)]
}

/// A key's list items, refusing anything but a list or nothing
/// (`okf.py`'s "`<key>` is not a list; fix it by hand").
fn list_or_nothing<'d>(
    path: &str,
    data: &'d IndexMap<String, Value>,
    key: &str,
) -> Result<&'d [Value], OkfError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Seq(items)) => Ok(items),
        Some(Value::Str(_) | Value::Map(_)) => Err(OkfError::Shape {
            path: path.to_string(),
            key: key.to_string(),
        }),
    }
}

/// Port of `cmd_stamp`: sets `generated`, or appends to `verified`, with
/// `{ by, at: now }`.
///
/// # Arguments
/// * `workspace` - the catalog (an overlay copy in the server).
/// * `ctx` - the frozen "now" and the actor.
/// * `concept` - the concept reference (`/dir/x`, `dir/x.md`).
/// * `field` - which stamp.
///
/// # Returns
/// The outcome; its message is `okf.py`'s, e.g.
/// `infra/x.md: generated { by: a, at: 2026-11-03T12:00:00Z }`.
///
/// # Errors
/// [`OkfError::NotFound`] when no concept matches `concept`;
/// [`OkfError::InvalidPath`] when it names a non-concept (`log.md`, an
/// `index.md`); [`OkfError::Refused`] when it has no frontmatter;
/// [`OkfError::FrontmatterParse`] when its frontmatter does not parse, or
/// the edit would break it; [`OkfError::Shape`] when `verified` is a scalar.
pub fn stamp(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    concept: &str,
    field: StampField,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept { path, mut doc, .. } = load_concept(workspace, concept)?;
    let item = flow_value(&stamp_value(ctx));
    // `match` must cover every variant: a third stamp field would not compile here.
    match field {
        StampField::Generated => doc.set_block("generated", vec![format!("generated: {item}")]),
        StampField::Verified => doc.append_seq_item(
            "verified",
            |indent| vec![format!("{}- {item}", " ".repeat(indent))],
            block_item,
        )?,
    }
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: {} {item}", field.as_str());
    Ok(outcome)
}

/// Port of `cmd_review`: replaces `reviewed` and queues open questions not
/// already queued.
///
/// # Arguments
/// * `workspace`, `ctx`, `concept` - as for [`stamp`].
/// * `outcome` - one of [`OUTCOMES`].
/// * `note` - what was and wasn't checked (always double-quoted).
/// * `questions` - open questions to queue; duplicates are reported, not added.
///
/// # Returns
/// The outcome; its message is one "already queued" line per duplicate,
/// then `<path>: reviewed <outcome> at <ts>[; queued N question(s)]`.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown outcome; as [`stamp`] otherwise
/// ([`OkfError::Shape`] when `open_questions` is a scalar).
pub fn review(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    concept: &str,
    outcome: &str,
    note: &str,
    questions: &[String],
) -> Result<OpOutcome, OkfError> {
    if !OUTCOMES.contains(&outcome) {
        return Err(refused(format!(
            "invalid outcome {}: choose from {}",
            py_repr_str(outcome),
            OUTCOMES.join(", ")
        )));
    }
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let at = format_timestamp(ctx.now);
    doc.set_block(
        "reviewed",
        vec![
            "reviewed:".to_string(),
            format!("  by: {}", scalar(ctx.by, ScalarStyle::Block)),
            format!("  at: {at}"),
            format!("  outcome: {outcome}"),
            format!("  note: {}", scalar(note, ScalarStyle::Quoted)),
        ],
    );
    // A non-list `open_questions` counts as empty here, as in okf.py; the
    // append below then converts a mapping or refuses a scalar.
    let have: &[Value] = match data.get(OPEN_QUESTIONS) {
        Some(Value::Seq(items)) => items,
        None | Some(Value::Null | Value::Str(_) | Value::Map(_)) => &[],
    };
    let mut added: Vec<&str> = Vec::new();
    let mut lines = Vec::new();
    for question in questions {
        let queued = have.iter().any(|v| v.as_str() == Some(question.as_str()))
            || added.contains(&question.as_str());
        if queued {
            lines.push(format!("{path}: open question already queued: {question}"));
            continue;
        }
        doc.append_seq_item(
            OPEN_QUESTIONS,
            |indent| vec![quoted_item(question, indent)],
            render_quoted,
        )?;
        added.push(question);
    }
    let mut result = OpOutcome::default();
    save(workspace, &doc, &mut result)?;
    let mut summary = format!("{path}: reviewed {outcome} at {at}");
    if !added.is_empty() {
        summary.push_str(&format!("; queued {} question(s)", added.len()));
    }
    lines.push(summary);
    result.message = lines.join("\n");
    Ok(result)
}

/// Port of `cmd_question`: queues one open question without a review.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused (no
///   timestamp), kept so every concept op has the same shape.
/// * `question` - the question, stored double-quoted.
///
/// # Returns
/// The outcome; nothing is touched when the question is already queued.
///
/// # Errors
/// As [`stamp`] when loading the concept; [`OkfError::Shape`] when
/// `open_questions` is not a list.
pub fn question(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    question: &str,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let existing = list_or_nothing(&path, &data, OPEN_QUESTIONS)?;
    if existing.iter().any(|v| v.as_str() == Some(question)) {
        return Ok(OpOutcome::unchanged(format!(
            "{path}: open question already queued: {question}"
        )));
    }
    doc.append_seq_item(
        OPEN_QUESTIONS,
        |indent| vec![quoted_item(question, indent)],
        render_quoted,
    )?;
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: queued open question: {question}");
    Ok(outcome)
}

/// The 0-based index `unquestion` removes: a 1-based number, else the
/// item whose text is exactly `question`.
fn question_index(path: &str, questions: &[Value], question: &str) -> Result<usize, OkfError> {
    let is_number = !question.is_empty() && question.bytes().all(|b| b.is_ascii_digit());
    if is_number {
        // `parse` fails only on overflow here, and such an index is out of range anyway.
        return question
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=questions.len()).contains(n))
            .map(|n| n - 1)
            .ok_or_else(|| {
                refused(format!(
                    "{path} has {} open question(s); index {question} is out of range (1-based)",
                    questions.len()
                ))
            });
    }
    questions
        .iter()
        .position(|v| v.as_str() == Some(question))
        .ok_or_else(|| {
            refused(format!(
                "no open question in {path} exactly matches {}",
                py_repr_str(question)
            ))
        })
}

/// Port of `cmd_unquestion`: removes one open question, deleting the key
/// when none are left.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused.
/// * `question` - a 1-based index (as `oldest --open-questions` lists them)
///   or the question's exact text.
///
/// # Returns
/// The outcome; the message names the removed question (Python's `str()`
/// of it, so a mapping item prints as `{'k': 'v'}`).
///
/// # Errors
/// [`OkfError::Refused`] when there are no open questions, the index is out
/// of range, or no question matches; as [`stamp`] when loading the concept.
pub fn unquestion(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    question: &str,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let questions = match data.get(OPEN_QUESTIONS) {
        Some(Value::Seq(items)) if !items.is_empty() => items,
        None | Some(Value::Null | Value::Str(_) | Value::Seq(_) | Value::Map(_)) => {
            return Err(refused(format!("{path} has no open_questions")));
        }
    };
    let index = question_index(&path, questions, question)?;
    let removed = py_str(&questions[index]);
    let last = questions.len() == 1;
    if last {
        doc.delete_block(OPEN_QUESTIONS);
    } else {
        match doc.seq_items(OPEN_QUESTIONS) {
            Some(items) if items.spans.len() == questions.len() => {
                doc.remove_lines(items.spans[index].clone());
            }
            // A flow list, on one line or several, is rewritten as a block
            // list of the remaining items. okf.py takes this branch for any
            // list `seq_items` can't map item by item; probing okf.py found
            // no block list that parses and gets here (a continuation or
            // indent `seq_items` refuses also fails to parse), so in practice
            // it is flow lists only.
            _ => {
                let mut lines = vec![format!("{OPEN_QUESTIONS}:")];
                lines.extend(
                    questions
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != index)
                        .map(|(_, q)| quoted_item(&py_str(q), 2)),
                );
                doc.set_block(OPEN_QUESTIONS, lines);
            }
        }
    }
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    let left = if last {
        " (key deleted, none left)"
    } else {
        ""
    };
    outcome.message = format!("{path}: removed open question: {removed}{left}");
    Ok(outcome)
}

/// Port of `cmd_source`: appends an entry to `sources`, refusing a
/// duplicate id.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused.
/// * `entry` - the new entry.
///
/// # Returns
/// The outcome, message `<path>: added source <id>`.
///
/// # Errors
/// [`OkfError::Shape`] when `sources` is not a list; [`OkfError::Refused`]
/// when an entry with this id exists; as [`stamp`] when loading the concept.
pub fn source(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    entry: &SourceEntry<'_>,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let existing = list_or_nothing(&path, &data, SOURCES)?;
    // `matches!(x, Pattern if guard)` is a one-line `match` returning a bool.
    let duplicate = existing.iter().any(
        |s| matches!(s, Value::Map(map) if map.get("id").and_then(Value::as_str) == Some(entry.id)),
    );
    if duplicate {
        return Err(refused(format!(
            "{path} already has a source with id {}",
            py_repr_str(entry.id)
        )));
    }
    let mut fields = IndexMap::new();
    fields.insert("id".to_string(), Value::Str(entry.id.to_string()));
    fields.insert(
        "resource".to_string(),
        Value::Str(entry.resource.to_string()),
    );
    for (key, value) in [("title", entry.title), ("author", entry.author)] {
        if let Some(text) = value.filter(|t| !t.is_empty()) {
            fields.insert(key.to_string(), Value::Str(text.to_string()));
        }
    }
    let item = Value::Map(fields);
    doc.append_seq_item(SOURCES, |indent| block_item(&item, indent), block_item)?;
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: added source {}", entry.id);
    Ok(outcome)
}

/// Does `text` hold `token` with no word character (letter, digit, `_`,
/// `-`) on either side? So id `s1` matches "see s1." but not "s12".
fn contains_token(text: &str, token: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    // `match_indices` yields each occurrence's byte offset, like `re.finditer`.
    text.match_indices(token).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + token.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// The label of a footnote in `body` that cites `source`, if any: one
/// labelled with its id (referenced or defined), or one whose definition
/// holds its id as a token, or its title or resource.
///
/// Stricter than `split_concept`'s choice of sources to carry, which also
/// counts the id as a bare substring: a false match there copies one extra
/// source, but here it would refuse a removal for good.
fn citing_footnote(body: &str, source: &IndexMap<String, Value>, id: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let defs = footnote_defs(&lines);
    if defs.contains_key(id) || footnote_refs(body).contains(id) {
        return Some(id.to_string());
    }
    let needles: Vec<&str> = ["title", "resource"]
        .iter()
        .filter_map(|key| source.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect();
    defs.into_iter()
        .find(|(_, indexes)| {
            let text: String = indexes
                .iter()
                .map(|&i| lines[i])
                .collect::<Vec<_>>()
                .join("\n");
            contains_token(&text, id) || needles.iter().any(|needle| text.contains(needle))
        })
        .map(|(label, _)| label)
}

/// Removes the `sources` entry with id `id`, deleting the key when none are
/// left; refused while a footnote in the body still cites it.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused.
/// * `id` - the entry's `id`.
///
/// # Returns
/// The outcome, message `<path>: removed source <id>` (plus
/// ` (key deleted, none left)` for the last one).
///
/// # Errors
/// [`OkfError::Shape`] when `sources` is not a list; [`OkfError::Refused`]
/// when no entry has this id, or a footnote cites it (the message names the
/// footnote); as [`stamp`] when loading the concept.
pub fn remove_source(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    id: &str,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let sources = list_or_nothing(&path, &data, SOURCES)?;
    let found = sources
        .iter()
        .enumerate()
        .find_map(|(index, source)| match source {
            Value::Map(map) if map.get("id").and_then(Value::as_str) == Some(id) => {
                Some((index, map))
            }
            Value::Null | Value::Str(_) | Value::Seq(_) | Value::Map(_) => None,
        });
    let Some((index, entry)) = found else {
        return Err(refused(format!(
            "{path} has no source with id {}",
            py_repr_str(id)
        )));
    };
    if let Some(label) = citing_footnote(&doc.body(), entry, id) {
        return Err(refused(format!(
            "footnote [^{label}] in {path} still cites source {}; \
             remove or rewrite the citation first",
            py_repr_str(id)
        )));
    }
    let last = sources.len() == 1;
    if last {
        doc.delete_block(SOURCES);
    } else {
        match doc.seq_items(SOURCES) {
            Some(items) if items.spans.len() == sources.len() => {
                doc.remove_lines(items.spans[index].clone());
            }
            // A flow list (`sources: [ ... ]`) has no per-item lines, so it
            // is rewritten as a block list of the remaining entries.
            _ => {
                let mut lines = vec![format!("{SOURCES}:")];
                lines.extend(
                    sources
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != index)
                        .flat_map(|(_, source)| block_item(source, 2)),
                );
                doc.set_block(SOURCES, lines);
            }
        }
    }
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    let left = if last {
        " (key deleted, none left)"
    } else {
        ""
    };
    outcome.message = format!("{path}: removed source {id}{left}");
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    /// The frozen clock and actor every test uses.
    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn read(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn stamp_generated_replaces_the_block() {
        let ws = MemWorkspace::from_files([(
            "a.md",
            "---\ntype: Note\ngenerated: { by: old, at: 2026-01-01T00:00:00Z }\n---\nBody\n",
        )]);

        let outcome = stamp(&ws, &ctx(), "a", StampField::Generated).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            "---\ntype: Note\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n---\nBody\n"
        );
        assert_eq!(outcome.touched, ["a.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(
            outcome.message,
            "a.md: generated { by: tester, at: 2026-11-03T12:00:00Z }"
        );
    }

    #[test]
    fn stamp_verified_appends_after_the_last_item() {
        let ws = MemWorkspace::from_files([(
            "a.md",
            "---\ntype: Note\nverified:\n  - by: a\n    at: 2026-01-01T00:00:00Z\n---\n",
        )]);

        stamp(&ws, &ctx(), "a.md", StampField::Verified).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            concat!(
                "---\ntype: Note\nverified:\n  - by: a\n    at: 2026-01-01T00:00:00Z\n",
                "  - { by: tester, at: 2026-11-03T12:00:00Z }\n---\n",
            )
        );
    }

    #[test]
    fn stamp_keeps_crlf_line_endings() {
        let ws = MemWorkspace::from_files([("c.md", "---\r\ntype: Note\r\n---\r\nBody\r\n")]);

        stamp(&ws, &ctx(), "c", StampField::Generated).unwrap();

        assert_eq!(
            read(&ws, "c.md"),
            "---\r\ntype: Note\r\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\r\n---\r\nBody\r\n"
        );
    }

    #[test]
    fn ops_refuse_missing_or_broken_frontmatter_and_non_concepts() {
        let ws = MemWorkspace::from_files([
            ("n.md", "Text only\n"),
            ("b.md", "---\ntype: Note\ntype: again\n---\n"),
            ("log.md", "# Log\n"),
        ]);

        let none = stamp(&ws, &ctx(), "n", StampField::Generated).unwrap_err();
        let broken = question(&ws, &ctx(), "b", "Why?").unwrap_err();
        let missing = stamp(&ws, &ctx(), "nope", StampField::Generated).unwrap_err();
        let log = stamp(&ws, &ctx(), "log.md", StampField::Generated).unwrap_err();

        assert_eq!(
            refusal(none),
            "n.md has no frontmatter; add it with your editor first"
        );
        assert!(matches!(broken, OkfError::FrontmatterParse { line: 3, .. }));
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(log, OkfError::InvalidPath { .. }));
        assert_eq!(read(&ws, "n.md"), "Text only\n");
    }

    #[test]
    fn review_replaces_reviewed_and_queues_only_new_questions() {
        let ws = MemWorkspace::from_files([(
            "q.md",
            "---\ntype: Note\nreviewed: { by: x, at: y, outcome: verified }\nopen_questions:\n  - Old?\n---\n",
        )]);
        let questions = ["Old?", "New?", "New?"].map(String::from);

        let outcome = review(&ws, &ctx(), "q", "partial", "Checked: half", &questions).unwrap();

        assert_eq!(
            read(&ws, "q.md"),
            concat!(
                "---\ntype: Note\nreviewed:\n  by: tester\n  at: 2026-11-03T12:00:00Z\n",
                "  outcome: partial\n  note: \"Checked: half\"\n",
                "open_questions:\n  - Old?\n  - \"New?\"\n---\n",
            )
        );
        assert_eq!(
            outcome.message,
            concat!(
                "q.md: open question already queued: Old?\n",
                "q.md: open question already queued: New?\n",
                "q.md: reviewed partial at 2026-11-03T12:00:00Z; queued 1 question(s)",
            )
        );
    }

    #[test]
    fn review_rejects_an_unknown_outcome() {
        let ws = MemWorkspace::from_files([("q.md", "---\ntype: Note\n---\n")]);

        let error = review(&ws, &ctx(), "q", "maybe", "n", &[]).unwrap_err();

        assert_eq!(
            refusal(error),
            "invalid outcome 'maybe': choose from verified, updated, partial, unverifiable"
        );
    }

    #[test]
    fn question_already_queued_changes_nothing() {
        let text = "---\ntype: Note\nopen_questions:\n  - Old?\n---\n";
        let ws = MemWorkspace::from_files([("q.md", text)]);

        let outcome = question(&ws, &ctx(), "q", "Old?").unwrap();

        assert!(outcome.touched.is_empty());
        assert_eq!(outcome.message, "q.md: open question already queued: Old?");
        assert_eq!(read(&ws, "q.md"), text);
    }

    #[test]
    fn question_converts_a_flow_list_and_refuses_a_scalar() {
        let ws = MemWorkspace::from_files([
            (
                "f.md",
                "---\ntype: Note\nopen_questions: [a, \"b, c\"]\n---\n",
            ),
            ("s.md", "---\ntype: Note\nopen_questions: just text\n---\n"),
        ]);

        let outcome = question(&ws, &ctx(), "f", "d").unwrap();
        let scalar = question(&ws, &ctx(), "s", "d").unwrap_err();

        assert_eq!(
            read(&ws, "f.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"b, c\"\n  - \"d\"\n---\n"
        );
        assert_eq!(outcome.message, "f.md: queued open question: d");
        assert!(matches!(scalar, OkfError::Shape { ref key, .. } if key == "open_questions"));
    }

    #[test]
    fn unquestion_by_index_then_text_then_the_last_one() {
        let ws = MemWorkspace::from_files([(
            "u.md",
            "---\ntype: Note\nopen_questions:\n  - First?\n  - Second?\n  - Third?\n---\n",
        )]);

        let by_index = unquestion(&ws, &ctx(), "u", "2").unwrap();
        let after_index = read(&ws, "u.md");
        let by_text = unquestion(&ws, &ctx(), "u", "Third?").unwrap();
        let last = unquestion(&ws, &ctx(), "u", "1").unwrap();

        assert_eq!(by_index.message, "u.md: removed open question: Second?");
        assert_eq!(
            after_index,
            "---\ntype: Note\nopen_questions:\n  - First?\n  - Third?\n---\n"
        );
        assert_eq!(by_text.message, "u.md: removed open question: Third?");
        assert_eq!(
            last.message,
            "u.md: removed open question: First? (key deleted, none left)"
        );
        assert_eq!(read(&ws, "u.md"), "---\ntype: Note\n---\n");
    }

    #[test]
    fn unquestion_rewrites_a_flow_list_and_prints_a_mapping_as_python_does() {
        let ws = MemWorkspace::from_files([(
            "f.md",
            "---\ntype: Note\nopen_questions: [a, { k: v }, \"b, c\"]\n---\n",
        )]);

        let outcome = unquestion(&ws, &ctx(), "f", "2").unwrap();

        assert_eq!(outcome.message, "f.md: removed open question: {'k': 'v'}");
        assert_eq!(
            read(&ws, "f.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"b, c\"\n---\n"
        );
    }

    #[test]
    fn unquestion_matches_okf_py_on_a_multi_line_flow_list_and_a_mapping_item() {
        // Expected text is okf.py's output for the same files and steps.
        let ws = MemWorkspace::from_files([
            ("m.md", "---\ntype: Note\nopen_questions: [a,\n  { k: v }, c]\n---\n"),
            (
                "b.md",
                "---\ntype: Note\nopen_questions:\n  - k: v\n    j: w\n  # keep\n  - b\n  - c\n---\n",
            ),
        ]);

        let flow = unquestion(&ws, &ctx(), "m", "3").unwrap();
        let middle = unquestion(&ws, &ctx(), "b", "2").unwrap();
        let mapping = unquestion(&ws, &ctx(), "b", "1").unwrap();

        assert_eq!(flow.message, "m.md: removed open question: c");
        assert_eq!(
            read(&ws, "m.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"{'k': 'v'}\"\n---\n"
        );
        assert_eq!(middle.message, "b.md: removed open question: b");
        assert_eq!(
            mapping.message,
            "b.md: removed open question: {'k': 'v', 'j': 'w'}"
        );
        assert_eq!(
            read(&ws, "b.md"),
            "---\ntype: Note\nopen_questions:\n  # keep\n  - c\n---\n"
        );
    }

    #[test]
    fn unquestion_refusals_use_okf_py_wording() {
        let ws = MemWorkspace::from_files([
            (
                "u.md",
                "---\ntype: Note\nopen_questions:\n  - A?\n  - B?\n---\n",
            ),
            ("e.md", "---\ntype: Note\n---\n"),
        ]);

        let high = unquestion(&ws, &ctx(), "u", "3").unwrap_err();
        let zero = unquestion(&ws, &ctx(), "u", "0").unwrap_err();
        let text = unquestion(&ws, &ctx(), "u", "Nope").unwrap_err();
        let none = unquestion(&ws, &ctx(), "e", "1").unwrap_err();

        assert_eq!(
            refusal(high),
            "u.md has 2 open question(s); index 3 is out of range (1-based)"
        );
        assert_eq!(
            refusal(zero),
            "u.md has 2 open question(s); index 0 is out of range (1-based)"
        );
        assert_eq!(
            refusal(text),
            "no open question in u.md exactly matches 'Nope'"
        );
        assert_eq!(refusal(none), "e.md has no open_questions");
    }

    #[test]
    fn source_appends_a_block_item_and_skips_an_empty_title() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n---\n",
        )]);
        let entry = SourceEntry {
            id: "two",
            resource: "a: b",
            title: Some(""),
            author: Some("human:pgrace"),
        };

        let outcome = source(&ws, &ctx(), "s", &entry).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            concat!(
                "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n",
                "  - id: two\n    resource: \"a: b\"\n    author: human:pgrace\n---\n",
            )
        );
        assert_eq!(outcome.message, "s.md: added source two");
    }

    #[test]
    fn source_refuses_a_duplicate_id_and_a_scalar() {
        let ws = MemWorkspace::from_files([
            ("s.md", "---\ntype: Note\nsources:\n  - { id: one }\n---\n"),
            ("x.md", "---\ntype: Note\nsources: nope\n---\n"),
        ]);
        let entry = SourceEntry {
            id: "one",
            resource: "r",
            title: None,
            author: None,
        };

        let duplicate = source(&ws, &ctx(), "s", &entry).unwrap_err();
        let scalar = source(&ws, &ctx(), "x", &entry).unwrap_err();

        assert_eq!(
            refusal(duplicate),
            "s.md already has a source with id 'one'"
        );
        assert!(matches!(scalar, OkfError::Shape { ref key, .. } if key == "sources"));
    }

    #[test]
    fn remove_source_drops_a_block_item_and_its_continuation_lines() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            concat!(
                "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n",
                "  - id: two\n    resource: r2\n    title: Two\n---\nCited.[^one]\n\n",
                "[^one]: r1\n",
            ),
        )]);

        let outcome = remove_source(&ws, &ctx(), "s", "two").unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n---\nCited.[^one]\n\n\
             [^one]: r1\n"
        );
        assert_eq!(outcome.touched, ["s.md"]);
        assert_eq!(outcome.message, "s.md: removed source two");
    }

    #[test]
    fn remove_source_deletes_the_key_with_the_last_entry() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            "---\ntype: Note\nsources:\n  - { id: one, resource: r }\nstatus: draft\n---\nBody\n",
        )]);

        let outcome = remove_source(&ws, &ctx(), "s", "one").unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\nstatus: draft\n---\nBody\n"
        );
        assert_eq!(
            outcome.message,
            "s.md: removed source one (key deleted, none left)"
        );
    }

    #[test]
    fn remove_source_rewrites_a_flow_list_as_a_block_list() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            "---\ntype: Note\nsources: [{ id: a, resource: ra }, { id: b, resource: rb }]\n---\n",
        )]);

        remove_source(&ws, &ctx(), "s", "a").unwrap();

        let text = read(&ws, "s.md");
        assert!(
            text.starts_with("---\ntype: Note\nsources:\n  - "),
            "{text}"
        );
        assert!(text.contains("rb") && !text.contains("ra"), "{text}");
    }

    #[test]
    fn remove_source_refuses_a_cited_entry_and_names_the_footnote() {
        let body = concat!(
            "---\ntype: Note\nsources:\n",
            "  - { id: lab, resource: r0 }\n",
            "  - { id: s1, resource: https://example.com/doc, title: Runbook }\n",
            "  - { id: s2, resource: r2 }\n",
            "  - { id: s3, resource: r3 }\n",
            "---\nA[^lab] B[^x] C[^y] D[^z]\n\n",
            "[^x]: The https://example.com/doc page.\n",
            "[^y]: See the\n    Runbook\n",
            "[^z]: See s2, section 4.\n",
        );
        let ws = MemWorkspace::from_files([("s.md", body)]);

        let by_label = refusal(remove_source(&ws, &ctx(), "s", "lab").unwrap_err());
        let by_resource = refusal(remove_source(&ws, &ctx(), "s", "s1").unwrap_err());
        let by_id = refusal(remove_source(&ws, &ctx(), "s", "s2").unwrap_err());
        remove_source(&ws, &ctx(), "s", "s3").unwrap();

        assert_eq!(
            by_label,
            "footnote [^lab] in s.md still cites source 'lab'; \
             remove or rewrite the citation first"
        );
        assert!(by_resource.starts_with("footnote [^x] "), "{by_resource}");
        assert!(by_id.starts_with("footnote [^z] "), "{by_id}");
        // `s3` matched nothing (and `s2` in [^z] is not `s3`): it went.
        assert!(!read(&ws, "s.md").contains("r3"));
    }

    #[test]
    fn remove_source_counts_a_title_on_a_continuation_line() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            "---\ntype: Note\nsources:\n  - { id: s, resource: r, title: Runbook }\n---\n\
             [^n]: See the\n    Runbook\n",
        )]);

        let refused = refusal(remove_source(&ws, &ctx(), "s", "s").unwrap_err());

        assert!(refused.starts_with("footnote [^n] "), "{refused}");
    }

    #[test]
    fn remove_source_refuses_an_unknown_id_and_a_scalar() {
        let ws = MemWorkspace::from_files([
            (
                "s.md",
                "---\ntype: Note\nsources:\n  - { id: one, resource: r }\n---\n",
            ),
            ("e.md", "---\ntype: Note\n---\n"),
            ("x.md", "---\ntype: Note\nsources: nope\n---\n"),
        ]);

        let unknown = refusal(remove_source(&ws, &ctx(), "s", "two").unwrap_err());
        let empty = refusal(remove_source(&ws, &ctx(), "e", "one").unwrap_err());
        let scalar = remove_source(&ws, &ctx(), "x", "one").unwrap_err();

        assert_eq!(unknown, "s.md has no source with id 'two'");
        assert_eq!(empty, "e.md has no source with id 'one'");
        assert!(matches!(scalar, OkfError::Shape { ref key, .. } if key == "sources"));
    }

    #[test]
    fn contains_token_needs_a_word_boundary_on_both_sides() {
        assert!(contains_token("see s1.", "s1"));
        assert!(contains_token("s1", "s1"));
        assert!(!contains_token("see s12", "s1"));
        assert!(!contains_token("as1", "s1"));
        assert!(!contains_token("s1-b", "s1"));
        // A multibyte neighbour is a char, not a byte: no slicing panic.
        assert!(!contains_token("\u{e9}s1", "s1"));
        assert!(contains_token("\u{2014}s1\u{2014}", "s1"));
    }
}
