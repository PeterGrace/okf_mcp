//! `okf.py lint --json`: frontmatter, timestamp, link, index, sprawl and
//! secret checks, as errors and warnings (a port of `check_ts` and `lint`).
//!
//! The one check that needs git (`generated-after-commit`) takes the commit
//! times as data ([`CommitTimes`]), so this module stays pure; M3's git
//! module fills them.
//!
//! # Known divergences from `okf.py`
//!
//! * A `status` that is a list or mapping is reported with its `repr`;
//!   `okf.py` raises `TypeError` (unhashable type) and lint crashes.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use indexmap::IndexMap;
use rayon::prelude::*;
use serde::Serialize;

use crate::clock::TIMESTAMP_FORMAT;
use crate::okf::analysis::links::links_report;
use crate::okf::analysis::sprawl::{sprawl_of, SPRAWL_WARN};
use crate::okf::analysis::Finding;
use crate::okf::pystr::{py_repr, py_repr_opt, py_truthy};
use crate::okf::snapshot::{DocKind, Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::timestamp::{is_iso_timestamp, parse_iso};
use crate::okf::yaml::{Value, YamlError};

/// `STATUSES`.
const STATUSES: [&str; 3] = ["draft", "stable", "deprecated"];
/// `OUTCOMES`, in `okf.py`'s order (the message lists them).
const OUTCOMES: [&str; 4] = ["verified", "updated", "partial", "unverifiable"];
/// `FUTURE_SLACK`: two minutes of clock skew allowed.
const FUTURE_SLACK: TimeDelta = TimeDelta::minutes(2);
/// The `reviewed-shape` message (two code paths report it).
const REVIEWED_SHAPE: &str = "reviewed should be a mapping with by, at, outcome, note";

/// Each file's last commit time, and the files with uncommitted changes:
/// what `okf.py`'s `last_commit_times()` returns when git works.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitTimes {
    times: HashMap<String, DateTime<FixedOffset>>,
    dirty: HashSet<String>,
}

impl CommitTimes {
    /// Builds the record.
    ///
    /// # Arguments
    /// * `times` - catalog-relative path (no leading `/`) to the committer
    ///   time of the last commit touching it (`git log --format=%cI`).
    /// * `dirty` - paths with uncommitted changes (`git status --porcelain`).
    ///
    /// # Returns
    /// The record [`lint`] reads.
    pub fn new(times: HashMap<String, DateTime<FixedOffset>>, dirty: HashSet<String>) -> Self {
        Self { times, dirty }
    }

    /// The last commit time of a clean, committed file: `None` when the
    /// file has uncommitted changes or was never committed (`okf.py` then
    /// skips the check).
    fn committed_clean(&self, path: &str) -> Option<DateTime<FixedOffset>> {
        if self.dirty.contains(path) {
            return None;
        }
        self.times.get(path).copied()
    }
}

/// `{"errors": [...], "warnings": [...]}`, each sorted by (file, line, rule).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LintReport {
    /// Problems that block `publish`.
    pub errors: Vec<Finding>,
    /// Advice.
    pub warnings: Vec<Finding>,
}

impl LintReport {
    /// Appends `other`'s findings after this report's, keeping both orders.
    fn append(&mut self, mut other: LintReport) {
        self.errors.append(&mut other.errors);
        self.warnings.append(&mut other.warnings);
    }
}

/// Runs every lint check.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `now` - the current time, for the future and `stale_after` checks.
/// * `commits` - git commit times, or `None` when git is unavailable (the
///   `generated-after-commit` check is then skipped, as in `okf.py`).
///
/// # Returns
/// The findings, each list sorted by (file, line, rule) with ties in
/// `okf.py`'s discovery order.
pub fn lint(snapshot: &Snapshot, now: DateTime<Utc>, commits: Option<&CommitTimes>) -> LintReport {
    let now = now.fixed_offset();
    // Each document is checked on rayon's thread pool. `collect` into a `Vec`
    // keeps the documents' order, so ties sort exactly as in `okf.py`.
    // `okf.py` finds sprawl in a later loop, after the links; scoring it here
    // in the same pass changes no order, because the `sprawl` rule occurs at
    // most once per file and so never ties with another finding.
    let per_doc: Vec<LintReport> = snapshot
        .docs()
        .par_iter()
        .map(|doc| {
            let mut linter = Linter::new(doc.path(), now);
            linter.document(doc, commits);
            linter.report
        })
        .collect();
    let mut report = LintReport::default();
    for doc_report in per_doc {
        report.append(doc_report);
    }
    report.append(link_findings(snapshot));
    report
        .errors
        .extend(snapshot.secret_findings().iter().cloned());
    // `sort_by` is stable, like Python's `sorted`; the key borrows instead
    // of cloning each finding's strings.
    let key = |a: &Finding, b: &Finding| -> Ordering {
        (&a.file, a.line, &a.rule).cmp(&(&b.file, b.line, &b.rule))
    };
    report.errors.sort_by(key);
    report.warnings.sort_by(key);
    report
}

