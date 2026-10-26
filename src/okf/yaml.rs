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
//! - Error messages quote with `"` where Python's `repr` uses `'`.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

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
    /// tests compare against `okf.py`'s `json.dumps` output.
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

/// Parser for one line's worth of inline YAML (a scalar or a flow collection).
struct Inline<'a> {
    chars: Vec<char>,
    pos: usize,
    line: Option<usize>,
    source: &'a str,
}

impl<'a> Inline<'a> {
    fn new(source: &'a str, line: Option<usize>) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line,
            source,
        }
    }

    fn err(&self, message: &str) -> YamlError {
        YamlError::new(format!("{message} in {:?}", self.source), self.line)
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
            Some('{') => self.flow_map(),
            Some('[') => self.flow_seq(),
            Some('"') => self.double_quoted().map(Value::Str),
            Some('\'') => self.single_quoted().map(Value::Str),
            Some(c @ ('&' | '*' | '!' | '|' | '>' | '%' | '@' | '`')) => {
                Err(self.err(&format!("unsupported YAML indicator {c:?}")))
            }
            _ => Ok(self.plain(flow).map_or(Value::Null, Value::Str)),
        }
    }

    fn double_quoted(&mut self) -> Result<String, YamlError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
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
                    let decoded = (hex.chars().count() == width
                        && hex.chars().all(|h| h.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32);
                    match decoded {
                        Some(ch) => out.push(ch),
                        None => return Err(self.err("bad escape")),
                    }
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
        let text: String = self.chars[start..self.pos].iter().collect();
        let text = text.trim_end();
        match text {
            "" | "~" | "null" | "Null" | "NULL" => None,
            other => Some(other.to_string()),
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
                return Err(self.err(&format!("duplicate key {key:?}")));
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
            format!("unquoted ': ' inside a plain value {text:?}; quote it"),
            line,
        ));
    }
    parser.finish()?;
    Ok(value)
}

/// Strips a value's surrounding whitespace, or all of it when it is a comment.
pub(crate) fn strip_comment(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with('#') {
        ""
    } else {
        trimmed
    }
}

/// `key: value` at the start of a (dedented) block-mapping line.
static MAP_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^([A-Za-z0-9_][\w.\-/ ]*?|"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')[ \t]*:(?:[ \t]+(.*)|[ \t]*)$"#,
    )
    .expect("MAP_KEY regex is valid")
});

/// Lines that carry content (not blank, not a comment).
fn content_lines<'a>(lines: &[NumberedLine<'a>]) -> Vec<NumberedLine<'a>> {
    lines
        .iter()
        .copied()
        .filter(|(_, t)| !t.trim().is_empty() && !t.trim().starts_with('#'))
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
            .filter(|(_, t)| !t.trim().is_empty())
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
    let has_content = !content_lines(children).is_empty();
    if !text.is_empty() {
        if text.starts_with(['[', '{']) && has_content {
            // A flow collection continued over several lines.
            let mut parts = vec![text];
            parts.extend(children.iter().map(|(_, t)| t.trim()));
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
    let content = content_lines(children);
    if content.is_empty() {
        Ok(Value::Null)
    } else {
        parse_node(&content)
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

/// Splits block-list lines into (item line, its child lines).
fn split_items<'c, 'a>(
    content: &'c [NumberedLine<'a>],
    indent: usize,
) -> Result<Vec<(NumberedLine<'a>, &'c [NumberedLine<'a>])>, YamlError> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < content.len() {
        let (number, text) = content[i];
        if indent_of(text) != indent || !is_seq_start(&text[indent..]) {
            return Err(YamlError::new(
                format!("bad indentation or structure: {:?}", text.trim()),
                Some(number),
            ));
        }
        let mut j = i + 1;
        while j < content.len() && indent_of(content[j].1) > indent {
            j += 1;
        }
        items.push((content[i], &content[i + 1..j]));
        i = j;
    }
    Ok(items)
}

fn parse_seq(content: &[NumberedLine<'_>], indent: usize) -> Result<Value, YamlError> {
    let mut items = Vec::new();
    for ((number, text), kids) in split_items(content, indent)? {
        let rest = &text[indent + 1..];
        let item = rest.trim();
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
                format!("bad indentation: {:?}", text.trim()),
                Some(number),
            ));
        }
        let Some(caps) = MAP_KEY.captures(&text[indent..]) else {
            return Err(YamlError::new(
                format!("expected 'key: value', got {:?}", text.trim()),
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
                    && inline.trim().is_empty())
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
                format!("duplicate key {key:?}"),
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
    if text.is_empty() || text != text.trim() || !PLAIN_SAFE.is_match(text) {
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
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Renders a scalar, quoting only when needed (or always, with `force`).
///
/// # Examples
/// ```
/// use okf_mcp::okf::yaml::scalar;
/// assert_eq!(scalar("plain text", false, false), "plain text");
/// assert_eq!(scalar("a: b", false, false), "\"a: b\"");
/// assert_eq!(scalar("x", false, true), "\"x\"");
/// ```
pub fn scalar(text: &str, flow: bool, force: bool) -> String {
    if force || needs_quote(text, flow) {
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
                .map(|(k, v)| format!("{}: {}", scalar(k, true, false), flow_value(v)))
                .collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Seq(items) => {
            let items: Vec<String> = items.iter().map(flow_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Null => "null".to_string(),
        Value::Str(s) => scalar(s, true, false),
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
                        .map(|s| scalar(s, false, false))
                        .unwrap_or_default();
                    format!("{lead}{}: {rendered}", scalar(k, false, false))
                        .trim_end()
                        .to_string()
                })
                .collect()
        }
        Value::Map(_) | Value::Seq(_) => vec![format!("{pad}- {}", flow_value(value))],
        Value::Null => vec![format!("{pad}- null")],
        Value::Str(s) => vec![format!("{pad}- {}", scalar(s, false, false))],
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
    fn to_json_shapes() {
        let value = map(&[("a", Value::Seq(vec![s("x"), Value::Null]))]);
        assert_eq!(value.to_json(), serde_json::json!({"a": ["x", null]}));
    }

    #[hegel::test]
    fn scalar_round_trips_through_parse_inline(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let rendered = scalar(&text, false, false);
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
}
