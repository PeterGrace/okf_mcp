//! A YAML-subset parser and renderer covering what OKF frontmatter uses.
//!
//! Port of the parser in `tools/okf.py`: plain, single- and double-quoted
//! scalars, flow maps and lists, block maps and lists (nested), comments,
//! and `|`/`>` block scalars. Scalars stay strings (`"5"` is not a number).
//!
//! Deliberate divergences from `okf.py`, all in edge cases:
//! - A null key parses as the string `"null"` (Python's `None` key, which
//!   `json.dumps` also writes as `"null"`).
//! - Rendering a null item in a block list writes `null`, not Python's `None`.
//! - A `\u`/`\U` escape naming a surrogate is an error (Python accepts it).
//! - Quoted source text in error messages follows Rust `Debug` escaping, not
//!   Python `repr`, so the parity tests compare error lines and each message's
//!   prefix before the first quote or `" in "` (its kind), never whole messages.
//! - Null-key consequences: `{null: a, "null": b}` is a duplicate-key error
//!   here, and `block_item` on a null-key map writes `- "null": a` where
//!   Python writes `- None: a`.
//! - `"\U00110000"` is a "bad escape" error where Python raises an uncaught
//!   `ValueError`.
//! - Flow collections nested more than `MAX_FLOW_DEPTH` (128) levels deep are an
//!   error (a stack overflow would abort the server).

use std::fmt::Write;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::okf::pystr::{is_py_space, py_repr_str, py_rstrip, py_strip};

/// A parsed YAML value. Every scalar is a string, as in `okf.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `~`, `null`, `Null`, `NULL`, or nothing.
    Null,
    /// Any scalar.
    Str(String),
    /// A flow (`[a, b]`) or block (`- a`) list.
    Seq(Vec<Value>),
    /// A flow (`{ a: b }`) or block mapping, in source order.
    Map(IndexMap<String, Value>),
}

impl Value {
    /// Returns the string if this is a scalar.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Null | Value::Seq(_) | Value::Map(_) => None,
        }
    }

    /// Converts to JSON (strings, arrays, objects, null), the shape the parity
    /// tests compare against `okf.py`'s `json.dumps` output. Map keys keep
    /// their source order (serde_json's `preserve_order` feature), as Python
    /// dicts do.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::Seq(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_json).collect())
            }
            Value::Map(map) => serde_json::Value::Object(
                map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect(),
            ),
        }
    }
}

/// A frontmatter parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
// The fields are public on purpose: this is a plain error record that callers
// read directly, the documented exception to the private-fields rule.
pub struct YamlError {
    /// What went wrong.
    pub message: String,
    /// 1-based file line, when known.
    pub line: Option<usize>,
}

impl YamlError {
    fn new(message: impl Into<String>, line: Option<usize>) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }
}

/// One frontmatter line with its 1-based file line number.
pub type NumberedLine<'a> = (usize, &'a str);

/// Number of leading spaces (tabs don't count, as in `okf.py`).
pub(crate) fn indent_of(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// `text` without its first `n` characters (Python's `t[n:]`; never panics).
fn skip_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((byte_index, _)) => &text[byte_index..],
        None => "",
    }
}

/// Deepest nesting of `[`/`{` the inline parser accepts. A deeper input is an
/// error, because unbounded recursion would overflow the stack and abort the
/// whole server (Python raises a catchable `RecursionError` near 1000).
const MAX_FLOW_DEPTH: usize = 128;

/// Parser for one line's worth of inline YAML (a scalar or a flow collection).
struct Inline<'a> {
    chars: Vec<char>,
    pos: usize,
    line: Option<usize>,
    source: &'a str,
    depth: usize,
}

