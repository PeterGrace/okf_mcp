//! Python string semantics shared by the OKF parsers.
//!
//! Rust's `str::trim` and the `regex` crate's classes differ from Python's in
//! edge cases, and the ports must match `okf.py` exactly. For regexes: Python's
//! `\w` is `[\p{L}\p{N}_]` and Python's `\s` is `[\s\x1c-\x1f]`, whereas the
//! Rust `regex` classes differ, so ported patterns must spell these classes out.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn one_sided_strips() {
        assert_eq!(py_rstrip(" a \x1f"), " a");
        assert_eq!(py_lstrip("\x1f a "), "a ");
    }
}
