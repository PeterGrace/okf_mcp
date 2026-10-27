//! `okf.py oldest --json`: concepts ordered by when they were last touched
//! (`AT_RE`, `_ats`, `describe` and `cmd_oldest`).
//!
//! A concept's freshness is its newest `at:` stamp under `generated`,
//! `verified` (a mapping or a list of them) and `reviewed`; concepts with none
//! sort first, at the Unix epoch. Deprecated concepts are skipped. When the
//! frontmatter does not parse, the stamps come from a regex scan of each
//! field's block and the scalars from `_fm_fallback`, as in `okf.py`.
//!
//! As in `okf.py`, only *string* values count: a non-string `at`, `title`,
//! `status` or `stale_after` is treated as missing, and `open_questions` must
//! be a list.
//!
//! # Known divergences from `okf.py`
//!
//! * Naive timestamps (no offset, e.g. `at: 2026-10-26`) are ignored, and a
//!   naive `stale_after` is never stale; `okf.py` raises `TypeError`
//!   comparing them with an aware time.
//! * In the fallback for unparseable frontmatter, an `at:` value that
//!   `AT_RE` matches but that is not a valid date is skipped; `okf.py`
//!   raises `ValueError`.

use std::cmp::Ordering;
use std::sync::LazyLock;

use chrono::{DateTime, FixedOffset, Utc};
use indexmap::IndexMap;
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;

use crate::okf::pystr::py_str;
use crate::okf::snapshot::{fm_fallback_scalar, DocKind, Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::timestamp::{parse_iso, IsoFormat};
use crate::okf::yaml::Value;

/// The stamp fields, in the order `okf.py` prefers on a tie.
const STAMP_FIELDS: [&str; 3] = ["generated", "verified", "reviewed"];

/// One row of `oldest --json`, keys in `okf.py`'s order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OldestRow {
    /// `/`-prefixed concept path.
    pub path: String,
    /// `title`, if a string.
    pub title: Option<String>,
    /// `status`, defaulting to `stable` when missing, empty or not a string.
    pub status: String,
    /// The newest stamp's `isoformat()`, with `+00:00` written `Z`.
    pub last_touched: Option<String>,
    /// Which field it came from (`generated`, `verified` or `reviewed`).
    pub last_touched_by: Option<&'static str>,
    /// Days since then, rounded to one decimal (negative for a future stamp).
    pub age_days: Option<f64>,
    /// `stale_after` has passed.
    pub stale: bool,
    /// Queued `open_questions`, each as Python's `str()`.
    pub open_questions: Vec<String>,
    /// The sort key (the Unix epoch when never touched); not serialized.
    // `#[serde(skip)]` leaves the field out of the JSON, like `okf.py`
    // deleting `_sort` before printing.
    #[serde(skip)]
    fresh: DateTime<FixedOffset>,
}

/// What `oldest` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OldestQuery {
    /// How many rows (ignored with `open_questions_only`).
    pub n: usize,
    /// Every concept with queued open questions, instead of the `n` oldest.
    pub open_questions_only: bool,
}

impl Default for OldestQuery {
    /// `okf.py`'s defaults: `-n 5`, no `--open-questions`.
    fn default() -> Self {
        Self {
            n: 5,
            open_questions_only: false,
        }
    }
}

/// `cmd_oldest`: non-deprecated concepts, oldest first (ties by path).
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `now` - the current time (from the server clock), for `age_days` and
///   `stale`.
/// * `query` - how many, or only those with open questions.
///
/// # Returns
/// The rows `okf.py oldest --json` prints, in its order.
pub fn oldest(snapshot: &Snapshot, now: DateTime<Utc>, query: OldestQuery) -> Vec<OldestRow> {
    let now = now.fixed_offset();
    // `par_iter` describes the concepts on a thread pool. The input order does
    // not matter: paths are unique, so the sort below fixes the final order.
    let mut rows: Vec<OldestRow> = snapshot
        .docs()
        .par_iter()
        .filter(|doc| doc.kind() == DocKind::Concept)
        .map(|doc| describe(doc, now))
        .filter(|row| row.status != "deprecated" && wanted(row, query))
        .collect();
    // `(fresh, path)` is a total order with no equal rows (paths are unique),
    // so an unstable sort gives the same result as Python's stable `sort`.
    let order = |a: &OldestRow, b: &OldestRow| -> Ordering {
        a.fresh.cmp(&b.fresh).then_with(|| a.path.cmp(&b.path))
    };
    if !query.open_questions_only && query.n < rows.len() {
        // Partition the `n` oldest to the front in O(len), then sort only
        // those: `rows[: n]` of the sorted list without sorting all of it.
        rows.select_nth_unstable_by(query.n, order);
        rows.truncate(query.n);
    }
    rows.sort_unstable_by(order);
    rows
}

