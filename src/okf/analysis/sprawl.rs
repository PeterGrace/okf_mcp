//! `okf.py sprawl --json`: which concepts should be split for progressive
//! disclosure (`VOLATILE_RE`, `DATE_RE`, `LEADIN_RE`, `volatile_reason`,
//! `sprawl_of` and `cmd_sprawl`).
//!
//! score = lines/20 + sections/2 + sources/3 + 1.5 per volatile heading or
//! lead-in + distinct dates/5, rounded to one decimal. Lines are non-blank
//! body lines, sections are headings, and dates are counted outside fenced
//! code. Concepts of an exempt type (`Backlog`) grow by design: they are left
//! out of the ranking but reported when named.
//!
//! The ported regexes emulate Python's `\b` with consumed neighbours and widen
//! `i` for `re.I` (the plan's verified facts 6 and 7); every expectation in
//! the tests was taken from `okf.py`.
//!
//! The score adds its parts the way Python 3.12+'s `sum()` does (with
//! compensation; see `py_float_sum`), so parity assumes `okf.py` runs on
//! Python 3.12 or later: older versions add naively and can round a score
//! differently (`3.15` vs `3.1500000000000004`).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::LazyLock;

use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;

use crate::error::OkfError;
use crate::okf::analysis::oldest::round_one;
use crate::okf::markdown::{body_split, fence_marker, headings, next_fence};
use crate::okf::pystr::py_strip;
use crate::okf::snapshot::{DocKind, Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// `SPRAWL_WARN`: `lint` warns above this score.
pub const SPRAWL_WARN: f64 = 16.0;

/// `SPRAWL_EXEMPT_TYPES`: concept types (stripped, lowercased) that grow by
/// design (a list of entries, not a hub).
const EXEMPT_TYPES: [&str; 1] = ["backlog"];

/// One heading's section, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SprawlSection {
    /// Heading text.
    pub heading: String,
    /// 1 to 6.
    pub level: usize,
    /// 1-based file line.
    pub line: usize,
    /// Non-blank lines until the next heading of the same or a higher level.
    pub lines: usize,
    /// Why the heading reads as volatile, if it does.
    pub volatile: Option<String>,
}

/// A volatile heading or bold lead-in, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolatileItem {
    /// `heading` or `lead-in`.
    pub kind: &'static str,
    /// The heading or lead-in text.
    pub text: String,
    /// 1-based file line.
    pub line: usize,
    /// The matched volatile word (lowercased), or the date as written.
    pub why: String,
}

/// One concept's sprawl report, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SprawlReport {
    /// `/`-prefixed path.
    pub path: String,
    /// The score.
    pub score: f64,
    /// The concept's `type` as written when it is exempt (e.g. `Backlog`).
    pub exempt: Option<String>,
    /// Non-blank body lines.
    pub lines: usize,
    /// Headings.
    pub sections: usize,
    /// Entries in `sources` (0 unless it is a list).
    pub sources: usize,
    /// Volatile headings and lead-ins.
    pub volatile: usize,
    /// Distinct dates outside fenced code, compared lowercased.
    pub dates: usize,
    /// Every section, in file order.
    pub sections_detail: Vec<SprawlSection>,
    /// Every volatile heading, then every volatile lead-in, in file order.
    pub volatile_detail: Vec<VolatileItem>,
    /// Split candidates: volatile sections first, then large ones, each
    /// group largest first.
    pub candidates: Vec<SprawlSection>,
}

/// What `sprawl` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SprawlQuery<'a> {
    /// One concept (exempt or not), instead of the top `n`.
    pub concept: Option<&'a str>,
    /// How many of the highest scores (exempt concepts excluded).
    pub n: usize,
}

impl Default for SprawlQuery<'_> {
    /// `okf.py`'s defaults: `-n 5`, no concept.
    fn default() -> Self {
        Self {
            concept: None,
            n: 5,
        }
    }
}

/// Python's `\w` class, for the emulated `\b`.
const WORD: &str = r"\p{L}\p{N}_";

