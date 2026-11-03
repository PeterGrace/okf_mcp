//! Replay operations: how a bookkeeping edit of `log.md` or an `index.md`
//! is redone on newer text at publish (design "Data flow", "Write" step 3
//! and "Publish" step 3), and the pure text edits that both the ops
//! (`log_index.rs`) and [`replay`] run, so an edit and its replay cannot
//! drift apart. M6 adds [`ReplayOp::Unlist`] (a listing taken out of a
//! folder index) and [`ReplayOp::Relink`] (links redirected by a move or
//! split), whose edits are `unlist_text` and `relink::rewrite_text` (both
//! crate-internal: a public doc may not link them, or `cargo doc -D
//! warnings` fails). `Unlist` works on universal-newline text, so a CRLF
//! `index.md` comes out with `\n` endings, while `Relink` keeps bytes as
//! they are. Both match `okf.py`: `cmd_mv` reads and writes the old index
//! in text mode (`okf.py:1538`), and `rewrite_links` opens files with
//! `newline=''` (`okf.py:1359`). This is not a parity bug.
//!
//! [`log_text`] is `okf.py`'s `cmd_log` body (`:752`) and
//! [`insert_index_bullet_text`] its `insert_index_bullet` (`:1410`), line
//! for line. `okf.py` reads and writes these files in text mode, so both
//! work on universal-newline text and produce `\n` endings.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::okf::analysis::links::INDEX_HEADING;
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::ops::relink::{rewrite_text, Redirect};
use crate::okf::paths::{basename, is_concept};
use crate::okf::pystr::{py_repr_str, py_strip, universal_newlines};
use crate::okf::workspace::{MemWorkspace, Workspace};

/// One replayable bookkeeping edit, stored in a changeset entry's `replay`
/// column as JSON (`{"op": "log", "date": ..., "bullet": ...}`).
///
/// `#[serde(tag = "op")]` writes the variant name into an `op` field next
/// to the variant's own fields, like a Python dict with a "type" key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReplayOp {
    /// `log`: add `bullet` under `## <date>` in `log.md`.
    Log {
        /// The UTC date of the heading, `2026-11-03`.
        date: String,
        /// The whole bullet line, `* **Update:** text`.
        bullet: String,
    },
    /// `index` and folder listings: add `bullet` to an `index.md` unless it
    /// already links `target`.
    ListBullet {
        /// Catalog-relative path the bullet lists (a concept, or a folder's
        /// `index.md`).
        target: String,
        /// The whole bullet line.
        bullet: String,
        /// The heading to put it under; `None` for `okf.py`'s default place.
        section: Option<String>,
        /// Create the heading when it is missing (folder listings under `# Sections`).
        create_section: bool,
    },
    /// `ensure_index`: create a folder's `index.md` unless it exists.
    CreateIndex {
        /// Catalog-relative folder, `infra/hosts`.
        folder: String,
    },
    /// `move_concept` and `delete_concept`: remove the first bullet of an
    /// `index.md` that links `target`.
    Unlist {
        /// Catalog-relative path the bullet lists.
        target: String,
        /// `Some`: the bullet as it read when the edit was made. A move
        /// carries that text to the new folder, so the replay refuses a
        /// listing that reads otherwise now, rather than drop someone
        /// else's change to it. `None`: whatever it reads (a delete).
        bullet: Option<String>,
    },
    /// `move_concept` and `split_concept`: redirect the links of an
    /// `index.md` or `log.md` that point into the moved document or section.
    Relink {
        /// Which links, and where to.
        redirect: Redirect,
    },
}

