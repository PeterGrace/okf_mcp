//! A concept's outline and its sections, for `get_concept` (issue #186): a
//! body too large to return whole is described by its headings, and one
//! section (with its subsections) can be read on its own.
//!
//! A section runs from its heading line to the next heading of the same or
//! a higher level (the end of the body for the last), less the blank lines
//! at its end, as `sprawl` and `split_concept` bound it. Headings and slugs
//! come from [`headings`], so fenced code is skipped and a repeated heading
//! gets `split_concept`'s numbered slug (`notes`, `notes-1`).

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::OkfError;
use crate::okf::markdown::{body_split, headings, Heading};
use crate::okf::ops::split::find_section;
use crate::okf::pystr::py_strip;

/// One heading of an outline, and the size of its section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OutlineHeading {
    /// The heading text.
    pub heading: String,
    /// Its anchor slug, as a link's `#fragment` names it; `section` takes it.
    pub slug: String,
    /// 1 to 6.
    pub level: usize,
    /// 1-based file line of the heading.
    pub line: usize,
    /// Lines in the section, its heading and subsections included.
    pub lines: usize,
    /// Characters (Unicode code points) in the section, as `section`
    /// returns it.
    pub chars: usize,
}

/// A document's body split into lines, with its headings and where each
/// heading's section ends. Built in one pass over the body, so the outline
/// and a section are linear in its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline<'t> {
    /// The body's lines, without their `\n`.
    body: Vec<&'t str>,
    /// Lines before the body (the frontmatter and its delimiters).
    offset: usize,
    /// The headings, in body order.
    found: Vec<Heading>,
    /// For each heading, the body index one past its section's last line.
    ends: Vec<usize>,
    /// `starts[i]`: characters before body line `i`, each line counting its
    /// `\n`; one entry more than `body`, so any range's size is a difference.
    starts: Vec<usize>,
}

impl<'t> Outline<'t> {
    /// Splits a document's text (with `\n` endings) and finds its sections.
    ///
    /// # Arguments
    /// * `text` - the whole file, frontmatter included, as the snapshot
    ///   holds it (universal newlines).
    ///
    /// # Returns
    /// The outline; it borrows `text` (the `'t` lifetime) instead of copying
    /// the lines.
    ///
    /// # Examples
    /// ```
    /// use okf_mcp::okf::outline::Outline;
    /// let outline = Outline::of("---\ntitle: A\n---\n# A\nIntro.\n## B\nText.\n");
    /// let entries = outline.entries();
    /// assert_eq!((entries[0].slug.as_str(), entries[0].line), ("a", 4));
    /// assert_eq!(outline.section("b").unwrap().1, "## B\nText.\n");
    /// ```
    pub fn of(text: &'t str) -> Self {
        let (head, body) = body_split(text);
        let found = headings(&body);
        let starts = std::iter::once(0)
            .chain(body.iter().scan(0, |total, line| {
                *total += line.chars().count() + 1;
                Some(*total)
            }))
            .collect();
        let ends = section_ends(&body, &found);
        Self {
            offset: head.len(),
            body,
            found,
            ends,
            starts,
        }
    }

    /// Every heading, in file order, with its section's size.
    pub fn entries(&self) -> Vec<OutlineHeading> {
        (0..self.found.len()).map(|k| self.entry(k)).collect()
    }

    /// One section: its heading's entry and its text, subsections included.
    ///
    /// # Arguments
    /// * `want` - the heading's text (case-insensitive, leading `#`s
    ///   ignored) or, failing that, its anchor slug (a leading `#` allowed).
    ///
    /// # Returns
    /// The entry and the section's lines, each ending in `\n`.
    ///
    /// # Errors
    /// [`OkfError::Refused`], as `split_concept` words it: no heading
    /// matches (the message lists every heading), or several do (it lists
    /// their slugs, which tell them apart).
    pub fn section(&self, want: &str) -> Result<(OutlineHeading, String), OkfError> {
        let hit = find_section(&self.found, want)?;
        // `find_section` returns a reference into `found`; `position` turns
        // it back into its index (heading lines are unique).
        let k = self
            .found
            .iter()
            .position(|heading| heading.index == hit.index)
            .expect("find_section returns one of the headings it was given");
        let entry = self.entry(k);
        let lines = &self.body[self.found[k].index..self.ends[k]];
        let mut text = String::with_capacity(entry.chars);
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        Ok((entry, text))
    }

    /// The `k`th heading's entry.
    fn entry(&self, k: usize) -> OutlineHeading {
        let heading = &self.found[k];
        let (start, end) = (heading.index, self.ends[k]);
        OutlineHeading {
            heading: heading.text.clone(),
            slug: heading.slug.clone(),
            level: heading.level,
            line: start + self.offset + 1,
            lines: end - start,
            chars: self.starts[end] - self.starts[start],
        }
    }
}