/// `VOLATILE_RE`'s alternatives. Each `i` is widened to `[iI\u{130}\u{131}]`
/// when the regex is built (verified fact 6).
const VOLATILE_WORDS: &str = "as of|current state|status|outage|incident|investigation|\
    post-?mortem|change ?log|changes since|changed|open (?:items|questions)|traps?|gotchas?|\
    fix(?:ed|es)?|workaround|stuck|broken|to ?do|snapshot|history|timeline|procedure|runbook|\
    to check";

/// `VOLATILE_RE`: `\b(WORDS)\b`, case-insensitive. Every alternative starts
/// and ends with a word character, so each `\b` becomes a consumed non-word
/// neighbour (or the haystack's edge) and the word is capture group 1.
// `LazyLock` compiles the regex on first use, like a module-level
// `re.compile` in Python.
static VOLATILE: LazyLock<Regex> = LazyLock::new(|| {
    let words = VOLATILE_WORDS.replace('i', "[iI\u{130}\u{131}]");
    Regex::new(&format!(r"(?:\A|[^{WORD}])(?i:({words}))(?:[^{WORD}]|\z)"))
        .expect("VOLATILE regex is valid")
});

/// `DATE_RE`: `2026-10-05`, `2026-10`, or `Oct 5, 2026` / `October 2026`,
/// case-insensitive, in capture group 1. Python's `[a-z]` under `re.I` also
/// matches U+0130 and U+0131, so they are added (no month name has an `i`).
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    // `{{` and `}}` are literal braces in a `format!` string.
    Regex::new(&format!(
        concat!(
            r"(?:\A|[^{w}])(?i:((?:19|20)\d{{2}}-[01]\d(?:-[0-3]\d)?|",
            r"(?:jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z\u{{130}}\u{{131}}]*\.? ",
            r"(?:\d{{1,2}},? )?(?:19|20)\d{{2}}))(?:[^{w}]|\z)"
        ),
        w = WORD
    ))
    .expect("DATE regex is valid")
});

/// `LEADIN_RE`: a bold lead-in at the start of a line, its text in group 1.
static LEADIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\*\*([^*]{2,80}?)\*\*").expect("LEADIN regex is valid"));

/// `volatile_reason`: the first volatile word, lowercased, else the first
/// date as written.
fn volatile_reason(text: &str) -> Option<String> {
    if let Some(found) = VOLATILE.captures(text) {
        // Indexing group 1 is safe: it is mandatory in the pattern.
        return Some(found[1].to_lowercase());
    }
    DATE.captures(text).map(|found| found[1].to_string())
}

/// `DATE_RE.findall(line)`: non-overlapping dates, left to right.
fn dates_in(line: &str) -> impl Iterator<Item = &str> {
    let mut pos = 0;
    // `from_fn` builds an iterator from a closure that returns the next item
    // (like a Python generator); `move` hands `pos` to the closure.
    std::iter::from_fn(move || {
        let found = DATE.captures_at(line, pos)?.get(1)?;
        // Resume at the date's end, not the match's: the consumed trailing
        // boundary character can be the next match's leading one.
        pos = found.end();
        Some(found.as_str())
    })
}

/// Python's `sum()` of floats, which since 3.12 adds with Neumaier
/// compensation (CPython's `builtin_sum_impl`), so it can differ from a
/// plain left-to-right `+`: `1.05 + 0.5 + 1.0 + 0.0 + 0.6000000000000001`
/// is `3.15` added naively but `3.1500000000000004` in Python, which rounds
/// to `3.2`.
fn py_float_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let (mut total, mut compensation) = (0.0_f64, 0.0_f64);
    for value in values {
        let next = total + value;
        // The low-order bits lost by this addition, from whichever operand
        // is larger in magnitude.
        compensation += if total.abs() >= value.abs() {
            (total - next) + value
        } else {
            (value - next) + total
        };
        total = next;
    }
    // CPython adds the compensation only when it is non-zero and finite.
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

/// Non-blank lines (Python's `l.strip()` is truthy).
fn non_blank(lines: &[&str]) -> usize {
    lines.iter().filter(|l| !py_strip(l).is_empty()).count()
}