// `'a` is a lifetime parameter: `source` is a borrowed `&str` that must
// outlive the parser, much like holding a reference to a Python string.
impl<'a> Inline<'a> {
    fn new(source: &'a str, line: Option<usize>) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line,
            source,
            depth: 0,
        }
    }

    fn err(&self, message: &str) -> YamlError {
        YamlError::new(
            format!("{message} in {}", py_repr_str(self.source)),
            self.line,
        )
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn node(&mut self, flow: bool) -> Result<Value, YamlError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.nested(Self::flow_map),
            Some('[') => self.nested(Self::flow_seq),
            Some('"') => self.double_quoted().map(Value::Str),
            Some('\'') => self.single_quoted().map(Value::Str),
            // `c @ (...)` binds the matched character to `c` while also
            // requiring it to be one of the listed alternatives.
            Some(c @ ('&' | '*' | '!' | '|' | '>' | '%' | '@' | '`')) => {
                // `encode_utf8` writes the char into a stack buffer and returns it as `&str`.
                Err(self.err(&format!(
                    "unsupported YAML indicator {}",
                    py_repr_str(c.encode_utf8(&mut [0u8; 4]))
                )))
            }
            _ => Ok(self.plain(flow).map_or(Value::Null, Value::Str)),
        }
    }

    /// Runs a flow-collection parser one level deeper, enforcing the limit.
    fn nested(
        &mut self,
        parse: fn(&mut Self) -> Result<Value, YamlError>,
    ) -> Result<Value, YamlError> {
        if self.depth >= MAX_FLOW_DEPTH {
            return Err(self.err("flow collections nested too deeply"));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn double_quoted(&mut self) -> Result<String, YamlError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            // `let ... else` binds the pattern or runs the diverging block
            // (here: return early), like `if x is None: return ...` in Python.
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated double-quoted string"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    let escape = self.chars.get(self.pos + 1).copied();
                    if let Some(decoded) = escape.and_then(simple_escape) {
                        out.push(decoded);
                        self.pos += 2;
                        continue;
                    }
                    let width = match escape {
                        Some('x') => 2,
                        Some('u') => 4,
                        Some('U') => 8,
                        _ => {
                            let shown = escape.map(String::from).unwrap_or_default();
                            return Err(self.err(&format!("invalid escape \\{shown}")));
                        }
                    };
                    let start = self.pos + 2;
                    let hex: String = self.chars.iter().skip(start).take(width).collect();
                    if hex.chars().count() != width || !hex.chars().all(|h| h.is_ascii_hexdigit()) {
                        return Err(self.err("bad escape"));
                    }
                    let code = u32::from_str_radix(&hex, 16).map_err(|_| self.err("bad escape"))?;
                    let decoded = char::from_u32(code).ok_or_else(|| self.err("bad escape"))?;
                    out.push(decoded);
                    self.pos = start + width;
                }
                other => {
                    out.push(other);
                    self.pos += 1;
                }
            }
        }
    }

    fn single_quoted(&mut self) -> Result<String, YamlError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated single-quoted string"));
            };
            self.pos += 1;
            if c != '\'' {
                out.push(c);
                continue;
            }
            // A doubled quote ('') is an escaped quote; a single one ends the string.
            if self.peek() == Some('\'') {
                out.push('\'');
                self.pos += 1;
                continue;
            }
            return Ok(out);
        }
    }

    /// A plain (unquoted) scalar; `None` for the null spellings.
    fn plain(&mut self, flow: bool) -> Option<String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            let next = self.chars.get(self.pos + 1).copied();
            if c == '#' && self.pos > start && matches!(self.chars[self.pos - 1], ' ' | '\t') {
                break;
            }
            if flow && matches!(c, ',' | ']' | '}') {
                break;
            }
            if c == ':'
                && (matches!(next, None | Some(' ' | '\t'))
                    || (flow && matches!(next, Some(',' | ']' | '}'))))
            {
                break;
            }
            self.pos += 1;
        }
        let mut end = self.pos;
        while end > start && is_py_space(self.chars[end - 1]) {
            end -= 1;
        }
        let text: String = self.chars[start..end].iter().collect();
        if matches!(text.as_str(), "" | "~" | "null" | "Null" | "NULL") {
            None
        } else {
            Some(text)
        }
    }

    fn key(&mut self) -> Result<String, YamlError> {
        self.skip_ws();
        let key = match self.peek() {
            Some('"') => self.double_quoted()?,
            Some('\'') => self.single_quoted()?,
            _ => self.plain(true).unwrap_or_else(|| "null".to_string()),
        };
        self.skip_ws();
        if self.peek() != Some(':') {
            return Err(self.err("expected ':' after key"));
        }
        self.pos += 1;
        Ok(key)
    }

    fn flow_map(&mut self) -> Result<Value, YamlError> {
        self.pos += 1;
        let mut map = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek() == Some('}') {
                self.pos += 1;
                return Ok(Value::Map(map));
            }
            let key = self.key()?;
            if map.contains_key(&key) {
                return Err(self.err(&format!("duplicate key {}", py_repr_str(&key))));
            }
            self.skip_ws();
            let value = if matches!(self.peek(), Some(',' | '}')) {
                Value::Null
            } else {
                self.node(true)?
            };
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {}
                _ => return Err(self.err("expected ',' or '}' in flow map")),
            }
        }
    }

    fn flow_seq(&mut self) -> Result<Value, YamlError> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(Value::Seq(items));
            }
            items.push(self.node(true)?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {}
                _ => return Err(self.err("expected ',' or ']' in flow list")),
            }
        }
    }

    fn finish(&mut self) -> Result<(), YamlError> {
        self.skip_ws();
        match self.peek() {
            None | Some('#') => Ok(()),
            Some(_) => Err(self.err("unexpected trailing text")),
        }
    }
}