/// Redoes `ops` on a file's current text (design "Publish", step 3).
///
/// Idempotent where publish needs it: a log bullet already under its date,
/// a listing whose `target` the index already links, a `CreateIndex` of a
/// file that exists, an `Unlist` whose listing is gone, and a `Relink` with
/// no link left into its source all leave the text alone. Text that no operation
/// changes is returned byte for byte; text a bookkeeping edit changed has
/// `\n` endings, as `okf.py` writes it, while a `Relink` keeps the file's
/// own line endings.
///
/// # Arguments
/// * `path` - the file's catalog-relative path (`log.md`, `infra/index.md`),
///   used to resolve relative links and in errors.
/// * `current` - its current published text; `None` when it does not exist.
/// * `ops` - the operations recorded for it, in order.
///
/// # Returns
/// The text after the operations.
///
/// # Errors
/// [`OkfError::NotFound`] when a `Log` or `ListBullet` (or nothing) finds
/// no file (as do `Unlist` and `Relink`); [`OkfError::Refused`] when a
/// listing's section heading is gone, or when a move's `Unlist` finds its
/// listing changed since the move.
///
/// # Examples
/// ```
/// use okf_mcp::okf::ops::{replay, ReplayOp};
/// let ops = [ReplayOp::Log { date: "2026-11-03".into(), bullet: "* **Update:** x".into() }];
/// let once = replay("log.md", Some("# Log\n"), &ops).unwrap();
/// assert_eq!(once, "# Log\n\n## 2026-11-03\n\n* **Update:** x\n");
/// assert_eq!(replay("log.md", Some(&once), &ops).unwrap(), once);
/// ```
pub fn replay(path: &str, current: Option<&str>, ops: &[ReplayOp]) -> Result<String, OkfError> {
    // Publish has no directory listing, so links resolve as if no path were
    // a directory; `lists_target` then also accepts `folder` for `folder/index.md`.
    let no_dirs = MemWorkspace::default();
    // `Cow` holds either the borrowed `current` or a newly built `String`,
    // so unchanged text is never copied.
    let mut text: Option<Cow<'_, str>> = current.map(Cow::Borrowed);
    for op in ops {
        let updated = match op {
            ReplayOp::CreateIndex { folder } => text.is_none().then(|| new_index_text(folder)),
            ReplayOp::Log { date, bullet } => {
                log_text(&universal_newlines(present(path, &text)?), date, bullet)
            }
            ReplayOp::ListBullet {
                target,
                bullet,
                section,
                create_section,
            } => {
                let base = universal_newlines(present(path, &text)?);
                if lists_target(&no_dirs, &base, path, target) {
                    None
                } else {
                    let section = section.as_deref();
                    let inserted =
                        insert_index_bullet_text(&base, bullet, section, *create_section);
                    Some(inserted.ok_or_else(|| missing_heading(path, section.unwrap_or("")))?)
                }
            }
            ReplayOp::Unlist { target, bullet } => {
                let base = universal_newlines(present(path, &text)?);
                match unlist_text(&no_dirs, &base, path, target) {
                    // Already gone: nothing to do (idempotent).
                    None => None,
                    // `is_some_and` is false for `None`: a delete removes any bullet.
                    Some((_, found)) if bullet.as_deref().is_some_and(|want| want != found) => {
                        return Err(changed_listing(path, target));
                    }
                    Some((rest, _)) => Some(rest),
                }
            }
            // Byte for byte, as the op itself rewrote the file.
            ReplayOp::Relink { redirect } => {
                // `no_dirs` cannot tell a directory link (`[x](../a)`) from a file
                // link, which only matters when `from` is an `index.md`. Moves and
                // splits only redirect concepts (see `rewrite_text`'s doc).
                debug_assert!(
                    is_concept(&redirect.from),
                    "a Relink redirects a concept, never an index.md or log.md"
                );
                rewrite_text(&no_dirs, path, path, present(path, &text)?, redirect)
            }
        };
        if let Some(updated) = updated {
            text = Some(Cow::Owned(updated));
        }
    }
    text.map(Cow::into_owned).ok_or_else(|| OkfError::NotFound {
        path: path.to_string(),
    })
}

/// The current text, or `NotFound` for an operation that needs a file.
fn present<'t>(path: &str, text: &'t Option<Cow<'_, str>>) -> Result<&'t str, OkfError> {
    text.as_deref().ok_or_else(|| OkfError::NotFound {
        path: path.to_string(),
    })
}

/// `okf.py`'s "no heading 'X' in <path>" refusal of a listing.
pub(crate) fn missing_heading(path: &str, section: &str) -> OkfError {
    OkfError::Refused {
        reason: format!("no heading {} in {path}", py_repr_str(section)),
    }
}

