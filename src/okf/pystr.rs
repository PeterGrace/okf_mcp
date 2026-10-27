//! Python string semantics shared by the OKF parsers.
//!
//! Rust's `str::trim` and the `regex` crate's classes differ from Python's in
//! edge cases, and the ports must match `okf.py` exactly. For regexes: Python's
//! `\w` is `[\p{L}\p{N}_]` and Python's `\s` is `[\s\x1c-\x1f]`, whereas the
//! Rust `regex` classes differ, so ported patterns must spell these classes out.

use std::borrow::Cow;
// `as _` imports the trait only for its methods (`write!` on a `String`),
// without binding the name `Write` in this module.
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;

use crate::okf::yaml::Value;

/// Python's `str.isspace()`: Unicode White_Space plus U+001C..U+001F, which
/// Rust's `char::is_whitespace` leaves out.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `\w` for one character: a letter (`\p{L}`), a number (`\p{N}`) or `_`.
///
/// Not `char::is_alphanumeric` (which counts combining marks and other
/// `Alphabetic` characters that Python's `\w` rejects). Matches the class the
/// ported regexes spell out, by asking the `regex` crate with that very class.
pub fn is_py_word(c: char) -> bool {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_]$").expect("word regex is valid"));
    let mut buffer = [0u8; 4];
    WORD.is_match(c.encode_utf8(&mut buffer))
}

/// Python's `s.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python's `s.rstrip()`.
pub fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(is_py_space)
}

/// Python's `s.lstrip()`.
pub fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(is_py_space)
}

/// Python's universal newlines: what `open(path, encoding="utf-8").read()`
/// (text mode, `newline=None`) returns for text read from disk. Every `\r\n`
/// and every lone `\r` becomes `\n`; nothing else changes (other Unicode line
/// separators are left alone, as in Python).
///
/// `okf.py` reads files this way for markdown analysis (headings, links,
/// footnotes, anchors), but reads them byte-preserving (`newline=""`) for
/// frontmatter editing. Only the analysis path may use this.
///
/// Returns the input unchanged, without allocating, when it has no `\r`.
/// (`Cow` is "clone on write": either a borrow of `text` or a new `String`.)
///
/// # Examples
/// ```
/// use okf_mcp::okf::pystr::universal_newlines;
/// assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
/// ```
pub fn universal_newlines(text: &str) -> Cow<'_, str> {
    let Some(first) = text.find('\r') else {
        return Cow::Borrowed(text);
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..first]);
    let mut rest = &text[first..];
    // Invariant at the top of each pass: `rest` starts with `\r`.
    while let Some(after_cr) = rest.strip_prefix('\r') {
        out.push('\n');
        let after = after_cr.strip_prefix('\n').unwrap_or(after_cr);
        let next = after.find('\r').unwrap_or(after.len());
        out.push_str(&after[..next]);
        rest = &after[next..];
    }
    Cow::Owned(out)
}

/// Python's `repr()` of a `str`: single quotes unless the text has a `'` and
/// no `"`; backslash escapes for the chosen quote, `\\`, `\t`, `\n`, `\r`;
/// `\xNN` for other control characters and for non-printable characters up
/// to U+00FF; `\uNNNN`/`\UNNNNNNNN` above. Printable characters stay as they
/// are.
///
/// "Printable" is Python's `str.isprintable`: not in Unicode categories Cc,
/// Cf, Co, Cn, Zl, Zp or Zs, except the ASCII space. (Python 3.14 and the
/// `regex` crate may disagree on a few recently assigned code points.)
///
/// # Arguments
/// * `text` - the string to quote.
///
/// # Returns
/// The quoted text, byte for byte what Python's `repr(text)` prints.
///
/// # Examples
/// ```
/// use okf_mcp::okf::pystr::py_repr_str;
/// assert_eq!(py_repr_str("it's"), "\"it's\"");
/// assert_eq!(py_repr_str("caf\u{e9}\t"), "'caf\u{e9}\\t'");
/// ```
pub fn py_repr_str(text: &str) -> String {
    static NON_PRINTABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$")
            .expect("NON_PRINTABLE regex is valid")
    });
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    let mut buffer = [0u8; 4];
    for c in text.chars() {
        // `c as u32` is the code point. `write!` into a String cannot fail,
        // so `let _ =` discards its `Ok(())`. Arms are tried top to bottom,
        // and `c if ...` is a guard: the arm matches only when it holds.
        match c {
            c if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c < ' ' || c == '\u{7f}' => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if c.is_ascii() => out.push(c),
            c if !NON_PRINTABLE.is_match(c.encode_utf8(&mut buffer)) => out.push(c),
            c if (c as u32) <= 0xff => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a parsed frontmatter value (`None`, `'text'`,
/// `['a', 'b']`, `{'k': 'v'}`), as `okf.py` embeds in lint messages.
///
/// # Arguments
/// * `value` - the parsed value; mappings keep their source key order.
///
/// # Returns
/// The text Python's `repr()` gives for the equivalent `None`/`str`/`list`/`dict`.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Str(text) => py_repr_str(text),
        Value::Seq(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", py_repr_str(key), py_repr(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `repr(d.get(key))`: `None` for a missing key.
///
/// # Arguments
/// * `value` - the looked-up value, `None` when the key is missing.
///
/// # Returns
/// `"None"` for a missing key, otherwise [`py_repr`] of the value.
pub fn py_repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_string(), py_repr)
}

/// Python's `str()` of a frontmatter value: the text itself for a string,
/// otherwise its `repr()`.
///
/// # Arguments
/// * `value` - the parsed value.
///
/// # Returns
/// The string Python's `str(value)` gives.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        Value::Null | Value::Seq(_) | Value::Map(_) => py_repr(value),
    }
}

