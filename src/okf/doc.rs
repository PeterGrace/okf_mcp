//! A markdown document split into frontmatter lines and everything else, with
//! textual edits that change only the key being written.
//!
//! Port of `okf.py`'s `Doc` and `append_seq_item`. Every byte outside the
//! edited key's lines is preserved, including comments, blank lines, key
//! order and each line's own newline style. A deliberate improvement over
//! `okf.py`: it rejoins every frontmatter line with the first line's newline,
//! so a file with mixed `\n` and `\r\n` endings would change on every edit
//! there; here each line keeps its own ending, and only lines an edit inserts
//! use the first line's.
//!
//! Divergences from `okf.py`:
//! - only `\n` and `\r\n` end lines. Python's `splitlines` also splits on a
//!   lone `\r`, `\x0b`, `\x0c`, `\x1c`-`\x1e`, `\x85`, U+2028 and U+2029,
//!   which would corrupt a document containing them;
//! - a line ending in `\r\r\n` keeps its extra `\r` as part of its text, so
//!   a key line `k:\r` is not recognized as a key (okf.py strips both `\r`s
//!   and sees `k`). Real catalog files don't contain such lines.

use std::ops::Range;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::error::OkfError;
use crate::okf::pystr::{py_lstrip, py_rstrip, py_strip};
use crate::okf::yaml::{
    indent_of, is_seq_start, parse_value, strip_comment, NumberedLine, Value, YamlError,
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
    Regex::new(r"^([A-Za-z_][\p{L}\p{N}_.\-]*)[ \t]*:(?:[ \t]|$)").expect("TOP_KEY regex is valid")
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

/// Where a block-list key's items sit, as returned by [`Doc::seq_items`].
///
/// A plain read-only result, so its fields are public.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqItems {
    /// The number of spaces before each item's dash.
    pub indent: usize,
    /// Each item's lines as an index range into [`Doc::frontmatter`].
    pub spans: Vec<Range<usize>>,
}

/// A parsed markdown file: opening delimiter, frontmatter lines, and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    path: String,
    head: Vec<String>,
    /// Frontmatter line texts, without their endings.
    frontmatter: Vec<String>,
    /// `endings[i]` is the exact ending (`""`, `"\n"` or `"\r\n"`) of
    /// `frontmatter[i]`. Every edit goes through `splice_lines` to keep the
    /// two vectors the same length.
    endings: Vec<&'static str>,
    tail: Vec<String>,
    /// The ending given to lines an edit inserts.
    newline: &'static str,
    has_frontmatter: bool,
}

/// Splits text into lines that keep their `\n` (Python's `splitlines(keepends=True)`
/// restricted to `\n` and `\r\n`).
fn lines_with_endings(text: &str) -> Vec<String> {
    // `split_inclusive` is like `splitlines(keepends=True)`: each piece keeps its `\n`.
    text.split_inclusive('\n').map(str::to_string).collect()
}

/// Python's `line.rstrip("\r\n")`: removes every trailing `\r` and `\n`.
fn strip_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