/// Which body lines are fenced code, the fence lines included.
fn code_lines(body: &[&str]) -> Vec<bool> {
    let mut fence: Option<&str> = None;
    body.iter()
        .map(|line| {
            let marker = fence_marker(line);
            let in_code = marker.is_some() || fence.is_some();
            if let Some(marker) = marker {
                fence = next_fence(fence, marker);
            }
            in_code
        })
        .collect()
}

/// `sprawl_of`: one concept's report.
///
/// # Arguments
/// * `doc` - the concept; unparseable frontmatter counts as empty, as in
///   `okf.py`.
///
/// # Returns
/// The report `okf.py` builds for it, line numbers counted in the file.
pub fn sprawl_of(doc: &SnapshotDoc) -> SprawlReport {
    let (head, body) = body_split(doc.text());
    let data = doc.frontmatter().data();
    let sources = match data.and_then(|d| d.get("sources")) {
        Some(Value::Seq(items)) => items.len(),
        _ => 0,
    };
    let exempt = data
        .and_then(|d| d.get("type"))
        .and_then(Value::as_str)
        .filter(|t| EXEMPT_TYPES.contains(&py_strip(t).to_lowercase().as_str()))
        .map(str::to_string);

    let code = code_lines(&body);
    let lines = non_blank(&body);
    let dates: HashSet<String> = body
        .iter()
        .zip(&code)
        .filter(|&(_, &in_code)| !in_code)
        .flat_map(|(line, _)| dates_in(line))
        .map(str::to_lowercase)
        .collect();

    // `okf.py` adds the lines before the body (`head.len()`) and 1 to each
    // 0-based body index.
    let offset = head.len() + 1;
    let found = headings(&body);
    let mut sections = Vec::with_capacity(found.len());
    let mut volatile = Vec::new();
    for (k, heading) in found.iter().enumerate() {
        let end = found[k + 1..]
            .iter()
            .find(|h| h.level <= heading.level)
            .map_or(body.len(), |h| h.index);
        let why = volatile_reason(&heading.text);
        if let Some(why) = &why {
            volatile.push(VolatileItem {
                kind: "heading",
                text: heading.text.clone(),
                line: heading.index + offset,
                why: why.clone(),
            });
        }
        sections.push(SprawlSection {
            heading: heading.text.clone(),
            level: heading.level,
            line: heading.index + offset,
            lines: non_blank(&body[heading.index + 1..end]),
            volatile: why,
        });
    }
    for (index, line) in body.iter().enumerate() {
        if code[index] {
            continue;
        }
        // `let ... else` binds on a match and runs the `else` block otherwise.
        let Some(lead_in) = LEADIN.captures(line).and_then(|c| c.get(1)) else {
            continue;
        };
        if let Some(why) = volatile_reason(lead_in.as_str()) {
            volatile.push(VolatileItem {
                kind: "lead-in",
                text: py_strip(lead_in.as_str()).to_string(),
                line: index + offset,
                why,
            });
        }
    }

    // `okf.py`'s weights and arithmetic: weight * count, in `parts` order,
    // added by Python's `sum`.
    let score = round_one(py_float_sum([
        (1.0 / 20.0) * lines as f64,
        (1.0 / 2.0) * found.len() as f64,
        (1.0 / 3.0) * sources as f64,
        1.5 * volatile.len() as f64,
        (1.0 / 5.0) * dates.len() as f64,
    ]));
    let large = 12.max(lines / 5);
    let mut candidates: Vec<SprawlSection> = sections
        .iter()
        .filter(|s| s.volatile.is_some() || s.lines >= large)
        .cloned()
        .collect();
    // A stable sort on okf.py's key (-bool(volatile), -lines): volatile first,
    // then the most lines; ties keep file order.
    candidates.sort_by(|a, b| {
        b.volatile
            .is_some()
            .cmp(&a.volatile.is_some())
            .then(b.lines.cmp(&a.lines))
    });
    SprawlReport {
        path: format!("/{}", doc.path()),
        score,
        exempt,
        lines,
        sections: found.len(),
        sources,
        volatile: volatile.len(),
        dates: dates.len(),
        sections_detail: sections,
        volatile_detail: volatile,
        candidates,
    }
}