/// Python truthiness of `d.get(key)`: false for a missing key, `None`, an
/// empty string, an empty list and an empty mapping.
///
/// # Arguments
/// * `value` - the looked-up value, `None` when the key is missing.
///
/// # Returns
/// What `bool(d.get(key))` gives in Python.
pub fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Str(text)) => !text.is_empty(),
        Some(Value::Seq(items)) => !items.is_empty(),
        Some(Value::Map(map)) => !map.is_empty(),
    }
}

/// `re.escape(literal)` for use inside a `(?i:...)` group, matching what
/// Python's `re.I` matches. The `regex` crate's case folding agrees with
/// Python's except that Python lets `i`, `I`, U+0130 and U+0131 all match
/// each other, so those four become the class `[iI\u{130}\u{131}]`
/// (verified fact 6 of the M2 plan).
///
/// # Arguments
/// * `literal` - the text to match literally.
///
/// # Returns
/// A regex fragment; it only has Python's `re.I` meaning inside `(?i:...)`.
///
/// # Examples
/// ```
/// use okf_mcp::okf::pystr::ignorecase_literal;
/// assert_eq!(ignorecase_literal("a.i"), "a\\.[iI\u{130}\u{131}]");
/// ```
pub fn ignorecase_literal(literal: &str) -> String {
    let escaped = regex::escape(literal);
    // `regex::escape` only adds backslashes before ASCII punctuation, so no
    // escape sequence contains one of the four letters replaced here.
    let mut out = String::with_capacity(escaped.len());
    for c in escaped.chars() {
        match c {
            'i' | 'I' | '\u{130}' | '\u{131}' => out.push_str("[iI\u{130}\u{131}]"),
            c => out.push(c),
        }
    }
    out
}

