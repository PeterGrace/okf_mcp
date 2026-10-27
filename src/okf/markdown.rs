//! Markdown structure the catalog tools need: frontmatter/body splits, ATX
//! headings and their GitHub anchor slugs, HTML anchors, footnotes, and
//! links with their resolution to bundle paths.
//!
//! Port of the corresponding helpers in `tools/okf.py`. Everything uses the
//! linear-time `regex` crate; the two places where Python's patterns use
//! lookaround (`LINK_RE`'s `(?<!!)` and `slugify`'s underscore rule) are
//! done by hand so no input can trip a backtracking limit.
//!
//! # Line endings
//!
//! `okf.py` reads files for markdown analysis in Python text mode, which turns
//! `\r\n` and lone `\r` into `\n` (universal newlines). The functions here
//! take text as given, so callers must read it the same way: through
//! [`Workspace::read_text`], or
//! [`universal_newlines`](crate::okf::pystr::universal_newlines) for text from
//! elsewhere. Raw `\r\n` text would make `## Closing ##\r` the heading
//! `Closing ##` (slug `closing-`) where `okf.py` sees `Closing`. Frontmatter
//! editing ([`crate::okf::doc`]) stays byte-preserving and never normalizes.
//!
//! # Known divergences from `okf.py`
//!
//! * **`resolve_link` directory detection is lexical.** Python asks the OS
//!   about an unnormalized path. Here `"/a/zz/../b"` gives `a/b/index.md`
//!   (Python: `a/b` when `zz` does not exist), `"/infra/x.md/.."` gives
//!   `infra/index.md` (Python: `infra`), and `".."` from `y.md` gives `..`
//!   (Python: `../index.md`). Later broken-link code must treat
//!   `escapes_root` paths explicitly.
//! * **Unicode tables differ.** Python 3.14 (Unicode 16) and the `regex`
//!   crate classify about 55 code points differently, which can change a
//!   slug; this cannot be fixed in code.
//! * **`headings` assumes its lines contain no `\n`.**

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::okf::paths::{dirname, join, normpath};
use crate::okf::pystr::{is_py_word, py_rstrip, py_strip};
use crate::okf::workspace::Workspace;