/// Does `text` (an `index.md` at `index`) link `target`? `cmd_index`'s
/// "already lists" check. A link resolving to `folder` also counts for
/// `folder/index.md`, which only matters when `workspace` cannot tell that
/// `folder` is a directory (replay): a concept target never ends in
/// `/index.md`, so `index`'s own check is exactly `okf.py`'s.
pub(crate) fn lists_target(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> bool {
    find_links(text)
        .iter()
        .filter_map(|link| resolve_link(workspace, &link.target, index))
        .any(|resolved| {
            resolved.path == target
                || target.strip_suffix("/index.md") == Some(resolved.path.as_str())
        })
}

/// The line of `text` (an `index.md` at `index`) that lists `target`: the
/// first bullet (`* ` or `- `) with a link resolving to it, as `cmd_mv`
/// takes it.
///
/// # Arguments
/// * `workspace` - resolves directory links (replay passes an empty one).
/// * `text` - the index, with universal newlines.
/// * `index` - its catalog-relative path, to resolve relative links.
/// * `target` - the catalog-relative path listed.
///
/// # Returns
/// The 0-based line index, or `None` when no bullet links `target`.
pub(crate) fn listing_line(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> Option<usize> {
    text.split('\n').position(|line| {
        is_bullet(line)
            && find_links(line).iter().any(|link| {
                resolve_link(workspace, &link.target, index)
                    .is_some_and(|resolved| resolved.path == target)
            })
    })
}

/// `cmd_mv`'s edit of the old folder's index: the text without the line
/// [`listing_line`] finds.
///
/// # Arguments
/// As [`listing_line`].
///
/// # Returns
/// The text without that line and the line itself, or `None` when no
/// bullet links `target`.
pub(crate) fn unlist_text(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> Option<(String, String)> {
    let at = listing_line(workspace, text, index, target)?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `Vec::remove(at)` takes the line out, like Python's `lines.pop(at)`.
    let bullet = lines.remove(at).to_string();
    Some((lines.join("\n"), bullet))
}

/// The refusal of a move's `Unlist` whose listing changed since the move.
fn changed_listing(index: &str, target: &str) -> OkfError {
    OkfError::Refused {
        reason: format!(
            "{index}: the listing of /{target} changed since this changeset moved it; \
             discard every path of the move together (pending_changes lists them: \
             both concept paths, both folder indexes and the files whose links it \
             rewrote), or the whole changeset, then move the concept again"
        ),
    }
}

/// Python's `str.capitalize`: the first character upper-cased, the rest
/// lower-cased.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        // `to_uppercase` yields one or more chars (`ß` becomes `SS`), so
        // the pieces are chained and collected rather than pushed one by one.
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

/// A folder's heading: `ensure_index`'s `name.replace("-", " ").capitalize()`.
pub(crate) fn folder_heading(folder: &str) -> String {
    capitalize(&basename(folder).replace('-', " "))
}

/// The `index.md` `ensure_index` writes for a new folder.
pub(crate) fn new_index_text(folder: &str) -> String {
    format!(
        "# {}\n\nTODO: say what this folder holds.\n",
        folder_heading(folder)
    )
}

/// `cmd_log`'s edit: adds `bullet` under `## <date>`, newest heading first
/// and newest bullet first under it.
///
/// # Arguments
/// * `text` - `log.md`, with universal newlines.
/// * `date` - the UTC date, `2026-11-03`.
/// * `bullet` - the whole bullet line.
///
/// # Returns
/// The new text, or `None` when `bullet` is already under that heading
/// (`okf.py`'s "already logged").
///
/// # Examples
/// ```
/// use okf_mcp::okf::ops::log_text;
/// let text = log_text("# Log\n", "2026-11-03", "* **Update:** x").unwrap();
/// assert_eq!(text, "# Log\n\n## 2026-11-03\n\n* **Update:** x\n");
/// assert_eq!(log_text(&text, "2026-11-03", "* **Update:** x"), None);
/// ```
pub fn log_text(text: &str, date: &str, bullet: &str) -> Option<String> {
    let heading = format!("## {date}");
    // `lines` borrows from `text`, `heading` and `bullet`; Rust checks they
    // all outlive it, which is why `heading` is declared first.
    let mut lines: Vec<&str> = text.split('\n').collect();
    if let Some(at) = lines.iter().position(|line| *line == heading) {
        let blank = lines[at + 1..]
            .iter()
            .take_while(|line| py_strip(line).is_empty())
            .count();
        let first = at + 1 + blank;
        let section_end = first
            + lines[first..]
                .iter()
                .take_while(|line| !line.starts_with("## "))
                .count();
        if lines[first..section_end].contains(&bullet) {
            return None;
        }
        if lines.get(first).is_some_and(|line| line.starts_with("* ")) {
            lines.insert(first, bullet);
        } else {
            // `splice` is Python's slice assignment `lines[a:b] = [...]`;
            // dropping the iterator it returns applies the edit.
            lines.splice(at + 1..first, ["", bullet, ""]);
        }
    } else if let Some(first) = lines.iter().position(|line| line.starts_with("## ")) {
        lines.splice(first..first, [heading.as_str(), "", bullet, ""]);
    } else {
        let title = lines.iter().position(|line| line.starts_with("# "));
        while lines.last() == Some(&"") {
            lines.pop();
        }
        let insert_at = title.map_or(lines.len(), |t| t + 1);
        let mut block = vec!["", heading.as_str(), "", bullet];
        if insert_at < lines.len() {
            block.push("");
        }
        lines.splice(insert_at..insert_at, block);
        lines.push("");
    }
    Some(lines.join("\n"))
}

/// Is this line an index bullet (`okf.py`'s `_index_bullet`)?
fn is_bullet(line: &str) -> bool {
    line.starts_with("* ") || line.starts_with("- ")
}

/// Drops trailing empty lines (`while lines and lines[-1] == "": lines.pop()`).
fn trim_trailing_blank(lines: &mut Vec<Cow<'_, str>>) {
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
}

/// The first bullet of a heading's span at `start`, with the blank lines
/// `okf.py` puts around it.
fn insert_first_bullet<'a>(lines: &mut Vec<Cow<'a, str>>, start: usize, bullet: &'a str) {
    let text_follows = lines
        .get(start)
        .is_some_and(|line| !py_strip(line).is_empty());
    let mut block: Vec<Cow<'a, str>> = vec!["".into(), bullet.into()];
    if text_follows {
        block.push("".into());
    }
    lines.splice(start..start, block);
}

/// `insert_index_bullet`'s edit: adds `bullet` after the last bullet of
/// its `section` heading, or, with no section, at the end of `# Concepts`
/// (or after the last bullet outside `# Sections`), creating `# Concepts`
/// ahead of `# Sections` when there is nowhere else to put it.
///
/// # Arguments
/// * `text` - the `index.md`, with universal newlines.
/// * `bullet` - the whole bullet line.
/// * `section` - the heading to list under (matched case-insensitively,
///   leading `#`s ignored); `None` or empty for the default place.
/// * `create_section` - append the heading when it is missing.
///
/// # Returns
/// The new text, ending in a newline; `None` when `section` is missing and
/// `create_section` is false (`okf.py`'s "no heading 'X'").
pub fn insert_index_bullet_text(
    text: &str,
    bullet: &str,
    section: Option<&str>,
    create_section: bool,
) -> Option<String> {
    let mut lines: Vec<Cow<'_, str>> = text.split('\n').map(Cow::Borrowed).collect();
    let total = lines.len();
    let headings: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| INDEX_HEADING.is_match(line))
        .map(|(i, line)| (i, py_strip(line.trim_start_matches('#')).to_lowercase()))
        .collect();
    // The lines after heading `i`, up to the next heading (`okf.py`'s `span`).
    let span = |i: usize| {
        let stop = headings
            .iter()
            .map(|(j, _)| *j)
            .find(|j| *j > i)
            .unwrap_or(total);
        (i + 1)..stop
    };
    let named = |want: &str| -> Vec<usize> {
        headings
            .iter()
            .filter(|(_, name)| name == want)
            .map(|(i, _)| *i)
            .collect()
    };
    if let Some(section) = section.filter(|s| !s.is_empty()) {
        let want = py_strip(py_strip(section).trim_start_matches('#'));
        match named(&want.to_lowercase()).first() {
            None if !create_section => return None,
            None => {
                trim_trailing_blank(&mut lines);
                // `Vec::extend` has two impls (owned and `Copy` references),
                // so the items are spelled as `Cow`s rather than `.into()`.
                lines.extend([
                    Cow::Borrowed(""),
                    Cow::Owned(format!("# {want}")),
                    Cow::Borrowed(""),
                    Cow::Borrowed(bullet),
                    Cow::Borrowed(""),
                ]);
            }
            Some(&hit) => {
                let range = span(hit);
                let start = range.start;
                // The last bullet: searched from the end (`rev().find`).
                let last = range.rev().find(|&i| is_bullet(&lines[i]));
                match last {
                    Some(last) => lines.insert(last + 1, bullet.into()),
                    None => insert_first_bullet(&mut lines, start, bullet),
                }
            }
        }
    } else {
        // Concept bullets never go under `# Sections`, which lists sub-folders.
        let concepts = named("concepts");
        let sections = named("sections");
        let last = match concepts.first() {
            Some(&heading) => span(heading).rev().find(|&i| is_bullet(&lines[i])),
            None => {
                let mut barred = vec![false; total];
                for range in sections.iter().map(|&s| span(s)) {
                    barred[range].iter_mut().for_each(|b| *b = true);
                }
                (0..total)
                    .rev()
                    .find(|&i| !barred[i] && is_bullet(&lines[i]))
            }
        };
        if let Some(last) = last {
            lines.insert(last + 1, bullet.into());
        } else if let Some(&heading) = concepts.first() {
            insert_first_bullet(&mut lines, heading + 1, bullet);
        } else if let Some(&heading) = sections.first() {
            let mut at = heading;
            while at > 0 && lines[at - 1].is_empty() {
                at -= 1;
            }
            lines.splice(
                at..at,
                [
                    Cow::Borrowed(""),
                    Cow::Borrowed("# Concepts"),
                    Cow::Borrowed(""),
                    Cow::Borrowed(bullet),
                ],
            );
        } else {
            trim_trailing_blank(&mut lines);
            lines.extend([
                Cow::Borrowed(""),
                Cow::Borrowed("# Concepts"),
                Cow::Borrowed(""),
                Cow::Borrowed(bullet),
                Cow::Borrowed(""),
            ]);
        }
    }
    if lines.last().is_none_or(|line| !line.is_empty()) {
        lines.push("".into());
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::{ensure_index, log, LogKind, OpContext, OpOutcome};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    const FIXTURE_LOG: &str = "# Log\n\n## 2026-10-26\n\n* **Creation:** Added fixtures.\n";

    fn on(month: u32, day: u32) -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, month, day, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn listing(target: &str, bullet: &str, section: Option<&str>) -> ReplayOp {
        ReplayOp::ListBullet {
            target: target.to_string(),
            bullet: bullet.to_string(),
            section: section.map(str::to_string),
            create_section: false,
        }
    }

    #[test]
    fn log_text_adds_a_newer_heading_above_older_ones() {
        let text = log_text(FIXTURE_LOG, "2026-10-27", "* **Update:** Ran.").unwrap();

        assert_eq!(
            text,
            concat!(
                "# Log\n\n## 2026-10-27\n\n* **Update:** Ran.\n\n",
                "## 2026-10-26\n\n* **Creation:** Added fixtures.\n",
            )
        );
    }

    #[test]
    fn log_text_puts_the_newest_bullet_first_and_skips_a_duplicate() {
        let once = log_text(FIXTURE_LOG, "2026-10-26", "* **Change:** More.").unwrap();

        let again = log_text(&once, "2026-10-26", "* **Creation:** Added fixtures.");

        assert_eq!(
            once,
            "# Log\n\n## 2026-10-26\n\n* **Change:** More.\n* **Creation:** Added fixtures.\n"
        );
        assert_eq!(again, None);
    }

    #[test]
    fn log_text_fills_an_empty_heading_and_logs_without_dates() {
        let date = "2026-10-27";

        let empty_heading = log_text("# Log\n\n## 2026-10-27\n", date, "* b").unwrap();
        let title_only = log_text("# Log\n", date, "* b").unwrap();
        let title_and_intro = log_text("# Log\n\nIntro.\n", date, "* b").unwrap();
        let no_title = log_text("Intro, no title\n", date, "* b").unwrap();
        let empty = log_text("", date, "* b").unwrap();

        // Each expected value is what okf.py's cmd_log writes.
        assert_eq!(empty_heading, "# Log\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(title_only, "# Log\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(
            title_and_intro,
            "# Log\n\n## 2026-10-27\n\n* b\n\n\nIntro.\n"
        );
        assert_eq!(no_title, "Intro, no title\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(empty, "\n## 2026-10-27\n\n* b\n");
    }

    #[test]
    fn insert_under_a_named_section() {
        let blank_after =
            insert_index_bullet_text("# A\n\n# Extras\n\nText.\n", "* b", Some("Extras"), false);
        let text_after =
            insert_index_bullet_text("# A\n\n# Extras\nText.\n", "* b", Some("## extras "), false);
        let after_bullets = insert_index_bullet_text(
            "# A\n\n# Extras\n\n* old\n- older\n\n# B\n",
            "* b",
            Some("Extras"),
            false,
        );

        assert_eq!(blank_after.unwrap(), "# A\n\n# Extras\n\n* b\n\nText.\n");
        assert_eq!(text_after.unwrap(), "# A\n\n# Extras\n\n* b\n\nText.\n");
        assert_eq!(
            after_bullets.unwrap(),
            "# A\n\n# Extras\n\n* old\n- older\n* b\n\n# B\n"
        );
    }

    #[test]
    fn insert_without_a_section_avoids_the_sections_list() {
        let sections = "\n# Sections\n\n* [S](s/) - s\n";
        let under_concepts = format!("# A\n\n# Concepts\n\nText.\n{sections}");
        let loose_bullets = format!("* [x](x.md) - x\n{sections}");
        let only_sections = format!("# A\n\nIntro.\n\n{sections}");

        let results = [
            insert_index_bullet_text(&under_concepts, "* b", None, false),
            insert_index_bullet_text(&loose_bullets, "* b", None, false),
            insert_index_bullet_text(&only_sections, "* b", None, false),
            insert_index_bullet_text("# A", "* b", None, false),
            insert_index_bullet_text("", "* b", Some(""), false),
        ];

        assert_eq!(
            results.map(Option::unwrap),
            [
                format!("# A\n\n# Concepts\n\n* b\n\nText.\n{sections}"),
                format!("* [x](x.md) - x\n* b\n{sections}"),
                format!("# A\n\nIntro.\n\n# Concepts\n\n* b\n\n{sections}"),
                "# A\n\n# Concepts\n\n* b\n".to_string(),
                "\n# Concepts\n\n* b\n".to_string(),
            ]
        );
    }

    #[test]
    fn a_missing_section_is_refused_or_created() {
        let refused = insert_index_bullet_text("# A\n", "* b", Some("Nope"), false);
        let created = insert_index_bullet_text(
            "# Deep\n\nTODO: say what this folder holds.\n",
            "* [Er](er/) - TODO: describe this folder",
            Some("Sections"),
            true,
        );

        assert_eq!(refused, None);
        assert_eq!(
            created.unwrap(),
            concat!(
                "# Deep\n\nTODO: say what this folder holds.\n\n# Sections\n\n",
                "* [Er](er/) - TODO: describe this folder\n",
            )
        );
    }

    #[test]
    fn create_index_creates_only_a_missing_file() {
        let create = [ReplayOp::CreateIndex {
            folder: "ops/new-stuff".to_string(),
        }];

        let missing = replay("ops/new-stuff/index.md", None, &create).unwrap();
        let present = replay("ops/new-stuff/index.md", Some("# Mine\n"), &create).unwrap();

        assert_eq!(
            missing,
            "# New stuff\n\nTODO: say what this folder holds.\n"
        );
        assert_eq!(present, "# Mine\n");
    }

    #[test]
    fn a_listing_is_added_once_and_a_folder_link_needs_no_slash() {
        let ops = [listing("ops/a.md", "* [A](a.md) - a", None)];
        let folder = [listing(
            "ops/index.md",
            "* [Ops](ops/) - o",
            Some("Sections"),
        )];

        let once = replay("ops/index.md", Some("# Ops\n"), &ops).unwrap();
        let twice = replay("ops/index.md", Some(&once), &ops).unwrap();
        let listed = replay("index.md", Some("# Root\n\n* [Ops](ops) - o\n"), &folder).unwrap();

        assert_eq!(once, "# Ops\n\n# Concepts\n\n* [A](a.md) - a\n");
        assert_eq!(twice, once);
        assert_eq!(listed, "# Root\n\n* [Ops](ops) - o\n");
    }

    #[test]
    fn replay_normalizes_newlines_only_when_it_changes_the_text() {
        let crlf_log = "# Log\r\n\r\n## 2026-10-26\r\n\r\n* **Update:** old\r\n";
        let crlf_index = "# Ops\r\n\r\n* [A](a.md) - a\r\n";
        let log_op = [ReplayOp::Log {
            date: "2026-10-27".to_string(),
            bullet: "* **Update:** b".to_string(),
        }];
        let listed = [listing("ops/a.md", "* [A](a.md) - a", None)];

        let changed = replay("log.md", Some(crlf_log), &log_op).unwrap();
        let unchanged = replay("ops/index.md", Some(crlf_index), &listed).unwrap();

        assert_eq!(
            changed,
            "# Log\n\n## 2026-10-27\n\n* **Update:** b\n\n## 2026-10-26\n\n* **Update:** old\n"
        );
        assert_eq!(unchanged, crlf_index);
    }

    #[test]
    fn replay_refuses_a_missing_file_or_heading() {
        let log_op = [ReplayOp::Log {
            date: "2026-10-27".to_string(),
            bullet: "* b".to_string(),
        }];
        let in_section = [listing("people/x.md", "* [X](x.md) - x", Some("Nowhere"))];

        let no_log = replay("log.md", None, &log_op).unwrap_err();
        let no_heading = replay("people/index.md", Some("# People\n"), &in_section).unwrap_err();

        assert!(matches!(no_log, OkfError::NotFound { ref path } if path == "log.md"));
        assert!(matches!(
            no_heading,
            OkfError::Refused { ref reason } if reason == "no heading 'Nowhere' in people/index.md"
        ));
    }

    #[test]
    fn replay_ops_serialize_with_an_op_tag() {
        let op = listing("a/b.md", "* [B](b.md) - b", None);

        let value = serde_json::to_value(&op).unwrap();
        let back: ReplayOp = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(
            value,
            json!({
                "op": "list_bullet",
                "target": "a/b.md",
                "bullet": "* [B](b.md) - b",
                "section": null,
                "create_section": false,
            })
        );
        assert_eq!(back, op);
    }

    #[test]
    fn ensure_index_ops_rebuild_every_file_they_touched() {
        let root = "# Root\n\n* [Infra](infra/) - systems\n";
        let workspace = MemWorkspace::from_files([("index.md", root)]);
        let mut outcome = OpOutcome::default();

        ensure_index(&workspace, "ops/deep/er", &mut outcome).unwrap();

        assert_eq!(
            outcome.touched,
            [
                "ops/deep/er/index.md",
                "ops/deep/index.md",
                "ops/index.md",
                "index.md"
            ]
        );
        for path in &outcome.touched {
            let original = (path == "index.md").then_some(root);
            let replayed = replay(path, original, &outcome.replay[path]).unwrap();
            assert_eq!(replayed, workspace.read_to_string(path).unwrap(), "{path}");
        }
    }

    #[hegel::test(derandomize = true)]
    fn log_replay_redoes_the_ops_and_is_idempotent(tc: hegel::TestCase) {
        use hegel::generators;
        let bases = vec![
            FIXTURE_LOG,
            "# Log\n",
            "",
            "Intro, no title\n",
            "# Log\n\n## 2026-10-27\n",
            "# Log\r\n\r\n## 2026-10-27\r\n\r\n* **Update:** a\r\n",
        ];
        let words = vec!["a", "b", "c  d", "e\nf"];
        let base: &str = tc.draw(generators::sampled_from(&bases));
        let workspace = MemWorkspace::from_files([("log.md", base)]);
        let count = tc.draw(generators::integers::<usize>().max_value(6));
        let mut ops = Vec::new();
        for _ in 0..count {
            let day = tc.draw(generators::integers::<u32>().min_value(26).max_value(28));
            let word: &str = tc.draw(generators::sampled_from(&words));
            let outcome = log(&workspace, &on(10, day), LogKind::Update, word).unwrap();
            ops.extend(outcome.replay.get("log.md").cloned().unwrap_or_default());
        }

        let once = replay("log.md", Some(base), &ops).unwrap();
        let twice = replay("log.md", Some(&once), &ops).unwrap();

        assert_eq!(once, workspace.read_to_string("log.md").unwrap());
        assert_eq!(twice, once);
    }

    #[hegel::test(derandomize = true)]
    fn replaying_a_branch_onto_another_keeps_every_bullet(tc: hegel::TestCase) {
        use hegel::generators;
        let left = MemWorkspace::from_files([("log.md", FIXTURE_LOG)]);
        let right = MemWorkspace::from_files([("log.md", FIXTURE_LOG)]);
        let mut bullets = vec!["* **Creation:** Added fixtures.".to_string()];
        let mut right_ops = Vec::new();
        for (workspace, side) in [(&left, "left"), (&right, "right")] {
            let count = tc.draw(generators::integers::<usize>().max_value(4));
            for n in 0..count {
                let day = tc.draw(generators::integers::<u32>().min_value(26).max_value(28));
                let text = format!("{side} {n}");
                let outcome = log(workspace, &on(10, day), LogKind::Update, &text).unwrap();
                let ops = outcome.replay["log.md"].clone();
                bullets.push(format!("* **Update:** {text}"));
                if side == "right" {
                    right_ops.extend(ops);
                }
            }
        }
        let published = left.read_to_string("log.md").unwrap();

        let merged = replay("log.md", Some(&published), &right_ops).unwrap();
        let again = replay("log.md", Some(&merged), &right_ops).unwrap();

        for bullet in &bullets {
            let copies = merged.split('\n').filter(|line| line == bullet).count();
            assert_eq!(copies, 1, "{bullet} in\n{merged}");
        }
        assert_eq!(again, merged);
    }

    const INFRA: &str =
        "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n* [NTP](ntp.md) - sync\n";

    fn unlist(target: &str, bullet: Option<&str>) -> ReplayOp {
        ReplayOp::Unlist {
            target: target.to_string(),
            bullet: bullet.map(str::to_string),
        }
    }

    fn relink(from: &str, to: &str) -> ReplayOp {
        ReplayOp::Relink {
            redirect: Redirect {
                from: from.to_string(),
                to: to.to_string(),
                anchors: None,
            },
        }
    }

    #[test]
    fn unlist_text_takes_the_first_bullet_linking_the_target() {
        let workspace = MemWorkspace::default();
        let text = concat!(
            "# Infra\n\nSee [NTP](ntp.md).\n\n",
            "* [NTP](/infra/ntp.md) - sync\n* [NTP again](ntp.md) - twice\n",
        );

        let line = listing_line(&workspace, text, "infra/index.md", "infra/ntp.md");
        let taken = unlist_text(&workspace, text, "infra/index.md", "infra/ntp.md");
        let missing = unlist_text(&workspace, INFRA, "infra/index.md", "infra/other.md");

        // The prose link on line 2 is not a bullet; the absolute bullet is first.
        assert_eq!(line, Some(4));
        assert_eq!(
            taken,
            Some((
                "# Infra\n\nSee [NTP](ntp.md).\n\n* [NTP again](ntp.md) - twice\n".to_string(),
                "* [NTP](/infra/ntp.md) - sync".to_string()
            ))
        );
        assert_eq!(missing, None);
    }

    #[test]
    fn unlist_replays_idempotently_and_refuses_a_changed_listing() {
        let moved = [unlist("infra/ntp.md", Some("* [NTP](ntp.md) - sync"))];
        let edited = INFRA.replace("- sync", "- how hosts sync");

        let once = replay("infra/index.md", Some(INFRA), &moved).unwrap();
        let twice = replay("infra/index.md", Some(&once), &moved).unwrap();
        let refused = replay("infra/index.md", Some(&edited), &moved).unwrap_err();
        let deleted = replay(
            "infra/index.md",
            Some(&edited),
            &[unlist("infra/ntp.md", None)],
        );

        assert_eq!(
            once,
            "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n"
        );
        assert_eq!(twice, once);
        assert!(
            matches!(refused, OkfError::Refused { ref reason } if reason.contains("changed since")),
            "{refused:?}"
        );
        // A delete removes the listing whatever it reads now.
        assert_eq!(deleted.unwrap(), once);
    }

    #[test]
    fn relink_replays_byte_for_byte_and_idempotently() {
        let crlf = "# Infra\r\n\r\n* [Time](time.md) - the clock\r\n";
        let ops = [relink("infra/time.md", "infra/clock.md")];

        let once = replay("infra/index.md", Some(crlf), &ops).unwrap();
        let twice = replay("infra/index.md", Some(&once), &ops).unwrap();

        assert_eq!(once, "# Infra\r\n\r\n* [Time](clock.md) - the clock\r\n");
        assert_eq!(twice, once);
    }

    #[test]
    fn a_log_entry_then_a_relink_replay_in_order() {
        let ops = [
            ReplayOp::Log {
                date: "2026-11-03".to_string(),
                bullet: "* **Update:** Added [x](/infra/x.md).".to_string(),
            },
            relink("infra/x.md", "infra/y.md"),
        ];

        let replayed = replay("log.md", Some("# Log\n"), &ops).unwrap();

        assert_eq!(
            replayed,
            "# Log\n\n## 2026-11-03\n\n* **Update:** Added [x](/infra/y.md).\n"
        );
    }

    #[test]
    fn the_new_ops_serialize_with_an_op_tag() {
        let ops = vec![unlist("a/b.md", None), relink("a/b.md", "a/c.md")];

        let value = serde_json::to_value(&ops).unwrap();
        let back: Vec<ReplayOp> = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(
            value,
            json!([
                {"op": "unlist", "target": "a/b.md", "bullet": null},
                {"op": "relink", "redirect": {"from": "a/b.md", "to": "a/c.md", "anchors": null}},
            ])
        );
        assert_eq!(back, ops);
    }
}