/// Python's `urllib.parse.unquote(text)` (UTF-8, `errors="replace"`):
/// `%XX` escapes become bytes, invalid UTF-8 becomes U+FFFD (maximal
/// subparts, as Python's decoder does), and a `%` not followed by two hex
/// digits stays as it is.
///
/// # Arguments
/// * `text` - the percent-encoded text, such as a link's `#fragment`.
///
/// # Returns
/// The decoded text, borrowed without allocating when `text` has no `%`.
///
/// # Examples
/// ```
/// use okf_mcp::okf::pystr::py_unquote;
/// assert_eq!(py_unquote("a%20b%zz"), "a b%zz");
/// ```
pub fn py_unquote(text: &str) -> Cow<'_, str> {
    if !text.contains('%') {
        return Cow::Borrowed(text);
    }
    // Python's hex digits here are ASCII only; `to_digit(16)` agrees, since a
    // byte >= 0x80 becomes a non-ASCII `char` that is not a hex digit.
    let hex = |byte: u8| char::from(byte).to_digit(16);
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // `.get` returns `None` past the end instead of panicking, and
            // `and_then` chains the hex check onto it.
            if let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) {
                // Two hex digits make at most 255, so the cast cannot truncate.
                decoded.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    // Python decodes each ASCII run separately, but one lossy pass is equal:
    // literal non-ASCII characters are copied as whole UTF-8 sequences, and a
    // decoded byte never merges with a following literal (which starts with
    // a lead byte or ASCII), so the invalid subparts are the same.
    Cow::Owned(String::from_utf8_lossy(&decoded).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use indexmap::IndexMap;

    #[test]
    fn repr_str_picks_quotes_and_escapes_like_python() {
        assert_eq!(py_repr_str("abc"), "'abc'");
        assert_eq!(py_repr_str("a'b"), "\"a'b\"");
        assert_eq!(py_repr_str("a\"b'"), "'a\"b\\''");
        assert_eq!(
            py_repr_str("x\u{200b}y\tz\u{e9}\u{85}"),
            "'x\\u200by\\tz\u{e9}\\x85'"
        );
        assert_eq!(py_repr_str("\\\n\r\u{7f}\u{0}"), "'\\\\\\n\\r\\x7f\\x00'");
        assert_eq!(
            py_repr_str("\u{a0} \u{1f600}\u{e0000}"),
            "'\\xa0 \u{1f600}\\U000e0000'"
        );
    }

    #[test]
    fn repr_and_str_of_values_follow_python() {
        let mut map = IndexMap::new();
        map.insert("by".to_string(), Value::Str("x".to_string()));
        map.insert("at".to_string(), Value::Null);
        let value = Value::Seq(vec![Value::Map(map), Value::Seq(Vec::new())]);
        assert_eq!(py_repr(&value), "[{'by': 'x', 'at': None}, []]");
        assert_eq!(py_repr(&Value::Map(IndexMap::new())), "{}");
        assert_eq!(py_str(&Value::Str("plain".to_string())), "plain");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_repr_opt(None), "None");
    }

    #[test]
    fn truthiness_follows_python() {
        assert!(!py_truthy(None));
        assert!(!py_truthy(Some(&Value::Null)));
        assert!(!py_truthy(Some(&Value::Str(String::new()))));
        assert!(!py_truthy(Some(&Value::Seq(Vec::new()))));
        assert!(!py_truthy(Some(&Value::Map(IndexMap::new()))));
        assert!(py_truthy(Some(&Value::Str("0".to_string()))));
    }

    #[test]
    fn ignorecase_literal_matches_like_python_re_i() {
        let pattern = Regex::new(&format!("^(?i:{})$", ignorecase_literal("Time.Server"))).unwrap();
        assert!(pattern.is_match("TIME.SERVER"));
        assert!(pattern.is_match("t\u{130}me.server"));
        assert!(pattern.is_match("t\u{131}me.server"));
        assert!(!pattern.is_match("timeXserver"), "the dot is escaped");
        let dotted = Regex::new(&format!("^(?i:{})$", ignorecase_literal("\u{130}x"))).unwrap();
        assert!(dotted.is_match("ix") && dotted.is_match("IX"));
    }

    #[test]
    fn unquote_matches_urllib() {
        assert_eq!(py_unquote("plain"), "plain");
        assert_eq!(py_unquote("a%20b%zz%C3%A9%FF"), "a b%zz\u{e9}\u{fffd}");
        assert_eq!(py_unquote("%E2%82\u{e9}"), "\u{fffd}\u{e9}");
        assert_eq!(py_unquote("100%"), "100%");
    }

    #[test]
    fn word_predicate_matches_the_regex_class() {
        let class = Regex::new(r"^[\p{L}\p{N}_]$").expect("valid");
        for c in [
            'a', 'Z', '0', '\u{b2}', '\u{301}', '_', '\u{e9}', '\u{65e5}', '-', ' ', '*',
        ] {
            assert_eq!(is_py_word(c), class.is_match(&c.to_string()), "{c:?}");
        }
        assert!(is_py_word('\u{b2}'));
        assert!(!is_py_word('\u{301}'));
    }

    #[test]
    fn strip_handles_separators_and_ideographic_space() {
        assert_eq!(py_strip("\x1fa\u{3000}"), "a");
        assert_eq!(py_strip("  a b \t\n"), "a b");
    }

    #[test]
    fn space_predicate() {
        assert!(is_py_space('\x1c'));
        assert!(is_py_space(' '));
        assert!(!is_py_space('a'));
    }

    #[test]
    fn universal_newlines_without_cr_borrows() {
        let text = "a\nb\u{2028}c";
        assert!(matches!(universal_newlines(text), Cow::Borrowed(t) if t == text));
        assert!(matches!(universal_newlines(""), Cow::Borrowed("")));
    }

    #[test]
    fn universal_newlines_translates_crlf_and_lone_cr() {
        assert_eq!(universal_newlines("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(universal_newlines("a\rb\r"), "a\nb\n");
        assert_eq!(universal_newlines("\r\r\n\n\r"), "\n\n\n\n");
        assert_eq!(universal_newlines("\r\n\r\n"), "\n\n");
        assert_eq!(universal_newlines("\u{e9}\r\u{65e5}"), "\u{e9}\n\u{65e5}");
    }

    #[test]
    fn universal_newlines_leaves_other_separators() {
        assert_eq!(
            universal_newlines("a\rb\x0bc\x0cd\u{85}e"),
            "a\nb\x0bc\x0cd\u{85}e"
        );
    }

    #[hegel::test]
    fn universal_newlines_matches_python_replace(tc: hegel::TestCase) {
        use hegel::generators;
        // Mostly line-ending pieces, so `\r\n` pairs and lone `\r`s are common.
        let pieces = vec!["\r", "\n", "\r\n", "a", "\u{e9}", "\u{2028}"];
        let count = tc.draw(generators::integers::<usize>().max_value(20));
        let mut text = String::new();
        for _ in 0..count {
            let piece: &str = tc.draw(generators::sampled_from(&pieces));
            text.push_str(piece);
        }
        text.push_str(&tc.draw(generators::text()));
        let normalized = universal_newlines(&text);
        assert_eq!(normalized, text.replace("\r\n", "\n").replace('\r', "\n"));
        assert!(!normalized.contains('\r'));
    }

    #[test]
    fn one_sided_strips() {
        assert_eq!(py_rstrip(" a \x1f"), " a");
        assert_eq!(py_lstrip("\x1f a "), "a ");
    }
}
