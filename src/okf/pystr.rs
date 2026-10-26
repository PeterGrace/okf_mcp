//! Python string semantics shared by the OKF parsers.
//!
//! Rust's `str::trim` and the `regex` crate's classes differ from Python's in
//! edge cases, and the ports must match `okf.py` exactly. For regexes: Python's
//! `\w` is `[\p{L}\p{N}_]` and Python's `\s` is `[\s\x1c-\x1f]`, whereas the
//! Rust `regex` classes differ, so ported patterns must spell these classes out.

/// Python's `str.isspace()`: Unicode White_Space plus U+001C..U+001F, which
/// Rust's `char::is_whitespace` leaves out.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn one_sided_strips() {
        assert_eq!(py_rstrip(" a \x1f"), " a");
        assert_eq!(py_lstrip("\x1f a "), "a ");
    }
}