/// The exact ending of `line`: `"\r\n"`, `"\n"` or `""` (one ending only).
fn line_ending(line: &str) -> &'static str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// The top-level key blocks of frontmatter `lines`, in order.
fn blocks_of(lines: &[String]) -> Vec<Block> {
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
            if py_strip(line).is_empty() {
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
        let mut lines = lines_with_endings(text);
        let newline = if lines.first().is_some_and(|l| l.ends_with("\r\n")) {
            "\r\n"
        } else {
            "\n"
        };
        let mut doc = Self {
            path: path.to_string(),
            head: Vec::new(),
            frontmatter: Vec::new(),
            endings: Vec::new(),
            tail: Vec::new(),
            newline,
            has_frontmatter: false,
        };
        let opens = lines.first().map(|l| strip_line_ending(l)) == Some("---");
        let closing = opens
            .then(|| {
                lines
                    .iter()
                    .skip(1)
                    .position(|l| matches!(py_rstrip(strip_line_ending(l)), "---" | "..."))
            })
            .flatten();
        let Some(offset) = closing else {
            doc.tail = lines;
            return doc;
        };
        // `split_off(n)` moves `lines[n..]` out into a new Vec (like `rest = lines[n:]; del lines[n:]`).
        doc.tail = lines.split_off(offset + 1);
        let frontmatter = lines.split_off(1);
        doc.head = lines;
        doc.has_frontmatter = true;
        for mut line in frontmatter {
            let ending = line_ending(&line);
            line.truncate(line.len() - ending.len());
            doc.frontmatter.push(line);
            doc.endings.push(ending);
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

    /// Replaces `frontmatter[range]` with `new_lines` (Python's slice
    /// assignment `fm[a:b] = new_lines`; an empty `new_lines` is `del fm[a:b]`).
    /// Inserted lines get [`Doc`]'s `newline`; the endings vector stays in step.
    fn splice_lines(&mut self, range: Range<usize>, new_lines: Vec<String>) {
        let endings = std::iter::repeat_n(self.newline, new_lines.len());
        // `splice` returns an iterator over the removed items; dropping it applies the edit.
        self.endings.splice(range.clone(), endings);
        self.frontmatter.splice(range, new_lines);
    }

    /// The top-level key blocks, in file order.
    ///
    /// # Returns
    /// One [`Block`] per `key:` line, with its continuation lines.
    pub fn blocks(&self) -> Vec<Block> {
        blocks_of(&self.frontmatter)
    }

    /// The block for `key`, if present.
    ///
    /// # Arguments
    /// * `key` - the top-level key.
    ///
    /// # Returns
    /// The first block with that key, or `None`.
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

    /// Reassembles the file text, each line with its own ending.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.extend(self.head.iter().map(String::as_str));
        for (line, ending) in self.frontmatter.iter().zip(&self.endings) {
            out.push_str(line);
            out.push_str(ending);
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
    /// * `new_lines` - the replacement lines; `new_lines[0]` must be the
    ///   `key:` line, or the key will not be found again.
    pub fn set_block(&mut self, key: &str, new_lines: Vec<String>) {
        let blocks = self.blocks();
        if let Some(b) = blocks.iter().find(|b| b.key == key) {
            self.splice_lines(b.start..b.end, new_lines);
            return;
        }
        let mut position = self.frontmatter.len();
        // Trailing blank lines at the end of the frontmatter stay at the end.
        while position > 0 && py_strip(&self.frontmatter[position - 1]).is_empty() {
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
        self.splice_lines(position..position, new_lines);
    }

    /// Removes `key`'s lines, if present.
    ///
    /// # Arguments
    /// * `key` - the top-level key; a missing key is a no-op.
    pub fn delete_block(&mut self, key: &str) {
        if let Some(b) = self.block(key) {
            self.splice_lines(b.start..b.end, Vec::new());
        }
    }

    /// Where a block-list key's items sit.
    ///
    /// Comment and blank lines are skipped, so a trailing comment after the
    /// last item is not part of any span (a new item goes before it, as in
    /// `okf.py`).
    ///
    /// # Arguments
    /// * `key` - the top-level key.
    ///
    /// # Returns
    /// The item indent (2 when there are no items) and each item's span, or
    /// `None` when the key is absent, has an inline value, or its
    /// continuation isn't a plain block list.
    pub fn seq_items(&self, key: &str) -> Option<SeqItems> {
        let b = self.block(key)?;
        let inline = self.frontmatter[b.start]
            .split_once(':')
            .map_or("", |(_, v)| v);
        if !strip_comment(inline).is_empty() {
            return None;
        }
        let mut spans: Vec<Range<usize>> = Vec::new();
        let mut item_indent: Option<usize> = None;
        for i in b.start + 1..b.end {
            let line = &self.frontmatter[i];
            let stripped = py_strip(line);
            if stripped.is_empty() || stripped.starts_with('#') {
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
                spans.push(i..i + 1);
            } else if indent > expected {
                // A continuation of the last item; `?` returns None when there is none.
                spans.last_mut()?.end = i + 1;
            } else {
                return None;
            }
        }
        Some(SeqItems {
            indent: item_indent.unwrap_or(2),
            spans,
        })
    }

    /// Appends an item to a list-valued key, creating or normalizing the key.
    ///
    /// Port of `okf.py`'s `append_seq_item`. A missing or null key becomes a
    /// new block list; a block list gets the item after its last item; a bare
    /// mapping becomes the first item of a new list; a flow list is
    /// rewritten as a block list (each old item via `render_item`). The key's
    /// current value comes from parsing the frontmatter first.
    ///
    /// Shared quirk with `okf.py`: in the bare block mapping case, an indented
    /// comment shallower than the keys sets the indent used for the first
    /// item, so the comment text becomes that item's text. Callers should
    /// write the result with [`Doc::render_checked`], which catches a broken
    /// outcome.
    ///
    /// # Arguments
    /// * `key` - the list key (`verified`, `sources`, `open_questions`).
    /// * `item_lines` - returns the new item's lines for a given dash indent.
    /// * `render_item` - renders an existing item when a flow list is rewritten
    ///   (normally [`crate::okf::yaml::block_item`]).
    ///
    /// # Errors
    /// [`OkfError::FrontmatterParse`] when the frontmatter does not parse, and
    /// [`OkfError::Shape`] when the key is a scalar.
    pub fn append_seq_item(
        &mut self,
        key: &str,
        item_lines: impl FnOnce(usize) -> Vec<String>,
        render_item: impl Fn(&Value, usize) -> Vec<String>,
    ) -> Result<(), OkfError> {
        let parsed = self.parse_checked()?;
        let current = parsed.get(key).filter(|v| !matches!(v, Value::Null));
        let (Some(b), Some(value)) = (self.block(key), current) else {
            let mut lines = vec![format!("{key}:")];
            lines.extend(item_lines(2));
            self.set_block(key, lines);
            return Ok(());
        };
        if let (Some(items), Value::Seq(_)) = (self.seq_items(key), value) {
            let end = items.spans.last().map_or(b.end, |s| s.end);
            self.splice_lines(end..end, item_lines(items.indent));
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
                        .filter(|t| !py_strip(t).is_empty())
                        .map(|t| indent_of(t))
                        .min()
                        .unwrap_or(2);
                    let mut first = true;
                    for line in kids {
                        if py_strip(line).is_empty() {
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
                self.splice_lines(b.start..b.end, lines);
                Ok(())
            }
            Value::Seq(items) => {
                let mut lines = vec![format!("{key}:")];
                for item in items {
                    lines.extend(render_item(item, 2));
                }
                lines.extend(item_lines(2));
                self.splice_lines(b.start..b.end, lines);
                Ok(())
            }
            Value::Null | Value::Str(_) => Err(OkfError::Shape {
                path: self.path.clone(),
                key: key.to_string(),
            }),
        }
    }
}

/// Parses frontmatter lines (no delimiters) the way `Doc.parse` does.
fn parse_frontmatter_lines(lines: &[String]) -> Result<IndexMap<String, Value>, YamlError> {
    let mut data = IndexMap::new();
    let mut covered = vec![false; lines.len()];
    for b in blocks_of(lines) {
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
        if !covered[i] && !py_strip(line).is_empty() && !py_lstrip(line).starts_with('#') {
            return Err(YamlError {
                message: format!("line is not part of any key: {:?}", py_strip(line)),
                line: Some(i + 2),
            });
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::yaml::block_item;

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
        assert_eq!(
            d.seq_items("q"),
            Some(SeqItems {
                indent: 2,
                spans: vec![1..2, 2..4]
            })
        );
        assert_eq!(doc("---\nq: [a]\n---\n").seq_items("q"), None);
        assert_eq!(doc("---\nq:\n  a: b\n---\n").seq_items("q"), None);
    }

    #[test]
    fn append_creates_missing_key() {
        let mut d = doc("---\ntype: Note\n---\n");
        d.append_seq_item("open_questions", quoted_item("Why?"), block_item)
            .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["type: Note", "open_questions:", "  - \"Why?\""]
        );
    }

    #[test]
    fn append_extends_block_list() {
        let mut d = doc(SAMPLE);
        let entry = Value::Map(
            [("id".to_string(), Value::Str("two".into()))]
                .into_iter()
                .collect(),
        );
        d.append_seq_item("sources", |i| block_item(&entry, i), block_item)
            .unwrap();
        assert_eq!(d.frontmatter()[7], "  - id: two");
        assert!(d.render_checked().is_ok());
    }

    #[test]
    fn append_converts_flow_list() {
        let mut d = doc("---\nverified: [{ by: a, at: b }]\n---\n");
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- {{ by: c, at: d }}", " ".repeat(i))],
            block_item,
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
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["verified:", "  - { by: a, at: b }", "  - new"]
        );

        let mut d = doc("---\nverified:\n  by: a\n  at: b\n---\n");
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            block_item,
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
        let err = d
            .append_seq_item("sources", |_| vec![], block_item)
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

    #[test]
    fn key_characters_follow_python_word_class() {
        // U+00B2 is a Python word character; U+0301 (a combining mark) is not.
        let keys: Vec<_> = doc("---\na\u{b2}: 1\n---\n")
            .blocks()
            .into_iter()
            .map(|b| b.key)
            .collect();
        assert_eq!(keys, ["a\u{b2}"]);
        assert!(doc("---\na\u{301}: 1\n---\n").blocks().is_empty());
    }

    #[test]
    fn python_whitespace_counts_as_blank() {
        // U+001F is whitespace to Python's str.strip but not to Rust's trim.
        assert!(doc("---\ntype: x\n\x1f\n---\n").parse().is_ok());
        // A closing delimiter padded with U+001F still closes the frontmatter.
        assert!(doc("---\ntype: x\n---\x1f\nbody\n").has_frontmatter());
        // A U+001F-only line before a new key stays trailing blank space.
        let mut d = doc("---\nx_custom: 1\n\x1f\n---\n");
        d.set_block("other", vec!["other: 2".into()]);
        assert_eq!(d.frontmatter(), ["x_custom: 1", "other: 2", "\x1f"]);
    }

    #[test]
    fn per_line_endings_survive_render_and_edits() {
        let text = "---\n,\r\n---\n[a\n---\n";
        assert_eq!(doc(text).render(), text);

        let mut d = doc("---\ntype: x\r\nother: 1\n---\n");
        d.set_block("tags", vec!["tags: [a]".into()]);
        assert_eq!(d.render(), "---\ntype: x\r\ntags: [a]\nother: 1\n---\n");

        let odd = "---\na: 1\r\r\n---\n";
        let d = doc(odd);
        assert_eq!(d.frontmatter(), ["a: 1\r"]);
        assert_eq!(d.render(), odd);
    }

    #[test]
    fn inserted_lines_use_the_first_lines_ending() {
        let mut d = doc("---\r\ntype: x\r\n---\r\n");
        d.set_block("title", vec!["title: T".into()]);
        assert_eq!(d.render(), "---\r\ntype: x\r\ntitle: T\r\n---\r\n");
    }

    #[test]
    fn set_block_inserts_before_later_ranked_keys() {
        let mut d = doc("---\nsources:\n  - a\n---\n");
        d.set_block("type", vec!["type: Note".into()]);
        assert_eq!(d.frontmatter(), ["type: Note", "sources:", "  - a"]);
    }

    #[test]
    fn seq_items_skips_comments() {
        let d = doc("---\nq:\n  # lead\n  - a\n  # mid\n  - b\n    cont\n  # trailing\n---\n");
        assert_eq!(
            d.seq_items("q"),
            Some(SeqItems {
                indent: 2,
                spans: vec![2..3, 4..6]
            })
        );
        let only_comments = doc("---\nq:\n  # nothing\n---\n");
        assert_eq!(
            only_comments.seq_items("q"),
            Some(SeqItems {
                indent: 2,
                spans: vec![]
            })
        );
    }

    #[test]
    fn append_goes_before_a_trailing_comment() {
        let mut d = doc("---\nq:\n  - a\n  # trailing\n---\n");
        d.append_seq_item("q", quoted_item("b"), block_item)
            .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["q:", "  - a", "  - \"b\"", "  # trailing"]
        );
    }

    #[test]
    fn append_twice_to_missing_key_keeps_both() {
        let mut d = doc("---\ntype: Note\n---\n");
        d.append_seq_item("open_questions", quoted_item("One?"), block_item)
            .unwrap();
        d.append_seq_item("open_questions", quoted_item("Two?"), block_item)
            .unwrap();
        assert_eq!(
            d.frontmatter(),
            [
                "type: Note",
                "open_questions:",
                "  - \"One?\"",
                "  - \"Two?\""
            ]
        );
    }

    #[test]
    fn append_on_crlf_document_uses_crlf() {
        let mut d = doc("---\r\ntype: Note\r\n---\r\n");
        d.append_seq_item("open_questions", quoted_item("Why?"), block_item)
            .unwrap();
        assert_eq!(
            d.render(),
            "---\r\ntype: Note\r\nopen_questions:\r\n  - \"Why?\"\r\n---\r\n"
        );
    }

    #[test]
    fn append_converts_bare_mapping_with_wide_indent() {
        let mut d = doc("---\nverified:\n    by: a\n    at: b\n---\n");
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["verified:", "    - by: a", "      at: b", "    - new"]
        );
        assert!(d.render_checked().is_ok());
    }

    #[hegel::test]
    fn mixed_endings_round_trip_and_edits_touch_one_line(tc: hegel::TestCase) {
        use hegel::generators;
        let pieces = vec!["type: x", "# c", "", "  - a", "k: [a, b]"];
        let endings = vec!["\n", "\r\n"];
        let count = tc.draw(generators::integers::<usize>().max_value(10));
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..count {
            let piece: &str = tc.draw(generators::sampled_from(&pieces));
            let ending: &str = tc.draw(generators::sampled_from(&endings));
            lines.push(format!("{piece}{ending}"));
        }
        let text = format!("---\n{}---\nbody\n", lines.concat());
        let mut d = doc(&text);
        assert_eq!(d.render(), text);

        d.set_block("zz_new", vec!["zz_new: 1".into()]);
        // The new key lands before any trailing blank lines, with the first line's ending.
        let trailing_blank = lines
            .iter()
            .rev()
            .take_while(|l| l.trim_end_matches(['\r', '\n']).is_empty())
            .count();
        lines.insert(lines.len() - trailing_blank, "zz_new: 1\n".to_string());
        assert_eq!(d.render(), format!("---\n{}---\nbody\n", lines.concat()));
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