/// The single-character escapes of a double-quoted scalar.
fn simple_escape(c: char) -> Option<char> {
    Some(match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '"' => '"',
        '\\' => '\\',
        '/' => '/',
        '0' => '\0',
        ' ' => ' ',
        'b' => '\u{8}',
        'f' => '\u{c}',
        'e' => '\u{1b}',
        'a' => '\u{7}',
        'v' => '\u{b}',
        'N' => '\u{85}',
        '_' => '\u{a0}',
        'L' => '\u{2028}',
        'P' => '\u{2029}',
        _ => return None,
    })
}

/// Parses one inline value: a scalar or a flow collection on a single line.
///
/// # Arguments
/// * `text` - the value text (after `key:` or `- `).
/// * `line` - its 1-based file line, for error messages.
///
/// # Errors
/// [`YamlError`] on unsupported indicators, unterminated quotes, bad flow
/// syntax, trailing text, or an unquoted `: ` inside a plain value.
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::{parse_inline, Value};
/// assert_eq!(parse_inline("[a, 'b c']", None).unwrap(),
///            Value::Seq(vec![Value::Str("a".into()), Value::Str("b c".into())]));
/// ```
pub fn parse_inline(text: &str, line: Option<usize>) -> Result<Value, YamlError> {
    let mut parser = Inline::new(text, line);
    let value = parser.node(false)?;
    if parser.peek() == Some(':') {
        return Err(YamlError::new(
            format!(
                "unquoted ': ' inside a plain value {}; quote it",
                py_repr_str(text)
            ),
            line,
        ));
    }
    parser.finish()?;
    Ok(value)
}

/// Strips a value's surrounding whitespace, or all of it when it is a comment.
pub(crate) fn strip_comment(text: &str) -> &str {
    let trimmed = py_strip(text);
    if trimmed.starts_with('#') {
        ""
    } else {
        trimmed
    }
}

/// `key: value` at the start of a (dedented) block-mapping line.
// `LazyLock` is a lazily initialized static: the regex compiles on first use,
// like a module-level `re.compile(...)` that Rust can't run at import time.
// `\w` is spelled out because Rust's differs from Python's (see `pystr`).
static MAP_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^([A-Za-z0-9_][\p{L}\p{N}_.\-/ ]*?|"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')[ \t]*:(?:[ \t]+(.*)|[ \t]*)$"#,
    )
    .expect("MAP_KEY regex is valid")
});

/// True for a line that carries content (not blank, not a comment).
fn is_content_line(text: &str) -> bool {
    let stripped = py_strip(text);
    !stripped.is_empty() && !stripped.starts_with('#')
}

/// Lines that carry content (not blank, not a comment).
fn content_lines<'a>(lines: &[NumberedLine<'a>]) -> Vec<NumberedLine<'a>> {
    lines
        .iter()
        .copied()
        .filter(|(_, t)| is_content_line(t))
        .collect()
}