/// A markdown link `[text](target "title")`, not an image (`![...]`).
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:\A|[^!])(\[((?:[^\]\\]|\\.)*)\]\(([^)\s\x1c-\x1f]+)(?:[\s\x1c-\x1f]+"[^"]*")?\))"#,
    )
    .expect("LINK regex is valid")
});
static HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$").expect("HEADING regex is valid")
});
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\s\x1c-\x1f]{0,3}(```|~~~)").expect("FENCE regex is valid"));
static FOOTNOTE_DEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\^([^\]]+)\]:").expect("FOOTNOTE_DEF regex is valid"));
static HTML_ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<a[\s\x1c-\x1f][^>]*(?:name|[i\u{130}\u{131}]d)=["']([^"']+)["']"#)
        .expect("HTML_ANCHOR regex is valid")
});
static URI_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[a-zA-Z\u{130}\u{131}][a-zA-Z0-9+.\-\u{130}\u{131}]*:")
        .expect("URI_SCHEME regex is valid")
});
static SLUG_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!?\[((?:[^\]\\]|\\.)*)\]\([^)]*\)").expect("SLUG_LINK regex is valid")
});
static SLUG_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("SLUG_TAG regex is valid"));
static SLUG_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}_\- ]").expect("SLUG_PUNCTUATION regex is valid"));

/// One link occurrence in a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMatch {
    /// The link text between the brackets.
    pub text: String,
    /// The raw target between the parentheses.
    pub target: String,
    /// Byte range of the whole `[text](target)` in the line.
    pub range: std::ops::Range<usize>,
    /// Byte range of the target within the line.
    pub target_range: std::ops::Range<usize>,
}

/// Finds every markdown link (not image) in `text`, left to right.
///
/// All ranges are byte offsets into `text` (which may span lines), lie on
/// `char` boundaries, and are safe to pass to `String::replace_range`.
///
/// # Examples
/// ```
/// use okf_mcp::okf::markdown::find_links;
/// let links = find_links("See [the server](/infra/x.md#ports) and ![img](a.png).");
/// assert_eq!(links.len(), 1);
/// assert_eq!(links[0].target, "/infra/x.md#ports");
/// ```
pub fn find_links(text: &str) -> Vec<LinkMatch> {
    // Python's `(?<!!)` means: the char before `[` is not `!`. `regex` has no
    // lookbehind, so LINK consumes that char instead (`[^!]`, or `\A` at the
    // text start) and the real link is group 1. Because a rejected `![` simply
    // fails to match, the scan stays linear. After a match we resume at its
    // closing `)` (one byte, never `!`), so an adjacent link `[a](b)[c](d)`
    // can use that `)` as its preceding char, as in Python.
    let mut links = Vec::new();
    let mut pos = 0;
    // `captures_at` searches from `pos` but still sees the text before it, so
    // `\A` matches only at the true start of `text`.
    while let Some(caps) = LINK.captures_at(text, pos) {
        let (Some(whole), Some(label), Some(target)) = (caps.get(1), caps.get(2), caps.get(3))
        else {
            break; // unreachable: groups 1-3 are mandatory
        };
        links.push(LinkMatch {
            text: label.as_str().to_string(),
            target: target.as_str().to_string(),
            range: whole.range(),
            target_range: target.range(),
        });
        pos = whole.end() - 1;
    }
    links
}

/// Splits a document's text into `(frontmatter, body, number of lines before the body)`.
///
/// ```
/// use okf_mcp::okf::markdown::split_frontmatter;
/// assert_eq!(split_frontmatter("---\na: 1\n---\nbody"), ("a: 1", "body", 3));
/// ```
///
/// Port of `okf.py`'s `split_frontmatter`: frontmatter needs a `---\n`
/// first line and a later line starting with `---`.
pub fn split_frontmatter(text: &str) -> (&str, &str, usize) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---").map(|p| p + 4) {
            let body_start = text[end + 4..]
                .find('\n')
                .map_or(text.len(), |p| end + 4 + p + 1);
            let offset = text[..body_start].matches('\n').count();
            return (&text[4..end], &text[body_start..], offset);
        }
    }
    ("", text, 0)
}

/// Splits a document's text on `\n` into (frontmatter lines with delimiters, body lines).
///
/// Port of `okf.py`'s `body_split`. Without frontmatter the first part is empty.
///
/// ```
/// use okf_mcp::okf::markdown::body_split;
/// assert_eq!(body_split("---\na: 1\n---\nx"), (vec!["---", "a: 1", "---"], vec!["x"]));
/// ```
pub fn body_split(text: &str) -> (Vec<&str>, Vec<&str>) {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first().map(|l| l.trim_end_matches('\r')) == Some("---") {
        if let Some(j) = (1..lines.len())
            .find(|&j| matches!(py_rstrip(lines[j].trim_end_matches('\r')), "---" | "..."))
        {
            return (lines[..=j].to_vec(), lines[j + 1..].to_vec());
        }
    }
    (Vec::new(), lines)
}

/// An ATX heading found in a list of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Index of the heading's line.
    pub index: usize,
    /// 1 to 6.
    pub level: usize,
    /// The heading text, trimmed, without closing `#`s.
    pub text: String,
    /// Its GitHub anchor slug, numbered for repeats (`x`, `x-1`, `x-2`).
    pub slug: String,
}