/// Builds one finding.
fn finding(file: &str, line: usize, rule: &str, message: String) -> Finding {
    Finding {
        file: file.to_string(),
        line,
        rule: rule.to_string(),
        message,
    }
}

/// The findings of one document's checks (`okf.py`'s first loop).
struct Linter<'a> {
    /// The document being checked.
    path: &'a str,
    /// "Now", for the future and `stale_after` checks.
    now: DateTime<FixedOffset>,
    /// What has been found so far.
    report: LintReport,
}

impl<'a> Linter<'a> {
    fn new(path: &'a str, now: DateTime<FixedOffset>) -> Self {
        Self {
            path,
            now,
            report: LintReport::default(),
        }
    }

    fn error(&mut self, line: usize, rule: &str, message: String) {
        let found = finding(self.path, line, rule, message);
        self.report.errors.push(found);
    }

    fn warn(&mut self, line: usize, rule: &str, message: String) {
        let found = finding(self.path, line, rule, message);
        self.report.warnings.push(found);
    }

    /// `frontmatter does not parse`, at the error's line (1 when unknown).
    fn parse_error(&mut self, error: &YamlError) {
        let message = format!("frontmatter does not parse: {}", error.message);
        self.error(error.line.unwrap_or(1), "frontmatter-parse", message);
    }

    /// Routes a document to the index or concept checks; `log.md` and
    /// machinery documents have none.
    fn document(&mut self, doc: &SnapshotDoc, commits: Option<&CommitTimes>) {
        match doc.kind() {
            DocKind::Index => self.index(doc),
            DocKind::Concept => {
                self.concept(doc, commits);
                self.sprawl(doc);
            }
            DocKind::Log | DocKind::Machinery => {}
        }
    }

    /// `index.md` rules: no frontmatter below the root; the root declares
    /// `okf_version`.
    fn index(&mut self, doc: &SnapshotDoc) {
        if self.path != "index.md" {
            if doc.doc().has_frontmatter() {
                let message = "non-root index.md must not have frontmatter".to_string();
                self.error(1, "index-frontmatter", message);
            }
            return;
        }
        let declared = match doc.frontmatter() {
            Frontmatter::Invalid(error) => return self.parse_error(error),
            Frontmatter::Absent => false,
            Frontmatter::Parsed(data) => get(data, "okf_version").is_some(),
        };
        if !declared {
            let message = "root index.md does not declare okf_version".to_string();
            self.warn(1, "okf-version", message);
        }
    }

    /// A `sprawl` warning when the concept scores above [`SPRAWL_WARN`] and
    /// its type is not exempt.
    fn sprawl(&mut self, doc: &SnapshotDoc) {
        let sprawl = sprawl_of(doc);
        if sprawl.score <= SPRAWL_WARN || sprawl.exempt.is_some() {
            return;
        }
        let message = format!(
            // `{:?}` prints an f64 as Python's str() does (20.0, not 20);
            // `{}` prints SPRAWL_WARN as 16, like okf.py's int.
            "sprawl score {:?} > {} ({} lines, {} sections, {} volatile); consider \
             splitting (okf.py sprawl {}, okf-reorganize skill)",
            sprawl.score, SPRAWL_WARN, sprawl.lines, sprawl.sections, sprawl.volatile, self.path
        );
        self.warn(1, "sprawl", message);
    }