/// Where each heading's section ends: the index of the next heading of the
/// same or a higher level (else the body's end), less trailing blank lines.
///
/// # Arguments
/// * `body` - the body's lines.
/// * `found` - its headings, from [`headings`].
///
/// # Returns
/// One end per heading, exclusive; never before the line after the heading.
fn section_ends(body: &[&str], found: &[Heading]) -> Vec<usize> {
    let mut ends = vec![body.len(); found.len()];
    // A stack of the headings whose section is still open, by position in
    // `found`. Each heading is pushed and popped once: O(headings).
    let mut open: Vec<usize> = Vec::new();
    for (k, heading) in found.iter().enumerate() {
        while let Some(&top) = open.last() {
            if found[top].level < heading.level {
                break;
            }
            ends[top] = heading.index;
            open.pop();
        }
        open.push(k);
    }
    for (end, heading) in ends.iter_mut().zip(found) {
        while *end > heading.index + 1 && py_strip(body[*end - 1]).is_empty() {
            *end -= 1;
        }
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "---\ntitle: Hub\n---\n# Hub\n\nIntro.\n\n## Notes\nFirst.\n\n\
                       ### Detail\nDeep.\n\n## Notes\nSecond.\n\n```\n# not a heading\n```\n\
                       ## Last\nEnd.\n";

    #[test]
    fn entries_give_file_lines_slugs_and_section_sizes() {
        let outline = Outline::of(DOC);

        let entries = outline.entries();

        let summary: Vec<(&str, &str, usize, usize, usize)> = entries
            .iter()
            .map(|e| {
                (
                    e.heading.as_str(),
                    e.slug.as_str(),
                    e.level,
                    e.line,
                    e.lines,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Hub", "hub", 1, 4, 18),
                ("Notes", "notes", 2, 8, 5),
                ("Detail", "detail", 3, 11, 2),
                ("Notes", "notes-1", 2, 14, 6),
                ("Last", "last", 2, 20, 2),
            ]
        );
        // Each entry's `chars` is exactly what `section` returns.
        for entry in &entries {
            let (_, text) = outline.section(&entry.slug).unwrap();
            assert_eq!(text.chars().count(), entry.chars, "{}", entry.slug);
            assert_eq!(text.lines().count(), entry.lines, "{}", entry.slug);
        }
    }

    #[test]
    fn a_section_by_slug_or_text_includes_subsections_and_drops_trailing_blanks() {
        let outline = Outline::of(DOC);

        let (entry, text) = outline.section("notes").unwrap();
        let (_, fenced) = outline.section("#notes-1").unwrap();
        let (_, by_text) = outline.section("## detail").unwrap();

        assert_eq!(entry.line, 8);
        assert_eq!(text, "## Notes\nFirst.\n\n### Detail\nDeep.\n");
        assert_eq!(fenced, "## Notes\nSecond.\n\n```\n# not a heading\n```\n");
        assert_eq!(by_text, "### Detail\nDeep.\n");
    }

    #[test]
    fn a_repeated_heading_by_text_is_ambiguous_and_an_unknown_one_lists_headings() {
        let outline = Outline::of(DOC);

        // "Notes" matches two headings by text and is neither's slug (slugs
        // compare as written), so it is refused; "notes" picks the first.
        let twice = outline.section("Notes").unwrap_err().to_string();
        let unknown = outline.section("Nowhere").unwrap_err();

        assert!(
            twice.contains("2 headings match 'Notes'") && twice.contains("notes, notes-1"),
            "{twice}"
        );
        assert_eq!(unknown.kind(), "Refused");
        let message = unknown.to_string();
        assert!(
            message.contains("no heading 'Nowhere'") && message.contains("## Last"),
            "{message}"
        );
    }

    #[test]
    fn a_heading_with_nothing_under_it_is_one_line() {
        let outline = Outline::of("# A\n\n\n# B\n");

        let entries = outline.entries();

        assert_eq!((entries[0].lines, entries[0].chars), (1, 4));
        assert_eq!(outline.section("a").unwrap().1, "# A\n");
        assert_eq!(outline.section("b").unwrap().1, "# B\n");
    }

    #[test]
    fn a_body_without_headings_has_an_empty_outline() {
        let outline = Outline::of("---\ntitle: A\n---\nJust text.\n");

        assert!(outline.entries().is_empty());
        assert_eq!(outline.section("x").unwrap_err().kind(), "Refused");
    }
}