/// True for a block-list item line (`-` or `- ...`), already dedented.
pub(crate) fn is_seq_start(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// Parses a key's value: the text after `key:` plus its indented child lines.
///
/// # Arguments
/// * `inline` - text after the colon on the key's line.
/// * `children` - the following, more-indented lines (blank and comment lines included).
/// * `line` - the key's 1-based file line.
///
/// # Errors
/// [`YamlError`] when the value can't be parsed.
pub fn parse_value(
    inline: &str,
    children: &[NumberedLine<'_>],
    line: usize,
) -> Result<Value, YamlError> {
    let text = strip_comment(inline);
    if let Some(style) = text.chars().next().filter(|c| matches!(c, '|' | '>')) {
        let indent = children
            .iter()
            .filter(|(_, t)| !py_strip(t).is_empty())
            .map(|(_, t)| indent_of(t))
            .min()
            .unwrap_or(0);
        let body: Vec<&str> = children
            .iter()
            .map(|(_, t)| skip_chars(t, indent))
            .collect();
        let joined = body.join(if style == '|' { "\n" } else { " " });
        return Ok(Value::Str(format!("{}\n", joined.trim_end_matches('\n'))));
    }
    let has_content = children.iter().any(|(_, t)| is_content_line(t));
    if !text.is_empty() {
        if text.starts_with(['[', '{']) && has_content {
            // A flow collection continued over several lines.
            let mut parts = vec![text];
            parts.extend(children.iter().map(|(_, t)| py_strip(t)));
            return parse_inline(&parts.join(" "), Some(line));
        }
        if has_content {
            return Err(YamlError::new(
                "value on the key line and indented lines below it",
                Some(line),
            ));
        }
        return parse_inline(text, Some(line));
    }
    if has_content {
        parse_node(&content_lines(children))
    } else {
        Ok(Value::Null)
    }
}

/// Parses a block node (list or mapping) from content lines.
fn parse_node(content: &[NumberedLine<'_>]) -> Result<Value, YamlError> {
    let indent = indent_of(content[0].1);
    if is_seq_start(&content[0].1[indent..]) {
        parse_seq(content, indent)
    } else {
        parse_map(content, indent)
    }
}

/// Parses a block list. Items are validated and parsed one at a time, as
/// `okf.py`'s generator-based `_split_items` does, so an error in an early
/// item is reported before a malformed later line is even looked at.
fn parse_seq(content: &[NumberedLine<'_>], indent: usize) -> Result<Value, YamlError> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < content.len() {
        let (number, text) = content[i];
        if indent_of(text) != indent || !is_seq_start(&text[indent..]) {
            return Err(YamlError::new(
                format!(
                    "bad indentation or structure: {}",
                    py_repr_str(py_strip(text))
                ),
                Some(number),
            ));
        }
        let mut j = i + 1;
        while j < content.len() && indent_of(content[j].1) > indent {
            j += 1;
        }
        let kids = &content[i + 1..j];
        i = j;
        let rest = &text[indent + 1..];
        let item = py_strip(rest);
        if item.is_empty() || item.starts_with('#') {
            items.push(if kids.is_empty() {
                Value::Null
            } else {
                parse_node(kids)?
            });
            continue;
        }
        let offset = indent + 1 + indent_of(rest);
        if !item.starts_with(['[', '{', '"', '\'']) && MAP_KEY.is_match(item) {
            // `- key: value`: a mapping whose first key shares the dash's line.
            let first = format!("{}{item}", " ".repeat(offset));
            let mut lines = vec![(number, first.as_str())];
            lines.extend_from_slice(kids);
            items.push(parse_map(&lines, offset)?);
        } else {
            items.push(parse_value(item, kids, number)?);
        }
    }
    Ok(Value::Seq(items))
}

fn parse_map(content: &[NumberedLine<'_>], indent: usize) -> Result<Value, YamlError> {
    let mut map = IndexMap::new();
    let mut i = 0;
    while i < content.len() {
        let (number, text) = content[i];
        if indent_of(text) != indent {
            return Err(YamlError::new(
                format!("bad indentation: {}", py_repr_str(py_strip(text))),
                Some(number),
            ));
        }
        let Some(caps) = MAP_KEY.captures(&text[indent..]) else {
            return Err(YamlError::new(
                format!("expected 'key: value', got {}", py_repr_str(py_strip(text))),
                Some(number),
            ));
        };
        let inline = caps.get(2).map_or("", |m| m.as_str());
        let mut j = i + 1;
        // Children are more indented, or a block list at the same indent.
        while j < content.len() && {
            let child = content[j].1;
            indent_of(child) > indent
                || (indent_of(child) == indent
                    && is_seq_start(&child[indent..])
                    && py_strip(inline).is_empty())
        } {
            j += 1;
        }
        let raw_key = &caps[1];
        let key = if raw_key.starts_with(['"', '\'']) {
            match parse_inline(raw_key, Some(number))? {
                Value::Str(s) => s,
                Value::Null | Value::Seq(_) | Value::Map(_) => raw_key.to_string(),
            }
        } else {
            raw_key.to_string()
        };
        if map.contains_key(&key) {
            return Err(YamlError::new(
                format!("duplicate key {}", py_repr_str(&key)),
                Some(number),
            ));
        }
        let value = parse_value(inline, &content[i + 1..j], number)?;
        map.insert(key, value);
        i = j;
    }
    Ok(Value::Map(map))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

static PLAIN_SAFE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_./~+()][^\n\t]*$").expect("PLAIN_SAFE regex is valid")
});
static NUMBER_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(\d[\d_]*)?\.?\d*([eE][-+]?\d+)?$").expect("NUMBER_LIKE regex is valid")
});
const RESERVED_WORDS: [&str; 10] = [
    "true", "false", "yes", "no", "on", "off", "null", "~", "y", "n",
];