/// `cmd_sprawl`: the named concept, or the `n` highest-scoring non-exempt
/// concepts (ties by path).
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `query` - one concept, or how many.
///
/// # Returns
/// The rows `okf.py sprawl --json` prints, in its order.
///
/// # Errors
/// From [`Snapshot::resolve_concept`] when `query.concept` names no concept
/// ([`OkfError::NotFound`]) or a document that is not one
/// ([`OkfError::InvalidPath`]).
pub fn sprawl(snapshot: &Snapshot, query: SprawlQuery<'_>) -> Result<Vec<SprawlReport>, OkfError> {
    if let Some(reference) = query.concept {
        let path = snapshot.resolve_concept(reference)?;
        let doc = snapshot.get(&path).ok_or(OkfError::NotFound { path })?;
        return Ok(vec![sprawl_of(doc)]);
    }
    // `par_iter` scores the concepts on a thread pool. Dropping exempt rows
    // before the sort keeps the same rows as `okf.py` dropping them after.
    let mut rows: Vec<SprawlReport> = snapshot
        .docs()
        .par_iter()
        .filter(|doc| doc.kind() == DocKind::Concept)
        .map(sprawl_of)
        .filter(|row| row.exempt.is_none())
        .collect();
    // Highest score first, then path: a total order with no equal rows (paths
    // are unique), so an unstable sort matches Python's stable `sort`.
    let order = |a: &SprawlReport, b: &SprawlReport| -> Ordering {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    };
    if query.n < rows.len() {
        // Partition the top `n` to the front in O(len), then sort only those.
        rows.select_nth_unstable_by(query.n, order);
        rows.truncate(query.n);
    }
    rows.sort_unstable_by(order);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    // A plain literal with real newlines keeps the indentation (a `\`
    // line continuation would strip it).
    const HOST: &str = "---
type: Host
sources:
  - { id: a, resource: x }
  - { id: b, resource: y }
  - { id: c, resource: z }
---
# Host

Intro line.
**As of 2026-10-20** the host runs fine.
**Owner** is pgrace.

## Incident on Oct 5, 2026

It broke.
Then 2026-10-06 it was fixed.

## Ports

```
2026-01-01 inside a fence does not count
```
- 22
- 80
";

    fn snapshot() -> Snapshot {
        Snapshot::build(&MemWorkspace::from_files([
            ("host.md", HOST),
            (
                "todo.md",
                "---\ntype: Backlog\n---\n# Items\n\n- one\n- two\n",
            ),
            ("plain.md", "no frontmatter\n## History\n"),
        ]))
        .unwrap()
    }

    #[test]
    fn host_report_matches_okf_py() {
        let rows = sprawl(
            &snapshot(),
            SprawlQuery {
                concept: None,
                n: 10,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&rows).unwrap();
        let expected = serde_json::json!([
            {"path": "/host.md", "score": 6.8, "exempt": null, "lines": 13, "sections": 3,
             "sources": 3, "volatile": 2, "dates": 3,
             "sections_detail": [
                {"heading": "Host", "level": 1, "line": 8, "lines": 12, "volatile": null},
                {"heading": "Incident on Oct 5, 2026", "level": 2, "line": 14, "lines": 2,
                 "volatile": "incident"},
                {"heading": "Ports", "level": 2, "line": 19, "lines": 5, "volatile": null}],
             "volatile_detail": [
                {"kind": "heading", "text": "Incident on Oct 5, 2026", "line": 14, "why": "incident"},
                {"kind": "lead-in", "text": "As of 2026-10-20", "line": 11, "why": "as of"}],
             "candidates": [
                {"heading": "Incident on Oct 5, 2026", "level": 2, "line": 14, "lines": 2,
                 "volatile": "incident"},
                {"heading": "Host", "level": 1, "line": 8, "lines": 12, "volatile": null}]},
            {"path": "/plain.md", "score": 2.1, "exempt": null, "lines": 2, "sections": 1,
             "sources": 0, "volatile": 1, "dates": 0,
             "sections_detail": [
                {"heading": "History", "level": 2, "line": 2, "lines": 0, "volatile": "history"}],
             "volatile_detail": [
                {"kind": "heading", "text": "History", "line": 2, "why": "history"}],
             "candidates": [
                {"heading": "History", "level": 2, "line": 2, "lines": 0, "volatile": "history"}]},
        ]);
        assert_eq!(json, expected);
        assert_eq!(json.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn exempt_types_are_listed_only_by_name() {
        let rows = sprawl(
            &snapshot(),
            SprawlQuery {
                concept: Some("todo"),
                n: 5,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].exempt.as_deref(), rows[0].score),
            (Some("Backlog"), 0.7)
        );
        assert!(matches!(
            sprawl(
                &snapshot(),
                SprawlQuery {
                    concept: Some("nope"),
                    n: 5
                }
            ),
            Err(OkfError::NotFound { .. })
        ));
    }

    #[test]
    fn volatile_words_ignore_case_like_python() {
        assert_eq!(
            volatile_reason("Recent \u{130}NCIDENT").as_deref(),
            Some("i\u{307}ncident")
        );
        assert_eq!(
            volatile_reason("Seen Oct 5, 2026"),
            Some("Oct 5, 2026".to_string())
        );
        assert_eq!(
            volatile_reason("fixedly"),
            None,
            "no word boundary after `fixed`"
        );
        assert_eq!(
            volatile_reason("\u{17f}napshot").as_deref(),
            Some("\u{17f}napshot")
        );
        assert_eq!(
            volatile_reason("Oc\u{131}ober 2026"),
            None,
            "`\u{131}` is not `t`"
        );
        assert_eq!(volatile_reason("2026-10-05x").as_deref(), Some("2026-10"));
        assert_eq!(
            volatile_reason("history\u{301}").as_deref(),
            Some("history")
        );
        assert_eq!(
            volatile_reason("x_history"),
            None,
            "`_` is a word character"
        );
    }

    #[test]
    fn scores_add_like_python_sum() {
        // From Python 3.14's `sum`: compensated, unlike a plain fold.
        let parts = [1.05, 0.5, 1.0, 0.0, 0.6000000000000001];
        assert_eq!(parts.iter().sum::<f64>(), 3.15);
        assert_eq!(py_float_sum(parts), 3.1500000000000004);
        assert_eq!(round_one(py_float_sum(parts)), 3.2);
        assert_eq!(py_float_sum([0.1; 10]), 1.0);
        assert_eq!(py_float_sum([]), 0.0);
        assert!(py_float_sum([f64::INFINITY, 1.0]).is_infinite());
    }

    #[test]
    fn dates_resume_after_each_match_like_findall() {
        let line = "2026-10-05-2026-11 Oct 2026,Nov 3 2026 2026-1x 1999-12-31";
        let dates: Vec<&str> = dates_in(line).collect();
        assert_eq!(
            dates,
            [
                "2026-10-05",
                "2026-11",
                "Oct 2026",
                "Nov 3 2026",
                "1999-12-31"
            ]
        );
    }

    // Fences closed only by their own marker, lead-ins inside code, invalid
    // frontmatter, Python-cased dates, a volatile tie broken by size, and the
    // top-`n` cut with a tie broken by path.
    const QUIRKS: [(&str, &str); 5] = [
        (
            "q.md",
            "---\ntype: [x\nsources: y\n---\n# Big\n~~~\n**Status** hidden\n```\n\
             2026-01-01\n~~~\n**Status:** shown 2026-02\n**x**\n\
             **\u{17f}napshot of OCT\u{130} 2026**\n\
             2026-10-05x 2026-10-05-2026-11 oct 2026 OCT 2026\n## Changelog\na\n\
             ## Gotchas\nb\nc\n",
        ),
        ("b.md", "---\ntype: \"  BACKLOG \"\nsources: [a, b]\n---\n"),
        ("a.md", "# A\n"),
        ("z.md", "# Z\n"),
        ("index.md", "# I\n"),
    ];

    #[test]
    fn quirks_match_okf_py() {
        // Arrange
        let snapshot = Snapshot::build(&MemWorkspace::from_files(QUIRKS)).unwrap();
        // `okf.py sprawl --json -n 2` and `okf.py sprawl --json b`, recompacted.
        let top: serde_json::Value = serde_json::from_str(concat!(
            r#"[{"path": "/q.md", "score": 9.4, "exempt": null, "lines": 15, "sections": 3, "#,
            r#""sources": 0, "volatile": 4, "dates": 6, "sections_detail": [{"heading": "Big", "#,
            r#""level": 1, "line": 5, "lines": 14, "volatile": null}, {"heading": "Changelog", "#,
            r#""level": 2, "line": 15, "lines": 1, "volatile": "changelog"}, {"heading": "#,
            r#""Gotchas", "level": 2, "line": 17, "lines": 2, "volatile": "gotchas"}], "#,
            r#""volatile_detail": [{"kind": "heading", "text": "Changelog", "line": 15, "why": "#,
            r#""changelog"}, {"kind": "heading", "text": "Gotchas", "line": 17, "why": "#,
            r#""gotchas"}, {"kind": "lead-in", "text": "Status:", "line": 11, "why": "status"}, "#,
            r#"{"kind": "lead-in", "text": "\u017fnapshot of OCT\u0130 2026", "line": 13, "#,
            r#""why": "\u017fnapshot"}], "candidates": [{"heading": "Gotchas", "level": 2, "#,
            r#""line": 17, "lines": 2, "volatile": "gotchas"}, {"heading": "Changelog", "#,
            r#""level": 2, "line": 15, "lines": 1, "volatile": "changelog"}, {"heading": "#,
            r#""Big", "level": 1, "line": 5, "lines": 14, "volatile": null}]}, {"path": "#,
            r#""/a.md", "score": 0.6, "exempt": null, "lines": 1, "sections": 1, "sources": 0, "#,
            r#""volatile": 0, "dates": 0, "sections_detail": [{"heading": "A", "level": 1, "#,
            r#""line": 1, "lines": 0, "volatile": null}], "volatile_detail": [], "#,
            r#""candidates": []}]"#
        ))
        .unwrap();
        let backlog: serde_json::Value = serde_json::from_str(concat!(
            r#"[{"path": "/b.md", "score": 0.7, "exempt": "  BACKLOG ", "lines": 0, "#,
            r#""sections": 0, "sources": 2, "volatile": 0, "dates": 0, "sections_detail": [], "#,
            r#""volatile_detail": [], "candidates": []}]"#
        ))
        .unwrap();

        // Act
        let top_rows = sprawl(
            &snapshot,
            SprawlQuery {
                concept: None,
                n: 2,
            },
        )
        .unwrap();
        let backlog_rows = sprawl(
            &snapshot,
            SprawlQuery {
                concept: Some("/b.md"),
                ..Default::default()
            },
        );
        let all = sprawl(
            &snapshot,
            SprawlQuery {
                concept: None,
                n: 9,
            },
        )
        .unwrap();
        let empty = sprawl(
            &snapshot,
            SprawlQuery {
                concept: None,
                n: 0,
            },
        )
        .unwrap();

        // Assert
        assert_eq!(serde_json::to_value(&top_rows).unwrap(), top);
        assert_eq!(
            serde_json::to_value(backlog_rows.unwrap()).unwrap(),
            backlog
        );
        let paths: Vec<&str> = all.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            ["/q.md", "/a.md", "/z.md"],
            "exempt b.md and index.md left out"
        );
        assert!(empty.is_empty());
        assert!(matches!(
            sprawl(
                &snapshot,
                SprawlQuery {
                    concept: Some("index"),
                    n: 5
                }
            ),
            Err(OkfError::InvalidPath { .. })
        ));
        assert_eq!(
            SprawlQuery::default(),
            SprawlQuery {
                concept: None,
                n: 5
            }
        );
    }
}
