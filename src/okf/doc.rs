//! A markdown document split into frontmatter lines and everything else, with
//! textual edits that change only the key being written.
//!
//! Port of `okf.py`'s `Doc` and `append_seq_item`. Every byte outside the
//! edited key's lines is preserved, including comments, blank lines, key
//! order and the file's newline style.
//!
//! Divergence from `okf.py`: only `\n` and `\r\n` end lines. Python's
//! `splitlines` also splits on `\x0b`, `\x0c`, `\x1c`-`\x1e`, `\x85`,
//! U+2028 and U+2029, which would corrupt a document containing them.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::error::OkfError;
use crate::okf::pystr::{py_lstrip, py_rstrip, py_strip};
use crate::okf::yaml::{
    block_item, indent_of, is_seq_start, parse_value, strip_comment, NumberedLine, Value, YamlError,
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

/// A parsed markdown file: opening delimiter, frontmatter lines, and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    path: String,
    head: Vec<String>,
    frontmatter: Vec<String>,
    tail: Vec<String>,
    newline: &'static str,
    has_frontmatter: bool,
}

/// Splits text into lines that keep their `\n` (Python's `splitlines(keepends=True)`
/// restricted to `\n` and `\r\n`).
fn lines_with_endings(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_string).collect()
}

fn strip_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
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
        let lines = lines_with_endings(text);
        let newline = if lines.first().is_some_and(|l| l.ends_with("\r\n")) {
            "\r\n"
        } else {
            "\n"
        };
        let mut doc = Self {
            path: path.to_string(),
            head: Vec::new(),
            frontmatter: Vec::new(),
            tail: lines.clone(),
            newline,
            has_frontmatter: false,
        };
        if lines.first().map(|l| strip_line_ending(l)) != Some("---") {
            return doc;
        }
        let closing = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, l)| matches!(py_rstrip(strip_line_ending(l)), "---" | "..."));
        if let Some((j, _)) = closing {
            doc.has_frontmatter = true;
            doc.head = lines[..1].to_vec();
            doc.frontmatter = lines[1..j]
                .iter()
                .map(|l| strip_line_ending(l).to_string())
                .collect();
            doc.tail = lines[j..].to_vec();
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

    /// The top-level key blocks, in file order.
    pub fn blocks(&self) -> Vec<Block> {
        let lines = &self.frontmatter;
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

    /// The block for `key`, if present.
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

    /// Reassembles the file text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.extend(self.head.iter().map(String::as_str));
        for line in &self.frontmatter {
            out.push_str(line);
            out.push_str(self.newline);
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
    /// * `new_lines` - the replacement lines, starting with `key:`.
    pub fn set_block(&mut self, key: &str, new_lines: Vec<String>) {
        let blocks = self.blocks();
        if let Some(b) = blocks.iter().find(|b| b.key == key) {
            self.frontmatter.splice(b.start..b.end, new_lines);
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
        self.frontmatter.splice(position..position, new_lines);
    }

    /// Removes `key`'s lines, if present.
    pub fn delete_block(&mut self, key: &str) {
        if let Some(b) = self.block(key) {
            self.frontmatter.drain(b.start..b.end);
        }
    }

    /// For a block-list key: the item indent and each item's `(start, end)` span.
    ///
    /// Returns `None` when the key is absent, has an inline value, or its
    /// continuation isn't a plain block list.
    pub fn seq_items(&self, key: &str) -> Option<(usize, Vec<(usize, usize)>)> {
        let b = self.block(key)?;
        let inline = self.frontmatter[b.start]
            .split_once(':')
            .map_or("", |(_, v)| v);
        if !strip_comment(inline).is_empty() {
            return None;
        }
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut item_indent: Option<usize> = None;
        for i in b.start + 1..b.end {
            let line = &self.frontmatter[i];
            if py_strip(line).is_empty() || py_strip(line).starts_with('#') {
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
                spans.push((i, i + 1));
            } else if indent > expected {
                // A continuation of the last item; `?` returns None when there is none.
                spans.last_mut()?.1 = i + 1;
            } else {
                return None;
            }
        }
        Some((item_indent.unwrap_or(2), spans))
    }

    /// Appends an item to a list-valued key, creating or normalizing the key.
    ///
    /// Port of `okf.py`'s `append_seq_item`. A missing or null key becomes a
    /// new block list; a block list gets the item after its last item; a bare
    /// mapping becomes the first item of a new list; a flow list is
    /// rewritten as a block list (each old item via `render_item`).
    ///
    /// # Arguments
    /// * `key` - the list key (`verified`, `sources`, `open_questions`).
    /// * `item_lines` - returns the new item's lines for a given dash indent.
    /// * `current` - the key's parsed value (`None` if absent or null).
    /// * `render_item` - renders an existing item when a flow list is rewritten.
    ///
    /// # Errors
    /// [`OkfError::Shape`] when the key is a scalar.
    pub fn append_seq_item(
        &mut self,
        key: &str,
        item_lines: impl Fn(usize) -> Vec<String>,
        current: Option<&Value>,
        render_item: impl Fn(&Value, usize) -> Vec<String>,
    ) -> Result<(), OkfError> {
        let block = self.block(key);
        let (Some(b), Some(value)) = (block, current.filter(|v| **v != Value::Null)) else {
            let mut lines = vec![format!("{key}:")];
            lines.extend(item_lines(2));
            self.set_block(key, lines);
            return Ok(());
        };
        if let (Some((indent, spans)), Value::Seq(_)) = (self.seq_items(key), value) {
            let end = spans.last().map_or(b.end, |s| s.1);
            self.frontmatter.splice(end..end, item_lines(indent));
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
                self.frontmatter.splice(b.start..b.end, lines);
                Ok(())
            }
            Value::Seq(items) => {
                let mut lines = vec![format!("{key}:")];
                for item in items {
                    lines.extend(render_item(item, 2));
                }
                lines.extend(item_lines(2));
                self.frontmatter.splice(b.start..b.end, lines);
                Ok(())
            }
            Value::Null | Value::Str(_) => Err(OkfError::Shape {
                path: self.path.clone(),
                key: key.to_string(),
            }),
        }
    }
}

/// [`block_item`] as a `render_item` callback for [`Doc::append_seq_item`].
pub fn render_block_item(value: &Value, indent: usize) -> Vec<String> {
    block_item(value, indent)
}

/// Parses frontmatter lines (no delimiters) the way `Doc.parse` does.
fn parse_frontmatter_lines(lines: &[String]) -> Result<IndexMap<String, Value>, YamlError> {
    let doc = Doc {
        path: String::new(),
        head: Vec::new(),
        frontmatter: lines.to_vec(),
        tail: Vec::new(),
        newline: "\n",
        has_frontmatter: true,
    };
    let mut data = IndexMap::new();
    let mut covered = vec![false; lines.len()];
    for b in doc.blocks() {
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
        assert_eq!(d.seq_items("q"), Some((2, vec![(1, 2), (2, 4)])));
        assert_eq!(doc("---\nq: [a]\n---\n").seq_items("q"), None);
        assert_eq!(doc("---\nq:\n  a: b\n---\n").seq_items("q"), None);
    }

    #[test]
    fn append_creates_missing_key() {
        let mut d = doc("---\ntype: Note\n---\n");
        d.append_seq_item(
            "open_questions",
            quoted_item("Why?"),
            None,
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["type: Note", "open_questions:", "  - \"Why?\""]
        );
    }

    #[test]
    fn append_extends_block_list() {
        let mut d = doc(SAMPLE);
        let current = d.parse().unwrap()["sources"].clone();
        let entry = Value::Map(
            [("id".to_string(), Value::Str("two".into()))]
                .into_iter()
                .collect(),
        );
        d.append_seq_item(
            "sources",
            |i| block_item(&entry, i),
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(d.frontmatter()[7], "  - id: two");
        assert!(d.render_checked().is_ok());
    }

    #[test]
    fn append_converts_flow_list() {
        let mut d = doc("---\nverified: [{ by: a, at: b }]\n---\n");
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- {{ by: c, at: d }}", " ".repeat(i))],
            Some(&current),
            render_block_item,
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
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            Some(&current),
            render_block_item,
        )
        .unwrap();
        assert_eq!(
            d.frontmatter(),
            ["verified:", "  - { by: a, at: b }", "  - new"]
        );

        let mut d = doc("---\nverified:\n  by: a\n  at: b\n---\n");
        let current = d.parse().unwrap()["verified"].clone();
        d.append_seq_item(
            "verified",
            |i| vec![format!("{}- new", " ".repeat(i))],
            Some(&current),
            render_block_item,
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
        let current = d.parse().unwrap()["sources"].clone();
        let err = d
            .append_seq_item("sources", |_| vec![], Some(&current), render_block_item)
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