/// Would `text` change meaning (or fail to parse) if written unquoted?
///
/// # Arguments
/// * `text` - the scalar.
/// * `flow` - whether it sits inside a flow collection, where `,[]{}` are syntax.
pub fn needs_quote(text: &str, flow: bool) -> bool {
    if text.is_empty() || text != py_strip(text) || !PLAIN_SAFE.is_match(text) {
        return true;
    }
    if RESERVED_WORDS.contains(&text.to_lowercase().as_str())
        || text.contains(": ")
        || text.contains(" #")
        || text.ends_with(':')
    {
        return true;
    }
    if flow && text.contains([',', '[', ']', '{', '}']) {
        return true;
    }
    NUMBER_LIKE.is_match(text)
}

/// Double-quotes a scalar, escaping quotes, backslashes and control characters.
pub fn double_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                write!(out, "\\x{:02x}", c as u32).expect("writing to a String cannot fail");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Where a scalar is written, which decides whether it needs quotes.
///
/// Replaces M1's two positional bools (`flow`, `force`): a call such as
/// `scalar(x, ScalarStyle::Quoted)` says what it means at the call site.
// `Copy` makes this a plain value like a Python int: passing it to `scalar`
// copies it rather than moving it, so a caller can reuse the same variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarStyle {
    /// A block value (`key: value`, `- item`): quoted only when needed.
    Block,
    /// Inside a flow collection (`[a, b]`, `{ k: v }`), where `,[]{}` are
    /// syntax: quoted when needed there.
    Flow,
    /// Always double-quoted.
    Quoted,
}

/// Renders a scalar in the given style.
///
/// # Arguments
/// * `text` - the scalar's value.
/// * `style` - where it is written; see [`ScalarStyle`].
///
/// # Returns
/// `text` unchanged when it can be written plain in that style, otherwise
/// [`double_quote`]d.
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::{scalar, ScalarStyle};
/// assert_eq!(scalar("plain text", ScalarStyle::Block), "plain text");
/// assert_eq!(scalar("a: b", ScalarStyle::Block), "\"a: b\"");
/// assert_eq!(scalar("x", ScalarStyle::Quoted), "\"x\"");
/// ```
pub fn scalar(text: &str, style: ScalarStyle) -> String {
    // `match` on an enum must cover every variant (no `_` arm), so adding a
    // style later is a compile error here until it is handled.
    let quote = match style {
        ScalarStyle::Block => needs_quote(text, false),
        ScalarStyle::Flow => needs_quote(text, true),
        ScalarStyle::Quoted => true,
    };
    if quote {
        double_quote(text)
    } else {
        text.to_string()
    }
}