/// The ATX headings in `lines`, skipping fenced code, with their anchor slugs.
pub fn headings(lines: &[&str]) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut fence: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(caps) = FENCE.captures(line) {
            let marker = caps.get(1).map_or("", |m| m.as_str());
            // A fence closes only with the same marker that opened it.
            fence = match fence {
                Some(open) if open == marker => None,
                Some(open) => Some(open),
                None => Some(marker),
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some(caps) = HEADING.captures(line) {
            // Indexing is safe: group 1 (the `#`s) is mandatory in HEADING; indexing a
            // group that did not participate would panic.
            let level = caps[1].len();
            let text = caps.get(2).map_or("", |m| py_strip(m.as_str())).to_string();
            found.push(Heading {
                index,
                level,
                text,
                slug: String::new(),
            });
        }
    }
    // Only looked up, never iterated, so plain hashing is enough.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for heading in &mut found {
        let base = slugify(&heading.text);
        let slug = match seen.get(&base) {
            Some(count) => format!("{base}-{count}"),
            None => base.clone(),
        };
        *seen.entry(base).or_insert(0) += 1;
        seen.entry(slug.clone()).or_insert(1);
        heading.slug = slug;
    }
    found
}

/// GitHub's heading anchor: rendered text, lowercased, punctuation dropped,
/// spaces turned into `-`.
///
/// # Examples
/// ```
/// use okf_mcp::okf::markdown::slugify;
/// assert_eq!(slugify("What it *wouldn't* change"), "what-it-wouldnt-change");
/// assert_eq!(slugify("Use `okf.py` [here](/x.md)"), "use-okfpy-here");
/// assert_eq!(slugify("snake_case stays"), "snake_case-stays");
/// ```
pub fn slugify(text: &str) -> String {
    let text = SLUG_LINK.replace_all(text, "$1");
    let text = SLUG_TAG.replace_all(&text, "");
    // Only allocate the backtick-free copy when there is a backtick.
    let text = if text.contains('`') {
        Cow::Owned(text.replace('`', ""))
    } else {
        text
    };
    let text = strip_emphasis(&text);
    let lowered = py_strip(&text).to_lowercase();
    SLUG_PUNCTUATION.replace_all(&lowered, "").replace(' ', "-")
}

/// Removes emphasis markers the way `slugify`'s Python pattern
/// `\*+|(?<!\w)_+|_+(?!\w)` does, in one linear pass.
///
/// Every `*` goes. A run of `_` goes whole unless it sits between two word
/// characters (so `snake_case` keeps its underscore). Neighbours are judged in
/// the original text, so `a*_b` loses both.
fn strip_emphasis(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    let mut previous: Option<char> = None;
    while let Some((start, c)) = chars.next() {
        match c {
            '*' => {}
            '_' => {
                let mut end = start + 1;
                while let Some(&(at, '_')) = chars.peek() {
                    end = at + 1;
                    chars.next();
                }
                let next = text[end..].chars().next();
                let between_words =
                    previous.is_some_and(is_py_word) && next.is_some_and(is_py_word);
                if between_words {
                    out.push_str(&text[start..end]);
                }
                previous = Some('_');
                continue;
            }
            _ => out.push(c),
        }
        previous = Some(c);
    }
    out
}

/// Every fragment a link may target in a document: heading slugs plus HTML
/// `<a name|id>` anchors.
pub fn anchors_in(text: &str) -> BTreeSet<String> {
    let (_, body, _) = split_frontmatter(text);
    let lines: Vec<&str> = body.split('\n').collect();
    let mut anchors: BTreeSet<String> = headings(&lines).into_iter().map(|h| h.slug).collect();
    anchors.extend(HTML_ANCHOR.captures_iter(body).map(|c| c[1].to_string()));
    anchors
}

/// Footnote definitions: label to the line indexes of its definition and
/// indented continuation lines, in first-definition order.
pub fn footnote_defs(lines: &[&str]) -> IndexMap<String, Vec<usize>> {
    let mut defs: IndexMap<String, Vec<usize>> = IndexMap::new();
    let mut current: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(caps) = FOOTNOTE_DEF.captures(line) {
            // Indexing is safe: group 1 is mandatory in FOOTNOTE_DEF.
            let label = caps[1].to_string();
            defs.entry(label.clone()).or_default().push(index);
            current = Some(label);
        } else if current.is_some()
            && (line.starts_with("    ") || line.starts_with('\t'))
            && !py_strip(line).is_empty()
        {
            if let Some(lines) = current.as_deref().and_then(|label| defs.get_mut(label)) {
                lines.push(index);
            }
        } else {
            current = None;
        }
    }
    defs
}