/// `--open-questions` keeps only rows with questions (filtering before the
/// sort gives the same rows as `okf.py` filtering after it).
fn wanted(row: &OldestRow, query: OldestQuery) -> bool {
    !query.open_questions_only || !row.open_questions.is_empty()
}

/// `AT_RE`: `\bat:\s*['"]?(TIMESTAMP)`, `\b` emulated by a consumed character
/// (verified fact 7). A match always ends on a digit or `Z`, never on the
/// consumed class, so consecutive matches find the same values as Python.
static AT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?:\A|[^\p{L}\p{N}_])at:[\s\x1c-\x1f]*['"]?"#,
        r"(\d{4}-\d{2}-\d{2}T[0-9:.]+(?:Z|[+-]\d{2}:?\d{2}))",
    ))
    .expect("AT regex is valid")
});

/// The newest of `values`, keeping the first on a tie (Python's `max`).
///
/// `DateTime`'s ordering compares instants, so `10:00Z` and `12:00+02:00`
/// tie, as aware datetimes do in Python.
fn first_max<T: Copy>(
    values: impl Iterator<Item = T>,
    key: impl Fn(T) -> DateTime<FixedOffset>,
) -> Option<T> {
    values.reduce(|best, next| if key(next) > key(best) { next } else { best })
}

/// `_ats`: aware timestamps under the `at` keys of a mapping or a list of
/// mappings (a non-string `at`, or one that does not parse, is skipped).
///
/// Returns `impl Iterator`, a lazily evaluated sequence like a Python
/// generator; `'_` says it borrows from `value`.
fn ats(value: Option<&Value>) -> impl Iterator<Item = DateTime<FixedOffset>> + '_ {
    let items: &[Value] = match value {
        Some(Value::Seq(items)) => items,
        // `from_ref` views one value as a one-item slice (Python's `[v]`).
        Some(single) => std::slice::from_ref(single),
        None => &[],
    };
    items.iter().filter_map(|item| match item {
        // `?` inside the closure returns `None` early, like a short-circuit.
        Value::Map(map) => parse_iso(map.get("at")?.as_str()?)?.aware(),
        Value::Null | Value::Str(_) | Value::Seq(_) => None,
    })
}