    /// The concept checks, in `okf.py`'s order.
    fn concept(&mut self, doc: &SnapshotDoc, commits: Option<&CommitTimes>) {
        let data = match doc.frontmatter() {
            Frontmatter::Absent => {
                let message = "concept has no YAML frontmatter block".to_string();
                return self.error(1, "no-frontmatter", message);
            }
            Frontmatter::Invalid(error) => return self.parse_error(error),
            Frontmatter::Parsed(data) => data,
        };
        let line = |key: &str| doc.doc().key_line(key);

        if !py_truthy(data.get("type")) {
            self.error(1, "no-type", "frontmatter has no 'type'".to_string());
        }
        for key in ["title", "description"] {
            if !py_truthy(data.get(key)) {
                self.warn(
                    1,
                    &format!("no-{key}"),
                    format!("frontmatter has no '{key}'"),
                );
            }
        }
        if let Some(status) = get(data, "status") {
            if !status.as_str().is_some_and(|s| STATUSES.contains(&s)) {
                let message = format!(
                    "status {} is not draft, stable or deprecated",
                    py_repr(status)
                );
                self.warn(line("status"), "status", message);
            }
        }
        // `and_then` runs the check only when `generated` is present.
        let generated_at =
            get(data, "generated").and_then(|value| self.generated(line("generated"), value));
        if let Some(verified) = get(data, "verified") {
            self.verified(line("verified"), verified);
        }
        if let Some(reviewed) = get(data, "reviewed") {
            self.reviewed(line("reviewed"), reviewed);
        }
        if let Some(stale_after) = get(data, "stale_after") {
            self.stale_after(line("stale_after"), stale_after);
        }
        if let Some(sources) = get(data, "sources") {
            self.sources(line("sources"), sources);
        }
        // `let ... else` binds both values or leaves the function.
        let (Some(generated_at), Some(committed)) = (
            generated_at,
            commits.and_then(|c| c.committed_clean(self.path)),
        ) else {
            return;
        };
        // Comparing the difference cannot overflow, unlike `committed + slack`.
        if generated_at - committed > FUTURE_SLACK {
            let message = format!(
                // `okf.py` formats generated.at with strftime in its own
                // offset (local fields, then a literal Z): `naive_local`.
                "generated.at {} is later than the file's last commit {}",
                generated_at.naive_local().format(TIMESTAMP_FORMAT),
                committed.with_timezone(&Utc).format(TIMESTAMP_FORMAT)
            );
            self.warn(line("generated"), "generated-after-commit", message);
        }
    }

    /// `generated`: a mapping with `by` and `at`; returns the parsed `at`.
    fn generated(&mut self, line: usize, value: &Value) -> Option<DateTime<FixedOffset>> {
        let map = as_map(value);
        if !map.is_some_and(|m| m.contains_key("at") && m.contains_key("by")) {
            let message = "generated should be a mapping with 'by' and 'at'".to_string();
            self.warn(line, "generated-shape", message);
        }
        // `?` returns `None` early when there is no mapping or no `at`.
        let at = map?.get("at")?;
        self.check_ts(at, "generated.at", line, true)
    }

    /// `verified`: one mapping with `at`, or a list of them.
    fn verified(&mut self, line: usize, value: &Value) {
        let items = match value {
            Value::Seq(items) => items.as_slice(),
            // A lone value is checked as a one-item list, as in `okf.py`.
            Value::Null | Value::Str(_) | Value::Map(_) => std::slice::from_ref(value),
        };
        for (i, item) in items.iter().enumerate() {
            match as_map(item).and_then(|m| m.get("at")) {
                Some(at) => {
                    self.check_ts(at, &format!("verified[{i}].at"), line, true);
                }
                None => {
                    let message = format!("verified[{i}] should be a mapping with 'by' and 'at'");
                    self.warn(line, "verified-shape", message);
                }
            }
        }
    }

    /// `reviewed`: a mapping with `at` and a known `outcome`.
    fn reviewed(&mut self, line: usize, value: &Value) {
        let Some((map, at)) = as_map(value).and_then(|m| Some((m, m.get("at")?))) else {
            return self.warn(line, "reviewed-shape", REVIEWED_SHAPE.to_string());
        };
        self.check_ts(at, "reviewed.at", line, true);
        let outcome = map.get("outcome");
        if !outcome
            .and_then(Value::as_str)
            .is_some_and(|o| OUTCOMES.contains(&o))
        {
            let message = format!(
                "reviewed.outcome {} is not one of {}",
                py_repr_opt(outcome),
                OUTCOMES.join(", ")
            );
            self.warn(line, "reviewed-outcome", message);
        }
    }

    /// `stale_after`: a valid timestamp that has not passed yet.
    fn stale_after(&mut self, line: usize, value: &Value) {
        let Some(ts) = self.check_ts(value, "stale_after", line, false) else {
            return;
        };
        if ts <= self.now {
            // `check_ts` accepted it, so it is a string.
            let text = value.as_str().unwrap_or_default();
            let message = format!("stale_after {text} has passed; re-check the concept");
            self.warn(line, "stale", message);
        }
    }