/// Where a link points inside the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    /// Bundle-relative target path (a directory link becomes its `index.md`).
    pub path: String,
    /// Was the link written bundle-absolute (`/dir/x.md`)?
    pub absolute: bool,
}

/// Resolves a link target written in `source` to a bundle path.
///
/// # Arguments
/// * `workspace` - the catalog, to tell directories from files.
/// * `link` - the raw link target, possibly with a `#fragment`.
/// * `source` - bundle-relative path of the document containing the link.
///
/// # Returns
/// `None` for external links (any URI scheme) and pure `#fragment` links.
pub fn resolve_link(workspace: &dyn Workspace, link: &str, source: &str) -> Option<ResolvedLink> {
    let target = link.split('#').next().unwrap_or_default();
    if target.is_empty() || URI_SCHEME.is_match(target) {
        return None;
    }
    let absolute = target.starts_with('/');
    let mut path = if absolute {
        target.trim_start_matches('/').to_string()
    } else {
        normpath(&join(dirname(source), target))
    };
    if target.ends_with('/') || workspace.is_dir(&path) {
        path = join(&path, "index.md");
    }
    Some(ResolvedLink {
        path: normpath(&path),
        absolute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    #[test]
    fn crlf_text_read_for_analysis_matches_okf_py() {
        let raw =
            "---\r\nk: v\r\n---\r\n## Closing ##\r\n```\r\n# no\r\n```\r\n[^n]: a\r\n    b\r\n";
        let workspace = MemWorkspace::from_files([("crlf.md", raw)]);
        let text = workspace.read_text("crlf.md").expect("readable");
        let (_, body) = body_split(&text);
        let found = headings(&body);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].text.as_str(), found[0].slug.as_str()),
            ("Closing", "closing")
        );
        assert_eq!(footnote_defs(&body).get("n"), Some(&vec![4, 5]));
    }

    #[test]
    fn find_links_skips_images_and_keeps_inner_links() {
        let links = find_links("![a [b](c)](d) and [e](f \"title\") [g\\]h](i)");
        let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["c", "f", "i"]);
        assert_eq!(links[2].text, "g\\]h");
        let line = "x [t](/a.md) y";
        let link = &find_links(line)[0];
        assert_eq!(&line[link.range.clone()], "[t](/a.md)");
        assert_eq!(&line[link.target_range.clone()], "/a.md");
    }

    #[test]
    fn split_frontmatter_cases() {
        assert_eq!(
            split_frontmatter("---\na: 1\n---\nbody\n"),
            ("a: 1", "body\n", 3)
        );
        assert_eq!(split_frontmatter("---\na: 1\n---"), ("a: 1", "", 2));
        assert_eq!(split_frontmatter("no fm\n"), ("", "no fm\n", 0));
    }

    #[test]
    fn body_split_cases() {
        let (head, body) = body_split("---\na: 1\n...\nbody");
        assert_eq!(head, ["---", "a: 1", "..."]);
        assert_eq!(body, ["body"]);
        let (head, body) = body_split("plain\ntext");
        assert!(head.is_empty());
        assert_eq!(body, ["plain", "text"]);
    }

    #[test]
    fn headings_skip_fences_and_number_repeats() {
        let lines = [
            "# Intro",
            "```",
            "# not a heading",
            "~~~",
            "# still code",
            "```",
            "## Intro ##",
            "### Intro",
            "#nospace",
            "####### seven",
        ];
        let found = headings(&lines);
        let summary: Vec<_> = found
            .iter()
            .map(|h| (h.index, h.level, h.slug.as_str()))
            .collect();
        assert_eq!(
            summary,
            [(0, 1, "intro"), (6, 2, "intro-1"), (7, 3, "intro-2")]
        );
        assert_eq!(found[1].text, "Intro");
    }

    #[test]
    fn repeat_numbering_avoids_existing_slugs() {
        let found = headings(&["# x-1", "# x", "# x"]);
        let slugs: Vec<_> = found.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(slugs, ["x-1", "x", "x-1"]);
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(
            slugify("Plan: CLI first, MCP later"),
            "plan-cli-first-mcp-later"
        );
        assert_eq!(slugify("<em>Tagged</em> heading"), "tagged-heading");
        assert_eq!(slugify("__dunder__ name"), "dunder-name");
        assert_eq!(slugify("Ünïcode Wörds"), "ünïcode-wörds");
    }

    #[test]
    fn anchors_include_html() {
        let anchors =
            anchors_in("---\nt: x\n---\n# Top\n<a name=\"custom\"></a>\n<A ID='Other'>\n");
        assert_eq!(
            anchors.into_iter().collect::<Vec<_>>(),
            ["Other", "custom", "top"]
        );
    }

    #[test]
    fn footnotes_with_continuations() {
        let lines = [
            "Text[^a].",
            "",
            "[^a]: first",
            "    more",
            "[^b]: other",
            "not indented",
            "\tlate",
        ];
        let defs = footnote_defs(&lines);
        assert_eq!(defs["a"], [2, 3]);
        assert_eq!(defs["b"], [4]);
        assert_eq!(defs.len(), 2);
    }

    #[test]
    fn resolve_link_cases() {
        let ws = MemWorkspace::from_files([("infra/index.md", ""), ("infra/x.md", "")]);
        let resolve = |link: &str, src: &str| resolve_link(&ws, link, src);
        assert_eq!(
            resolve("/infra/x.md#a", "p/q.md"),
            Some(ResolvedLink {
                path: "infra/x.md".into(),
                absolute: true
            })
        );
        assert_eq!(
            resolve("x.md", "infra/index.md"),
            Some(ResolvedLink {
                path: "infra/x.md".into(),
                absolute: false
            })
        );
        assert_eq!(
            resolve("../infra/", "people/y.md").unwrap().path,
            "infra/index.md"
        );
        assert_eq!(resolve("/infra", "y.md").unwrap().path, "infra/index.md");
        assert_eq!(resolve("https://example.com", "y.md"), None);
        assert_eq!(resolve("mailto:a@b", "y.md"), None);
        assert_eq!(resolve("#frag", "y.md"), None);
    }

    #[test]
    fn slugify_follows_python_word_characters() {
        // `²` is a Python word char (category No); a combining mark is not.
        assert_eq!(slugify("a²b"), "a²b");
        assert_eq!(slugify("e\u{301}x"), "ex");
        assert_eq!(slugify("\u{1f}a b\u{1f}"), "a-b");
        assert_eq!(slugify("²_x"), "²_x");
        assert_eq!(slugify("a_\u{301}"), "a");
    }

    #[test]
    fn python_whitespace_extras_count_as_space() {
        let found = headings(&[
            "# \u{1f}pad\u{1f}",
            "\u{1c}```",
            "# in",
            "\u{1c}```",
            "# out",
        ]);
        let summary: Vec<_> = found.iter().map(|h| (h.index, h.text.as_str())).collect();
        assert_eq!(summary, [(0, "pad"), (4, "out")]);
        let (head, body) = body_split("---\na: 1\n---\u{1f}\nb");
        assert_eq!(head, ["---", "a: 1", "---\u{1f}"]);
        assert_eq!(body, ["b"]);
        let links = find_links("[a](b\u{1f}c) [d](e\u{1f}\"t\")");
        let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["e"]);
        assert!(anchors_in("<a\u{1f}name=\"zz\">").contains("zz"));
        let defs = footnote_defs(&["[^a]: x", "    \u{1f}", "    more"]);
        assert_eq!(defs["a"], [0]);
    }

    #[test]
    fn pathological_inputs_neither_panic_nor_drop_later_links() {
        // Python finds exactly the trailing link here (checked at 1000 reps).
        let text = format!("{}[ok](x)", "[a](b ".repeat(100_000));
        let targets: Vec<_> = find_links(&text).into_iter().map(|l| l.target).collect();
        assert_eq!(targets, ["x"]);
        // Python: slugify("__a" * n) == "a" + "__a" * (n - 1).
        let expected = format!("a{}", "__a".repeat(99_999));
        assert_eq!(slugify(&"__a".repeat(100_000)), expected);
    }

    #[test]
    fn many_rejected_image_openers_stay_linear() {
        let text = format!("{}[ok](t.md)", "![".repeat(20_000));
        let links = find_links(&text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "t.md");
        let adjacent = find_links("[a](b)[c](d)");
        assert_eq!(adjacent.len(), 2);
    }

    #[test]
    fn target_range_with_title_and_multibyte_prefix() {
        // Python's character spans (checked) are starts 10 and 27, targets 14..19 and 31..32;
        // `\u{e9}` and `\u{fc}` add one byte each to the byte offsets below.
        let line = "\u{e9} ![x](y) [t](/a.md \"ti\") \u{fc}[k](z)";
        let links = find_links(line);
        assert_eq!(links.len(), 2);
        assert_eq!(
            (links[0].range.start, links[0].target_range.clone()),
            (11, 15..20)
        );
        assert_eq!(&line[links[0].target_range.clone()], "/a.md");
        assert_eq!(&line[links[0].range.clone()], "[t](/a.md \"ti\")");
        assert_eq!(
            (links[1].range.start, links[1].target_range.clone()),
            (29, 33..34)
        );
    }

    #[test]
    fn unclosed_fence_hides_later_headings() {
        let found = headings(&["# a", "```", "# b", "## c"]);
        let texts: Vec<_> = found.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["a"]);
    }

    #[test]
    fn slugify_emphasis_table_matches_python() {
        // Expected values come from okf.py's slugify.
        let table = [
            ("_a_", "a"),
            ("a__b", "a__b"),
            ("__init__", "init"),
            ("*a*_b_", "ab"),
            ("a_*_b", "ab"),
            ("a*_b", "ab"),
            ("x_ y", "x-y"),
            ("_", ""),
            ("__", ""),
            ("\u{b2}_x", "\u{b2}_x"),
            ("\u{e9}_", "\u{e9}"),
            ("a_\u{301}", "a"),
            ("`c_d`", "c_d"),
            ("[l_k](u)", "l_k"),
            ("<i>_t_</i>", "t"),
            ("snake_case_", "snake_case"),
        ];
        for (input, expected) in table {
            assert_eq!(slugify(input), expected, "{input:?}");
        }
    }

    #[test]
    fn dotless_and_dotted_i_fold_like_python() {
        let ws = MemWorkspace::from_files([("a.md", "")]);
        // Python: resolve_link("\u{131}map:x") is external, as are the others.
        for link in ["\u{131}map:x", "\u{130}map:x", "\u{212a}ey:x", "\u{17f}:x"] {
            assert_eq!(resolve_link(&ws, link, "y.md"), None, "{link:?}");
        }
        // Python: both are anchors.
        assert!(anchors_in("<a \u{131}d=\"m\">").contains("m"));
        assert!(anchors_in("<a \u{130}d=\"m\">").contains("m"));
    }

    #[hegel::test]
    fn link_ranges_are_consistent(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let wrapped = format!(
            "[\u{e9}]({}) !{text}[a](b) \u{fc}[c](d \"t\")",
            text.replace(')', "")
        );
        for link in find_links(&wrapped) {
            assert_eq!(&wrapped[link.target_range.clone()], link.target);
            assert!(link.range.start < link.target_range.start);
            assert!(link.target_range.end < link.range.end);
        }
    }

    #[hegel::test]
    fn slugify_output_has_no_spaces_or_uppercase_ascii(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        let slug = slugify(&text);
        assert!(!slug.contains(' '));
        assert!(!slug.chars().any(|c| c.is_ascii_uppercase()));
    }
}