/// The fields `describe` reads, from parsed frontmatter or the fallback.
struct Described {
    /// The newest stamp of each field that has one, in [`STAMP_FIELDS`] order.
    stamps: Vec<(&'static str, DateTime<FixedOffset>)>,
    title: Option<String>,
    status: Option<String>,
    stale_after: Option<String>,
    questions: Vec<String>,
}

/// `describe` for parsed (or absent, `None`) frontmatter.
fn from_data(data: Option<&IndexMap<String, Value>>) -> Described {
    let value = |key: &str| data.and_then(|d| d.get(key));
    // The str-only `get` of `okf.py`: a non-string value counts as missing.
    let text = |key: &str| value(key).and_then(Value::as_str).map(str::to_string);
    let questions = match value("open_questions") {
        Some(Value::Seq(items)) => items.iter().map(py_str).collect(),
        Some(Value::Null | Value::Str(_) | Value::Map(_)) | None => Vec::new(),
    };
    Described {
        stamps: STAMP_FIELDS
            .iter()
            .filter_map(|&field| Some((field, first_max(ats(value(field)), |t| t)?)))
            .collect(),
        title: text("title"),
        status: text("status"),
        stale_after: text("stale_after"),
        questions,
    }
}

/// `describe`'s regex fallback for frontmatter that does not parse.
fn from_fallback(doc: &SnapshotDoc) -> Described {
    let lines = doc.doc().frontmatter();
    let raw = lines.join("\n");
    let stamps = STAMP_FIELDS
        .iter()
        .filter_map(|&field| {
            let block = doc.doc().block(field)?;
            let text = lines[block.start..block.end].join("\n");
            let found = AT
                .captures_iter(&text)
                .filter_map(|c| parse_iso(&c[1])?.aware());
            Some((field, first_max(found, |t| t)?))
        })
        .collect();
    Described {
        stamps,
        title: fm_fallback_scalar(&raw, "title"),
        status: fm_fallback_scalar(&raw, "status"),
        stale_after: fm_fallback_scalar(&raw, "stale_after"),
        questions: Vec::new(),
    }
}

/// `describe`: one concept's row.
fn describe(doc: &SnapshotDoc, now: DateTime<FixedOffset>) -> OldestRow {
    let described = match doc.frontmatter() {
        Frontmatter::Parsed(data) => from_data(Some(data)),
        Frontmatter::Absent => from_data(None),
        Frontmatter::Invalid(_) => from_fallback(doc),
    };
    let newest = first_max(described.stamps.iter().copied(), |(_, t)| t);
    // `bool(stale_after) and now >= parse_ts(stale_after)`.
    let stale = described
        .stale_after
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(parse_iso)
        .and_then(|t| t.aware())
        .is_some_and(|t| now >= t);
    OldestRow {
        path: format!("/{}", doc.path()),
        title: described.title,
        // `get("status") or "stable"`: an empty string is falsy too.
        status: described
            .status
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "stable".to_string()),
        last_touched: newest.map(|(_, t)| t.to_string_iso().replace("+00:00", "Z")),
        last_touched_by: newest.map(|(field, _)| field),
        age_days: newest.map(|(_, t)| age_days(now, t)),
        stale,
        open_questions: described.questions,
        fresh: newest.map_or(DateTime::<Utc>::UNIX_EPOCH.fixed_offset(), |(_, t)| t),
    }
}

/// `round((now - fresh).total_seconds() / 86400, 1)`.
fn age_days(now: DateTime<FixedOffset>, fresh: DateTime<FixedOffset>) -> f64 {
    // `num_microseconds` is `None` only past about 292,000 years, which two
    // dates in years 1..=9999 cannot reach.
    let micros = (now - fresh).num_microseconds().unwrap_or(i64::MAX);
    // Exact for |micros| < 2^53 (about 285 years); beyond that the cast rounds
    // once, as Python's integer true division does. Then both divisions round
    // like `timedelta.total_seconds()` and `/ 86400`.
    round_one(micros as f64 / 1e6 / 86400.0)
}