/// Renders a value in flow style on one line: `{ by: x, at: y }`, `[a, b]`.
pub fn flow_value(value: &Value) -> String {
    match value {
        Value::Map(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", scalar(k, ScalarStyle::Flow), flow_value(v)))
                .collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Seq(items) => {
            let items: Vec<String> = items.iter().map(flow_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Null => "null".to_string(),
        Value::Str(s) => scalar(s, ScalarStyle::Flow),
    }
}

/// Renders one block-list item as lines (no trailing newlines).
///
/// A mapping of scalars becomes `- k: v` with continuation keys aligned
/// under it; any other collection is written in flow style after `- `.
///
/// # Arguments
/// * `value` - the item.
/// * `indent` - spaces before the dash.
pub fn block_item(value: &Value, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    match value {
        Value::Map(map)
            if !map.is_empty()
                && map
                    .values()
                    .all(|v| matches!(v, Value::Null | Value::Str(_))) =>
        {
            map.iter()
                .enumerate()
                .map(|(i, (k, v))| {
                    let lead = if i == 0 {
                        format!("{pad}- ")
                    } else {
                        format!("{pad}  ")
                    };
                    let rendered = v
                        .as_str()
                        .map(|s| scalar(s, ScalarStyle::Block))
                        .unwrap_or_default();
                    let line = format!("{lead}{}: {rendered}", scalar(k, ScalarStyle::Block));
                    py_rstrip(&line).to_string()
                })
                .collect()
        }
        Value::Map(_) | Value::Seq(_) => vec![format!("{pad}- {}", flow_value(value))],
        Value::Null => vec![format!("{pad}- null")],
        Value::Str(s) => vec![format!("{pad}- {}", scalar(s, ScalarStyle::Block))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn lines(texts: &[&'static str]) -> Vec<NumberedLine<'static>> {
        texts.iter().enumerate().map(|(i, t)| (i + 3, *t)).collect()
    }

    #[test]
    fn flow_errors_quote_like_python_repr() {
        // Expected text is what `okf.py` (Python 3.14) raises for the same input.
        let err = parse_inline("{by: a, by: b}", Some(3)).unwrap_err();
        assert_eq!(err.message, "duplicate key 'by' in '{by: a, by: b}'");
        assert_eq!(err.line, Some(3));
        let err = parse_inline("{\"\u{e9}\": 1, \"\u{e9}\": 2}", None).unwrap_err();
        assert_eq!(
            err.message,
            "duplicate key '\u{e9}' in '{\"\u{e9}\": 1, \"\u{e9}\": 2}'"
        );
        let err = parse_inline("&x", None).unwrap_err();
        assert_eq!(err.message, "unsupported YAML indicator '&' in '&x'");
    }

    #[test]
    fn plain_scalars_and_nulls() {
        assert_eq!(parse_inline("hello world", None).unwrap(), s("hello world"));
        assert_eq!(parse_inline("5", None).unwrap(), s("5"));
        for null in ["", "~", "null", "Null", "NULL"] {
            assert_eq!(parse_inline(null, None).unwrap(), Value::Null, "{null:?}");
        }
    }

    #[test]
    fn plain_scalar_stops_at_comment_and_keeps_inner_hash() {
        assert_eq!(parse_inline("a#b # note", None).unwrap(), s("a#b"));
    }

    #[test]
    fn colon_rules_in_plain_scalars() {
        assert_eq!(
            parse_inline("https://x.y/z", None).unwrap(),
            s("https://x.y/z")
        );
        let err = parse_inline("a: b", Some(7)).unwrap_err();
        assert!(err.message.contains("unquoted ': '"));
        assert_eq!(err.line, Some(7));
    }

    #[test]
    fn double_quoted_escapes() {
        assert_eq!(
            parse_inline(r#""a\"b\\c\nd\x41é\L""#, None).unwrap(),
            s("a\"b\\c\ndA\u{e9}\u{2028}")
        );
        assert!(parse_inline(r#""bad \q""#, None).is_err());
        assert!(parse_inline(r#""\x4""#, None).is_err());
        assert!(parse_inline(r#""open"#, None).is_err());
    }

    #[test]
    fn single_quoted_doubles_quotes() {
        assert_eq!(parse_inline("'it''s'", None).unwrap(), s("it's"));
        assert!(parse_inline("'open", None).is_err());
    }

    #[test]
    fn flow_collections() {
        assert_eq!(
            parse_inline("{ by: claude, at: 2026-10-26T23:01:11Z }", None).unwrap(),
            map(&[("by", s("claude")), ("at", s("2026-10-26T23:01:11Z"))])
        );
        assert_eq!(
            parse_inline("[a, [b, c], {d: }]", None).unwrap(),
            Value::Seq(vec![
                s("a"),
                Value::Seq(vec![s("b"), s("c")]),
                map(&[("d", Value::Null)])
            ])
        );
        assert_eq!(parse_inline("{}", None).unwrap(), map(&[]));
        assert!(parse_inline("{a: 1, a: 2}", None)
            .unwrap_err()
            .message
            .contains("duplicate"));
        assert!(parse_inline("[a, b", None).is_err());
        assert!(parse_inline("[a] trailing", None).is_err());
    }

    #[test]
    fn unsupported_indicators_are_errors() {
        for text in ["&anchor", "*alias", "!tag", "@at", "`tick"] {
            assert!(parse_inline(text, None).is_err(), "{text}");
        }
    }

    #[test]
    fn block_list_of_mappings() {
        let children = lines(&[
            "  - id: a",
            "    resource: r1",
            "  - id: b",
            "    resource: \"r 2\"",
        ]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            Value::Seq(vec![
                map(&[("id", s("a")), ("resource", s("r1"))]),
                map(&[("id", s("b")), ("resource", s("r 2"))]),
            ])
        );
    }

    #[test]
    fn block_list_at_key_indent() {
        let children = lines(&["- one", "- two"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            Value::Seq(vec![s("one"), s("two")])
        );
    }

    #[test]
    fn nested_block_mapping_with_comments() {
        let children = lines(&["  # a comment", "  by: x", "", "  at: y  # trailing"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            map(&[("by", s("x")), ("at", s("y"))])
        );
    }

    #[test]
    fn block_scalars() {
        let children = lines(&["  line one", "    indented", "", "  last"]);
        assert_eq!(
            parse_value("|", &children, 2).unwrap(),
            s("line one\n  indented\n\nlast\n")
        );
        assert_eq!(
            parse_value(">", &children, 2).unwrap(),
            s("line one   indented  last\n")
        );
    }

    #[test]
    fn multi_line_flow_collection() {
        let children = lines(&["  b,", "  c]"]);
        assert_eq!(
            parse_value("[a,", &children, 2).unwrap(),
            Value::Seq(vec![s("a"), s("b"), s("c")])
        );
    }

    #[test]
    fn value_on_key_line_and_children_is_an_error() {
        let children = lines(&["  more"]);
        let err = parse_value("x", &children, 2).unwrap_err();
        assert_eq!(err.line, Some(2));
    }

    #[test]
    fn quoted_map_keys() {
        let children = lines(&["  \"a b\": 1", "  'c': 2"]);
        assert_eq!(
            parse_value("", &children, 2).unwrap(),
            map(&[("a b", s("1")), ("c", s("2"))])
        );
    }

    #[test]
    fn bad_indentation_is_reported_with_line() {
        let children = lines(&["  a: 1", " b: 2"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(
            err.message.starts_with("bad indentation"),
            "{}",
            err.message
        );
        assert_eq!(err.line, Some(4));
    }

    #[test]
    fn list_items_are_validated_lazily_like_okf_py() {
        // okf.py's generator reports item 1's error before looking at item 3.
        let children = [(4, " - b"), (5, "  lit"), (6, "- three")];
        let err = parse_value("", &children, 3).unwrap_err();
        assert!(
            err.message.starts_with("value on the key line"),
            "{}",
            err.message
        );
        assert_eq!(err.line, Some(4));

        let children = [
            (3, "  - two"),
            (4, "  - id: one"),
            (5, "   - c"),
            (6, "  by: a"),
            (7, "    deep"),
        ];
        let err = parse_value("", &children, 3).unwrap_err();
        assert!(
            err.message.starts_with("bad indentation"),
            "{}",
            err.message
        );
        assert_eq!(err.line, Some(5));
    }

    #[test]
    fn deeper_line_under_a_scalar_is_an_error_on_the_key_line() {
        let children = lines(&["  a: 1", "   b: 2"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(err.message.contains("value on the key line"));
        assert_eq!(err.line, Some(3));
    }

    #[test]
    fn needs_quote_cases() {
        for text in [
            "", " x", "true", "No", "a: b", "a #b", "end:", "5", "-1.5e3", "#x", "[x",
        ] {
            assert!(needs_quote(text, false), "{text:?} should need quotes");
        }
        for text in [
            "plain",
            "a:b",
            "claude-code/claude-opus-5-5",
            "2026-10-26T23:01:11Z",
        ] {
            assert!(!needs_quote(text, false), "{text:?} should be plain");
        }
        assert!(needs_quote("a,b", true));
        assert!(!needs_quote("a,b", false));
    }

    #[test]
    fn double_quote_escapes_controls() {
        assert_eq!(
            double_quote("a\"b\\c\n\t\u{1}\u{7f}"),
            "\"a\\\"b\\\\c\\n\\t\\x01\\x7f\""
        );
    }

    #[test]
    fn flow_value_renders_like_okf_py() {
        let item = map(&[
            ("by", s("claude-code/claude-opus-5-5")),
            ("at", s("2026-10-26T23:01:11Z")),
        ]);
        assert_eq!(
            flow_value(&item),
            "{ by: claude-code/claude-opus-5-5, at: 2026-10-26T23:01:11Z }"
        );
        assert_eq!(
            flow_value(&Value::Seq(vec![s("a,b"), Value::Null])),
            "[\"a,b\", null]"
        );
        assert_eq!(flow_value(&map(&[])), "{  }");
    }

    #[test]
    fn block_item_shapes() {
        let entry = map(&[
            ("id", s("x")),
            ("resource", s("a: b")),
            ("title", Value::Null),
        ]);
        assert_eq!(
            block_item(&entry, 2),
            vec!["  - id: x", "    resource: \"a: b\"", "    title:"]
        );
        assert_eq!(block_item(&s("q"), 0), vec!["- q"]);
        assert_eq!(block_item(&Value::Seq(vec![s("a")]), 2), vec!["  - [a]"]);
        assert_eq!(block_item(&Value::Null, 2), vec!["  - null"]);
    }

    #[test]
    fn map_key_uses_python_word_class() {
        let ok = lines(&["  a\u{b2}: 1"]);
        assert_eq!(
            parse_value("", &ok, 2).unwrap(),
            map(&[("a\u{b2}", s("1"))])
        );
        let combining = lines(&["  e\u{301}x: 1"]);
        assert!(parse_value("", &combining, 2).is_err());
    }

    #[test]
    fn flow_nesting_is_limited() {
        assert!(parse_inline(&"[".repeat(10_000), None).is_err());
        let err =
            parse_inline(&format!("{}{}", "[".repeat(200), "]".repeat(200)), None).unwrap_err();
        assert!(err.message.contains("nested too deeply"));
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_inline(&ok, None).is_ok());
    }

    #[test]
    fn block_scalar_blank_line_uses_python_whitespace() {
        let children = lines(&["    a", "\u{3000}", "    b"]);
        assert_eq!(parse_value("|", &children, 2).unwrap(), s("a\n\nb\n"));
    }

    #[test]
    fn tab_indented_child_line_is_an_error() {
        // okf.py: `expected 'key: value', got 'a: 1'` (tabs don't count as indent).
        let children = lines(&["\ta: 1"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(
            err.message.starts_with("expected 'key: value'"),
            "{}",
            err.message
        );
        let mixed = lines(&["  a: 1", "\tb: 2"]);
        let err = parse_value("", &mixed, 2).unwrap_err();
        assert!(
            err.message.starts_with("bad indentation"),
            "{}",
            err.message
        );
    }

    #[test]
    fn seq_item_mapping_with_nested_child_line() {
        // okf.py: "value on the key line and indented lines below it".
        let children = lines(&["  - k: v", "      n: x"]);
        let err = parse_value("", &children, 2).unwrap_err();
        assert!(err.message.contains("value on the key line"));
        let fine = lines(&["  - k: v", "    n: x"]);
        assert_eq!(
            parse_value("", &fine, 2).unwrap(),
            Value::Seq(vec![map(&[("k", s("v")), ("n", s("x"))])])
        );
    }

    #[test]
    fn to_json_shapes() {
        let value = map(&[("a", Value::Seq(vec![s("x"), Value::Null]))]);
        assert_eq!(value.to_json(), serde_json::json!({"a": ["x", null]}));
    }

    #[test]
    fn to_json_keeps_source_key_order() {
        let value = map(&[("z", s("1")), ("a", s("2")), ("m", s("3"))]);
        let text = serde_json::to_string(&value.to_json()).expect("serializable");
        assert_eq!(text, r#"{"z":"1","a":"2","m":"3"}"#);
    }

    #[test]
    fn scalar_styles() {
        assert_eq!(scalar("a,b", ScalarStyle::Block), "a,b");
        assert_eq!(scalar("a,b", ScalarStyle::Flow), "\"a,b\"");
        assert_eq!(scalar("plain", ScalarStyle::Quoted), "\"plain\"");
        assert_eq!(scalar("a: b", ScalarStyle::Block), "\"a: b\"");
    }

    #[hegel::test]
    fn scalar_round_trips_through_parse_inline(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let rendered = scalar(&text, ScalarStyle::Block);
        assert_eq!(parse_inline(&rendered, None).unwrap(), Value::Str(text));
    }

    #[hegel::test]
    fn flow_list_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        let items: Vec<String> = tc.draw(generators::vecs(generators::text()));
        let value = Value::Seq(items.into_iter().map(Value::Str).collect());
        assert_eq!(parse_inline(&flow_value(&value), None).unwrap(), value);
    }

    #[hegel::test]
    fn flow_map_round_trips(tc: hegel::TestCase) {
        use hegel::generators;
        let keys: Vec<String> = tc.draw(generators::vecs(generators::text()).unique(true));
        let value = Value::Map(
            keys.into_iter()
                .map(|k| (k.clone(), Value::Str(k)))
                .collect(),
        );
        assert_eq!(parse_inline(&flow_value(&value), None).unwrap(), value);
    }

    #[hegel::test]
    fn parse_inline_never_panics(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let _ = parse_inline(&text, Some(1));
    }

    #[hegel::test]
    fn block_parsing_never_panics(tc: hegel::TestCase) {
        use hegel::generators;
        let alphabet = [
            " ", "-", ":", "\t", "#", "|", "[", "a", "e\u{301}", "\u{3000}",
        ];
        let pieces = generators::vecs(generators::sampled_from(&alphabet)).max_size(8);
        let drawn: Vec<Vec<&str>> = tc.draw(generators::vecs(pieces).min_size(1).max_size(6));
        let owned: Vec<String> = drawn.iter().map(|line| line.concat()).collect();
        let children: Vec<NumberedLine<'_>> = owned
            .iter()
            .enumerate()
            .map(|(i, text)| (i + 3, text.as_str()))
            .collect();
        for inline in ["", "|", "["] {
            let _ = parse_value(inline, &children, 2);
        }
    }
}