    /// `sources`: a list of mappings with a `resource`, and a valid
    /// `last_modified` where one is given.
    fn sources(&mut self, line: usize, value: &Value) {
        let Value::Seq(items) = value else {
            return self.warn(
                line,
                "sources-shape",
                "sources should be a list".to_string(),
            );
        };
        for (i, item) in items.iter().enumerate() {
            let Some(map) = as_map(item).filter(|m| py_truthy(m.get("resource"))) else {
                self.warn(
                    line,
                    "sources-shape",
                    format!("sources[{i}] has no 'resource'"),
                );
                continue;
            };
            if let Some(modified) = get(map, "last_modified") {
                let label = format!("sources[{i}].last_modified");
                self.check_ts(modified, &label, line, false);
            }
        }
    }

    /// `check_ts`: shape, validity, UTC and (optionally) not in the future.
    ///
    /// Returns the parsed instant when the value is a valid timestamp.
    fn check_ts(
        &mut self,
        value: &Value,
        label: &str,
        line: usize,
        future_check: bool,
    ) -> Option<DateTime<FixedOffset>> {
        let Some(text) = value.as_str().filter(|t| is_iso_timestamp(t)) else {
            let message = format!("{label} is not an ISO 8601 timestamp: {}", py_repr(value));
            self.error(line, "timestamp", message);
            return None;
        };
        // `ISO_RE` demands an offset, so a parse is always aware; `aware()`
        // only turns the impossible naive case into "not valid".
        let Some(ts) = parse_iso(text).and_then(|t| t.aware()) else {
            let message = format!("{label} is not a valid date/time: {}", py_repr(value));
            self.error(line, "timestamp", message);
            return None;
        };
        if !text.ends_with('Z') && !text.ends_with("+00:00") {
            let message = format!("{label} is not in UTC: {}", py_repr(value));
            self.warn(line, "timestamp-utc", message);
        }
        if future_check && ts - self.now > FUTURE_SLACK {
            let message = format!(
                "{label} {text} is in the future (now {})",
                self.now.format(TIMESTAMP_FORMAT)
            );
            self.error(line, "future-timestamp", message);
        }
        Some(ts)
    }
}

/// `d.get(key)` treating a YAML `null` like a missing key, as `okf.py`'s
/// `d.get(key) is not None` does.
fn get<'v>(data: &'v IndexMap<String, Value>, key: &str) -> Option<&'v Value> {
    data.get(key).filter(|v| !matches!(v, Value::Null))
}

/// `isinstance(value, dict)`.
fn as_map(value: &Value) -> Option<&IndexMap<String, Value>> {
    match value {
        Value::Map(map) => Some(map),
        Value::Null | Value::Str(_) | Value::Seq(_) => None,
    }
}