/// Python's `round(x, 1)`: `{:.1}` rounds the exact binary value with ties
/// to even, as Python does (verified fact 4).
///
/// # Arguments
/// * `value` - a finite float.
///
/// # Returns
/// `value` rounded to one decimal place (`value` itself if not finite).
pub(crate) fn round_one(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::TimeZone;

    const FILES: [(&str, &str); 5] = [
        (
            "a.md",
            "---\ntype: T\ntitle: A\ngenerated: { by: x, at: 2026-10-01T00:00:00Z }\n\
             verified:\n  - { by: x, at: \"2026-10-10T12:00:00+05:30\" }\n\
             open_questions: [Why?, { k: v }]\n---\n",
        ),
        ("b.md", "---\ntype: T\ntitle: B\n---\n"),
        (
            "c.md",
            "---\ntype: T\nstatus: deprecated\ngenerated: { by: x, at: 2020-01-01T00:00:00Z }\n---\n",
        ),
        (
            "d.md",
            "---\ntype: T\ntitle: D\ntitle: again\ngenerated: { by: x, at: 2026-09-01T00:00:00Z }\n\
             stale_after: 2026-01-01T00:00:00Z\n---\n",
        ),
        (
            "e.md",
            "---\ntype: T\ngenerated: { by: x, at: 2026-10-20T00:00:00.5Z }\n\
             stale_after: 2027-01-01T00:00:00Z\n\
             reviewed: { by: x, at: 2026-10-21T00:00:00Z, outcome: verified, note: n }\n---\n",
        ),
    ];

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
    }

    fn rows(files: &[(&str, &str)], query: OldestQuery) -> Vec<OldestRow> {
        let workspace = MemWorkspace::from_files(files.iter().copied());
        oldest(&Snapshot::build(&workspace).unwrap(), now(), query)
    }

    /// Asserts value equality and, through the text, key order.
    fn assert_json(found: &[OldestRow], expected: serde_json::Value) {
        let json = serde_json::to_value(found).unwrap();
        assert_eq!(json, expected);
        assert_eq!(json.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn rows_match_okf_py() {
        // Arrange: expected rows are okf.py's output for FILES, now frozen at
        // 2026-10-27T12:00:00Z.
        let expected = serde_json::json!([
            {"path": "/b.md", "title": "B", "status": "stable", "last_touched": null,
             "last_touched_by": null, "age_days": null, "stale": false, "open_questions": []},
            {"path": "/d.md", "title": "D", "status": "stable",
             "last_touched": "2026-09-01T00:00:00Z", "last_touched_by": "generated",
             "age_days": 56.5, "stale": true, "open_questions": []},
            {"path": "/a.md", "title": "A", "status": "stable",
             "last_touched": "2026-10-10T12:00:00+05:30", "last_touched_by": "verified",
             "age_days": 17.2, "stale": false, "open_questions": ["Why?", "{'k': 'v'}"]},
            {"path": "/e.md", "title": null, "status": "stable",
             "last_touched": "2026-10-21T00:00:00Z", "last_touched_by": "reviewed",
             "age_days": 6.5, "stale": false, "open_questions": []},
        ]);
        // Act
        let found = rows(
            &FILES,
            OldestQuery {
                n: 10,
                open_questions_only: false,
            },
        );
        // Assert
        assert_json(&found, expected);
    }

    #[test]
    fn quirks_match_okf_py() {
        // Arrange: each file exercises an okf.py quirk; the expected rows are
        // okf.py's output for these files with now frozen as above.
        let files = [
            // Fallback (duplicate key): regex stamps, `+0530` offset, quoted
            // `stale_after` stripped of its quotes.
            (
                "h.md",
                "---\ntype: T\ntitle: H\ntitle: dup\nverified:\n\
                 \x20 - { by: x, at: 2026-10-03T00:00:00Z }\n\
                 \x20 - { by: x, at: \"2026-10-04T00:00:00+0530\" }\n\
                 reviewed: { by: x, at: 2026-10-02T00:00:00Z }\n\
                 stale_after: \"2026-01-01T00:00:00Z\"\nstatus: draft\n---\n",
            ),
            // Non-string values count as missing; non-mapping items and bad
            // dates are skipped; a scalar `open_questions` gives none.
            (
                "i.md",
                "---\ntype: T\ntitle: [x]\nstatus: [a]\n\
                 generated: { by: x, at: [2026-10-25T00:00:00Z] }\n\
                 verified: [{ at: 2026-10-05T00:00:00Z }, { at: 2026-10-09T00:00:00Z }, \
                 plain, { at: nonsense }]\n\
                 open_questions: Why?\nstale_after: [2026-01-01T00:00:00Z]\n---\n",
            ),
            // A future stamp gives a negative age; list questions use repr.
            (
                "j.md",
                "---\ntype: T\ntitle: J\n\
                 generated: { by: x, at: \"2026-10-01T00:00:00+00:00:30\" }\n\
                 reviewed: { by: x, at: 2026-10-27T13:00:00-01:00 }\n\
                 open_questions:\n  - [a, b]\n  - plain\n---\n",
            ),
            ("k.md", "# K\n\nNo frontmatter.\n"),
            // `replace("+00:00", "Z")` also hits a seconds offset.
            (
                "l.md",
                "---\ntype: T\ngenerated: { by: x, at: \"2026-10-01T00:00:00+00:00:30\" }\n---\n",
            ),
        ];
        let expected = serde_json::json!([
            {"path": "/k.md", "title": null, "status": "stable", "last_touched": null,
             "last_touched_by": null, "age_days": null, "stale": false, "open_questions": []},
            {"path": "/l.md", "title": null, "status": "stable",
             "last_touched": "2026-10-01T00:00:00Z:30", "last_touched_by": "generated",
             "age_days": 26.5, "stale": false, "open_questions": []},
            {"path": "/h.md", "title": "H", "status": "draft",
             "last_touched": "2026-10-04T00:00:00+05:30", "last_touched_by": "verified",
             "age_days": 23.7, "stale": true, "open_questions": []},
            {"path": "/i.md", "title": null, "status": "stable",
             "last_touched": "2026-10-09T00:00:00Z", "last_touched_by": "verified",
             "age_days": 18.5, "stale": false, "open_questions": []},
            {"path": "/j.md", "title": "J", "status": "stable",
             "last_touched": "2026-10-27T13:00:00-01:00", "last_touched_by": "reviewed",
             "age_days": -0.1, "stale": false, "open_questions": ["['a', 'b']", "plain"]},
        ]);
        // Act
        let found = rows(&files, OldestQuery::default());
        // Assert
        assert_json(&found, expected);
    }

    #[test]
    fn n_limits_and_open_questions_filters() {
        // Arrange
        let first_two = OldestQuery {
            n: 2,
            open_questions_only: false,
        };
        let questions = OldestQuery {
            n: 1,
            open_questions_only: true,
        };
        // Act
        let limited = rows(&FILES, first_two);
        let with_questions = rows(&FILES, questions);
        let none = rows(
            &FILES,
            OldestQuery {
                n: 0,
                open_questions_only: false,
            },
        );
        // Assert: `n` keeps the oldest, and is ignored with open questions.
        let paths =
            |found: &[OldestRow]| -> Vec<String> { found.iter().map(|r| r.path.clone()).collect() };
        assert_eq!(paths(&limited), ["/b.md", "/d.md"]);
        assert_eq!(paths(&with_questions), ["/a.md"]);
        assert!(none.is_empty());
    }

    #[test]
    fn naive_timestamps_are_ignored() {
        // Arrange: okf.py raises TypeError here (documented divergence).
        let files = [(
            "f.md",
            "---\ntype: T\ngenerated: { by: x, at: 2026-10-02 }\nstale_after: 2026-01-01\n---\n",
        )];
        // Act
        let found = rows(&files, OldestQuery::default());
        // Assert
        assert_eq!(found[0].last_touched, None);
        assert!(!found[0].stale);
    }

    #[test]
    fn invalid_fallback_dates_are_skipped() {
        // Arrange: AT_RE matches `2026-13-01T...`; okf.py raises ValueError
        // (documented divergence). The duplicate key forces the fallback.
        let files = [(
            "m.md",
            "---\ntype: T\ntype: U\ngenerated: { by: x, at: 2026-13-01T00:00:00Z }\n\
             reviewed: { by: x, at: 2026-10-02T00:00:00Z }\n---\n",
        )];
        // Act
        let found = rows(&files, OldestQuery::default());
        // Assert
        assert_eq!(found[0].last_touched_by, Some("reviewed"));
    }

    #[test]
    fn equal_instants_keep_the_first_field() {
        // Arrange: Python's max() returns the first maximal item.
        let files = [(
            "g.md",
            "---\ntype: T\ngenerated: { by: x, at: 2026-10-02T00:00:00Z }\n\
             verified: { by: x, at: \"2026-10-02T02:00:00+02:00\" }\n---\n",
        )];
        // Act
        let found = rows(&files, OldestQuery::default());
        // Assert
        assert_eq!(found[0].last_touched_by, Some("generated"));
        assert_eq!(
            found[0].last_touched.as_deref(),
            Some("2026-10-02T00:00:00Z")
        );
    }

    #[test]
    fn round_one_rounds_like_python() {
        // Arrange: Python's round(x, 1) for each input (verified fact 4).
        let cases = [
            (0.25, 0.2),
            (0.35, 0.3),
            (2.675, 2.7),
            (18.75, 18.8),
            (0.05, 0.1),
            (0.15, 0.1),
            (0.45, 0.5),
            (-0.04, -0.0),
        ];
        // Act / Assert
        for (input, expected) in cases {
            assert_eq!(
                round_one(input).to_bits(),
                f64::to_bits(expected),
                "{input}"
            );
        }
    }
}