/// The link report's findings, as `okf.py` maps them.
fn link_findings(snapshot: &Snapshot) -> LintReport {
    let links = links_report(snapshot, false);
    let mut report = LintReport::default();
    for b in links.broken {
        let message = format!("link target does not exist: {}", b.link);
        let found = finding(&b.file, b.line, "broken-link", message);
        report.errors.push(found);
    }
    for b in links.broken_anchors {
        let message = format!("no heading in the target matches the anchor: {}", b.link);
        let found = finding(&b.file, b.line, "broken-anchor", message);
        report.errors.push(found);
    }
    for b in links.relative {
        let message = format!(
            "relative link {}; prefer a bundle-root absolute path",
            b.link
        );
        let found = finding(&b.file, b.line, "relative-link", message);
        report.warnings.push(found);
    }
    for orphan in links.orphans {
        let message = "no other concept links to this concept".to_string();
        report.warnings.push(finding(&orphan, 1, "orphan", message));
    }
    for drift in links.index_drift {
        let found = finding(&drift.file, 1, "index-drift", drift.problem);
        report.warnings.push(found);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::TimeZone;

    /// The plan's unit-test catalog; `testdata/lint-mini.json` is `okf.py`'s
    /// `lint()` for it with "now" and the commit times of [`mini_commits`].
    const MINI: [(&str, &str); 8] = [
        ("bare.md", "no frontmatter\n"),
        ("broken.md", "---\ntype: T\nbad: [x\n---\n"),
        (
            "index.md",
            "---\ntitle: Root\n---\n# Concepts\n\n* [Stamps](/stamps.md)\n* [Infra](infra/)\n",
        ),
        ("infra/index.md", "* [Sub](/infra/sub/)\n"),
        ("infra/sub/index.md", "---\nx: 1\n---\n"),
        (
            "notype.md",
            "---\ntitle: No type\ngenerated: { by: x, at: 2026-10-20T00:00:00Z }\n---\n",
        ),
        (
            "shapes.md",
            "---\ntype: T\ngenerated: { at: \"2026-10-01\" }\nreviewed: yes\nsources: one\n---\n",
        ),
        (
            "stamps.md",
            "---\ntype: Note\nstatus: bogus\ngenerated: { by: x, at: 2027-01-01T00:00:00Z }\n\
             verified:\n  - { by: x, at: \"2026-10-01T10:00:00-04:00\" }\n  - plain\n\
             reviewed: { by: x, at: 2026-10-02T00:00:00Z, outcome: maybe }\n\
             stale_after: 2026-01-01T00:00:00Z\nsources:\n  - { id: a }\n\
             \x20 - { id: b, resource: r, last_modified: 2026-13-01T00:00:00Z }\n---\n\
             See [gone](/gone.md) and [rel](infra/index.md).\n",
        ),
    ];

    /// Edge cases beyond `MINI`; `testdata/lint-quirks.json` is `okf.py`'s
    /// output for it with [`quirks_commits`]: a generated stamp with an
    /// offset (formatted with its local fields), commits inside the slack,
    /// stamps just inside and outside the future slack, `stale_after` equal
    /// to now, a block scalar with a trailing newline, full-width digits,
    /// null and empty values, non-string stamps, a broken anchor, and a
    /// folder index whose bad frontmatter is not parsed.
    const QUIRKS: [(&str, &str); 8] = [
        (
            "index.md",
            "---\nokf_version: ~\n---\n* [Local](/local.md)\n* [Edge](/edge.md)\n\
             * [Odd](/odd.md)\n",
        ),
        ("AGENTS.md", "# Agents\n"),
        ("log.md", "# Log\n"),
        (
            "local.md",
            "---\ntype: T\ntitle: L\ndescription: d\n\
             generated: { by: x, at: \"2026-10-20T10:00:00+05:00\" }\n\
             verified: { by: x, at: 2026-10-27T12:02:00Z }\n\
             reviewed: { by: x, at: 2026-10-27T12:02:00.000001Z }\n---\n\
             See [edge](/edge.md#nope) and [odd](/odd.md).\n",
        ),
        (
            "edge.md",
            "---\ntype: \"\"\ntitle: E\ndescription: d\nstatus: ~\n\
             generated: { by: x, at: 2026-10-01T00:01:00Z }\n\
             stale_after: 2026-10-27T12:00:00Z\nsources:\n  - { id: a, resource: \"\" }\n\
             \x20 - { id: b, resource: r, last_modified: ~ }\n\
             \x20 - { id: c, resource: r, last_modified: [1] }\n---\n\
             # Edge\n[local](/local.md) [odd](/odd.md)\n",
        ),
        (
            "odd.md",
            "---\ntype: T\ntitle: O\ndescription: d\ngenerated: { by: x, at: [1] }\n\
             verified: ~\nreviewed: { at: \"2026-10-01T00:00:00+00:00\" }\n\
             stale_after: |\n  2026-01-01T00:00:00Z\n---\n[local](/local.md)\n",
        ),
        (
            "wide.md",
            "---\ntype: T\ntitle: W\ndescription: d\n\
             stale_after: \"\u{ff12}\u{ff10}\u{ff12}\u{ff16}-01-01T00:00:00Z\"\n\
             generated: { by: x, at: \"2026-10-01T00:00:00+00:00\" }\n---\n[local](/local.md)\n",
        ),
        ("sub/index.md", "---\nbad: [x\n---\n"),
    ];

    fn at(text: &str) -> DateTime<FixedOffset> {
        parse_iso(text)
            .and_then(|t| t.aware())
            .expect("aware timestamp")
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
    }

    fn commit_times(times: &[(&str, &str)], dirty: &[&str]) -> CommitTimes {
        CommitTimes::new(
            times.iter().map(|&(p, t)| (p.to_string(), at(t))).collect(),
            dirty.iter().map(|p| p.to_string()).collect(),
        )
    }

    fn mini_commits() -> CommitTimes {
        commit_times(
            &[
                ("notype.md", "2026-10-19T00:00:00Z"),
                ("stamps.md", "2026-10-01T00:00:00Z"),
            ],
            &["stamps.md"],
        )
    }

    fn quirks_commits() -> CommitTimes {
        commit_times(
            &[
                ("local.md", "2026-10-20T00:00:00-04:00"),
                ("edge.md", "2026-10-01T00:00:00Z"),
                ("odd.md", "2026-01-01T00:00:00Z"),
                ("wide.md", "2026-09-30T23:59:00Z"),
            ],
            &[],
        )
    }

    fn lint_files<'a>(
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
        commits: Option<&CommitTimes>,
    ) -> LintReport {
        let snapshot = Snapshot::build(&MemWorkspace::from_files(files)).unwrap();
        lint(&snapshot, now(), commits)
    }

    /// Compares finding by finding, so a failure names the one at fault,
    /// then the whole text, so key order is checked too.
    fn assert_matches_okf_py(report: &LintReport, expected_json: &str) {
        let expected: serde_json::Value = serde_json::from_str(expected_json).unwrap();
        let actual = serde_json::to_value(report).unwrap();
        for list in ["errors", "warnings"] {
            let (a, e) = (
                actual[list].as_array().unwrap(),
                expected[list].as_array().unwrap(),
            );
            for (i, (a, e)) in a.iter().zip(e).enumerate() {
                assert_eq!(a, e, "{list}[{i}]");
            }
            assert_eq!(a.len(), e.len(), "{list} count");
        }
        assert_eq!(actual.to_string(), expected.to_string(), "key order");
    }

    #[test]
    fn mini_catalog_matches_okf_py() {
        // Arrange
        let commits = mini_commits();
        // Act
        let report = lint_files(MINI, Some(&commits));
        // Assert
        assert_matches_okf_py(&report, include_str!("testdata/lint-mini.json"));
    }

    #[test]
    fn quirks_match_okf_py() {
        let commits = quirks_commits();
        let report = lint_files(QUIRKS, Some(&commits));
        assert_matches_okf_py(&report, include_str!("testdata/lint-quirks.json"));
    }

    #[test]
    fn without_git_there_is_no_commit_check() {
        let with_git = lint_files(MINI, Some(&mini_commits()));
        let report = lint_files(MINI, None);
        assert!(with_git
            .warnings
            .iter()
            .any(|w| w.rule == "generated-after-commit"));
        assert!(report
            .warnings
            .iter()
            .all(|w| w.rule != "generated-after-commit"));
        assert_eq!(report.errors, with_git.errors);
    }

    #[test]
    fn root_index_parse_errors_are_reported() {
        let report = lint_files(
            [("index.md", "---\ntitle: R\nokf_version: [x\n---\n")],
            None,
        );
        assert_eq!(
            report.errors,
            [finding(
                "index.md",
                3,
                "frontmatter-parse",
                "frontmatter does not parse: expected ',' or ']' in flow list in '[x'".to_string()
            )]
        );
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn secrets_and_sprawl_are_reported() {
        // Built at runtime so no secret-shaped literal sits in the source.
        let value = format!("Ab{}", "c1".repeat(4));
        let long_body = "line\n".repeat(400);
        let files = [
            (
                "index.md",
                "---\nokf_version: \"0.2\"\n---\n* [A](/a.md)\n".to_string(),
            ),
            (
                "a.md",
                format!("---\ntype: T\ntitle: A\ndescription: d\n---\n{long_body}"),
            ),
            ("notes.txt", format!("password: {value}")),
        ];
        let report = lint_files(files.iter().map(|(p, c)| (*p, c.as_str())), None);
        assert!(report
            .errors
            .iter()
            .any(|e| e.rule == "secret:assignment" && e.file == "notes.txt"));
        let sprawl = report
            .warnings
            .iter()
            .find(|w| w.rule == "sprawl")
            .expect("sprawl warning");
        assert_eq!(
            sprawl.message,
            "sprawl score 20.0 > 16 (400 lines, 0 sections, 0 volatile); consider splitting \
             (okf.py sprawl a.md, okf-reorganize skill)"
        );
    }

    #[test]
    fn non_string_values_are_reported_with_python_repr() {
        let files = [(
            "x.md",
            "---\ntype: T\nstatus: [a, b]\nreviewed: { at: 2026-10-01T00:00:00Z, outcome: [1] }\n---\n",
        )];
        let report = lint_files(files, None);
        let messages: Vec<&str> = report.warnings.iter().map(|w| w.message.as_str()).collect();
        // okf.py raises TypeError on an unhashable status (documented divergence).
        assert!(messages.contains(&"status ['a', 'b'] is not draft, stable or deprecated"));
        assert!(messages.contains(
            &"reviewed.outcome ['1'] is not one of verified, updated, partial, unverifiable"
        ));
    }
}
