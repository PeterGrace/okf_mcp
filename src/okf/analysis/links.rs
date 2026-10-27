//! `okf.py links --json`: broken links and anchors, relative links, orphans,
//! dead ends, index drift and unlinked mentions (`link_analysis`).
//!
//! Unlinked mentions use one `RegexSet` holding every (concept, name)
//! pattern, so each prose line is scanned once for all concepts at once,
//! instead of once per concept per name as in `okf.py`. Concepts are
//! scanned in parallel.

use std::collections::HashSet;
use std::sync::LazyLock;

use rayon::prelude::*;
use regex::{Regex, RegexSet, RegexSetBuilder};
use serde::Serialize;

use crate::okf::markdown::{find_links, split_frontmatter};
use crate::okf::paths::{basename, dirname, join, normpath};
use crate::okf::pystr::{ignorecase_literal, py_lstrip, py_strip, py_unquote};
use crate::okf::snapshot::{DocKind, Snapshot, SnapshotDoc};

/// Names shorter than this (in characters) are never matched as mentions.
pub const MIN_NAME_LEN: usize = 4;
/// Mention hit text is cut to this many characters.
const HIT_TEXT_LEN: usize = 160;

/// A link problem: `{"file", "line", "link"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkIssue {
    /// The linking document.
    pub file: String,
    /// 1-based line of the link.
    pub line: usize,
    /// The link target as written.
    pub link: String,
}

/// An index problem: `{"file", "problem"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexDrift {
    /// The unlisted document, or the index with a misfiled bullet.
    pub file: String,
    /// What is wrong, worded as `okf.py` words it.
    pub problem: String,
}

/// One line mentioning a concept by name: `{"line", "name", "text"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MentionHit {
    /// 1-based line.
    pub line: usize,
    /// The name that matched (stem, title or alias).
    pub name: String,
    /// The line, stripped, cut to 160 characters.
    pub text: String,
}

/// A concept mentioning another without linking to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnlinkedMention {
    /// The mentioning concept.
    pub file: String,
    /// The mentioned concept.
    pub target: String,
    /// Every line with a mention, in order.
    pub hits: Vec<MentionHit>,
}

/// Counts: `concepts`, then one per report list, in `okf.py`'s key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinksSummary {
    /// Number of concepts.
    pub concepts: usize,
    /// `broken.len()`.
    pub broken: usize,
    /// `broken_anchors.len()`.
    pub broken_anchors: usize,
    /// `relative.len()`.
    pub relative: usize,
    /// `orphans.len()`.
    pub orphans: usize,
    /// `dead_ends.len()`.
    pub dead_ends: usize,
    /// `index_drift.len()`.
    pub index_drift: usize,
    /// `unlinked_mentions.len()`.
    pub unlinked_mentions: usize,
}

/// The whole report, keys in `okf.py`'s order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinksReport {
    /// Links whose target does not exist.
    pub broken: Vec<LinkIssue>,
    /// Links whose `#fragment` names no heading or anchor in the target.
    pub broken_anchors: Vec<LinkIssue>,
    /// Relative links in concepts (bundle-root absolute paths are preferred).
    pub relative: Vec<LinkIssue>,
    /// Concepts no other concept links to.
    pub orphans: Vec<String>,
    /// Concepts that link to no other concept.
    pub dead_ends: Vec<String>,
    /// Documents missing from their folder's `index.md`, and misfiled bullets.
    pub index_drift: Vec<IndexDrift>,
    /// Name mentions without a link (candidates only).
    pub unlinked_mentions: Vec<UnlinkedMention>,
    /// Counts.
    pub summary: LinksSummary,
}

/// Runs the link analysis.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `mentions` - also look for unlinked mentions (`lint` passes `false`).
///
/// # Returns
/// The report; `okf.py links --json` prints the same JSON.
pub fn links_report(snapshot: &Snapshot, mentions: bool) -> LinksReport {
    let graph: Vec<&SnapshotDoc> = snapshot
        .docs()
        .iter()
        .filter(|d| d.in_link_graph())
        .collect();
    let concepts: Vec<&SnapshotDoc> = graph
        .iter()
        .copied()
        .filter(|d| d.kind() == DocKind::Concept)
        .collect();
    let concept_paths: HashSet<&str> = concepts.iter().map(|d| d.path()).collect();

    let mut report = LinksReport::default();
    let mut has_inbound: HashSet<&str> = HashSet::new();
    for doc in &graph {
        let in_concept = doc.kind() == DocKind::Concept;
        for link in doc.links() {
            // A closure (Python's `lambda`) that borrows `doc` and `link`;
            // it builds the record only for the lists that need it.
            let issue = || LinkIssue {
                file: doc.path().to_string(),
                line: link.line,
                link: link.raw.clone(),
            };
            if !link.target_exists {
                report.broken.push(issue());
                continue;
            }
            if let Some(fragment) = link.fragment.as_deref().filter(|f| !f.is_empty()) {
                if link.target.ends_with(".md") && link.target_is_file {
                    let wanted = py_unquote(fragment).to_lowercase();
                    let found = snapshot
                        .anchors_of(&link.target)
                        // `is_some_and` is `x is not None and pred(x)`. A target
                        // the snapshot couldn't read has no anchors, so the
                        // fragment counts as broken.
                        .is_some_and(|anchors| anchors.contains(&wanted));
                    if !found {
                        report.broken_anchors.push(issue());
                    }
                }
            }
            if in_concept && !link.absolute {
                report.relative.push(issue());
            }
            if in_concept
                && link.target != doc.path()
                && concept_paths.contains(link.target.as_str())
            {
                has_inbound.insert(link.target.as_str());
            }
        }
    }

    for doc in &concepts {
        if !has_inbound.contains(doc.path()) {
            report.orphans.push(doc.path().to_string());
        }
        let has_outbound = doc
            .links()
            .iter()
            .any(|l| l.target != doc.path() && concept_paths.contains(l.target.as_str()));
        if !has_outbound {
            report.dead_ends.push(doc.path().to_string());
        }
    }

    report.index_drift = index_drift(snapshot, &graph, &concept_paths);
    if mentions {
        report.unlinked_mentions = unlinked_mentions(&concepts);
    }
    report.summary = LinksSummary {
        concepts: concepts.len(),
        broken: report.broken.len(),
        broken_anchors: report.broken_anchors.len(),
        relative: report.relative.len(),
        orphans: report.orphans.len(),
        dead_ends: report.dead_ends.len(),
        index_drift: report.index_drift.len(),
        unlinked_mentions: report.unlinked_mentions.len(),
    };
    report
}

/// `^#+\s` (Python `\s`).
static INDEX_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#+[\s\x1c-\x1f]").expect("INDEX_HEADING regex is valid"));

/// Every concept listed by its folder's `index.md`, every folder index by
/// its parent's; and no concept bullet under an index's `# Sections`.
fn index_drift(
    snapshot: &Snapshot,
    graph: &[&SnapshotDoc],
    concept_paths: &HashSet<&str>,
) -> Vec<IndexDrift> {
    let mut drift = Vec::new();
    for doc in graph {
        let parent = match doc.kind() {
            DocKind::Log | DocKind::Machinery => continue,
            _ if doc.path() == "index.md" => continue,
            DocKind::Index => dirname(dirname(doc.path())),
            DocKind::Concept => dirname(doc.path()),
        };
        let listed_in = normpath(&join(parent, "index.md"));
        let index = snapshot.get(&listed_in).filter(|d| d.in_link_graph());
        let problem = match index {
            None => format!("no {listed_in} to list it"),
            Some(index) if !index.links().iter().any(|l| l.target == doc.path()) => {
                format!("not listed in {listed_in}")
            }
            Some(_) => continue,
        };
        drift.push(IndexDrift {
            file: doc.path().to_string(),
            problem,
        });
    }
    for index in graph.iter().filter(|d| d.kind() == DocKind::Index) {
        let mut heading: Option<String> = None;
        let mut section_bullets = HashSet::new();
        // `(1..)` is an endless counter; `zip` pairs it with each line,
        // like Python's `enumerate(lines, 1)`.
        for (number, line) in (1..).zip(index.text().split('\n')) {
            if INDEX_HEADING.is_match(line) {
                heading = Some(py_strip(line.trim_start_matches('#')).to_lowercase());
            } else if heading.as_deref() == Some("sections")
                && (line.starts_with("* ") || line.starts_with("- "))
            {
                section_bullets.insert(number);
            }
        }
        let misfiled = index.links().iter().filter(|l| {
            !l.in_page
                && section_bullets.contains(&l.line)
                && concept_paths.contains(l.target.as_str())
        });
        for link in misfiled {
            drift.push(IndexDrift {
                file: index.path().to_string(),
                problem: format!(
                    "line {}: concept /{} is listed under # Sections (which lists folders); \
                     move it to # Concepts",
                    link.line, link.target
                ),
            });
        }
    }
    drift
}

/// `names_for`: the file stem, the title and the aliases, at least
/// [`MIN_NAME_LEN`] characters long, sorted and without duplicates.
fn names_for(doc: &SnapshotDoc) -> Vec<String> {
    // `os.path.basename(p)[:-3]`: drop one `.md` (every document has it).
    // `strip_suffix` removes it once; `trim_end_matches` would strip `a.md.md`
    // down to `a`.
    let name = basename(doc.path());
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let mut names: Vec<String> = std::iter::once(stem)
        .chain(doc.title())
        .chain(doc.aliases().iter().map(String::as_str))
        .filter(|name| name.chars().count() >= MIN_NAME_LEN)
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `(?<![\w/-])NAME(?![\w-])` with `re.I`, lookaround emulated by consumed
/// neighbour characters (an existence test only needs that).
fn mention_pattern(name: &str) -> String {
    format!(
        r"(?:\A|[^\p{{L}}\p{{N}}_/\-])(?i:{})(?:[^\p{{L}}\p{{N}}_\-]|\z)",
        ignorecase_literal(name)
    )
}

/// Every concept's names as one `RegexSet`.
// `<'a>` is a lifetime parameter: the matcher borrows the concept paths from
// the snapshot instead of copying them, and the compiler checks that it never
// outlives the snapshot.
struct MentionMatcher<'a> {
    /// `(concept path, its names)`, in concept order.
    targets: Vec<(&'a str, Vec<String>)>,
    /// For each set pattern: `(index into targets, index into its names)`.
    owners: Vec<(usize, usize)>,
    /// One [`mention_pattern`] per `owners` entry, in the same order.
    set: RegexSet,
}

impl<'a> MentionMatcher<'a> {
    /// Builds the set from every concept's [`names_for`]; concepts without
    /// a long enough name are left out, as `okf.py` leaves them out of
    /// `patterns`.
    fn new(concepts: &[&'a SnapshotDoc]) -> Self {
        let targets: Vec<(&str, Vec<String>)> = concepts
            .iter()
            .map(|doc| (doc.path(), names_for(doc)))
            .filter(|(_, names)| !names.is_empty())
            .collect();
        let mut owners = Vec::new();
        let mut patterns = Vec::new();
        for (target, (_, names)) in targets.iter().enumerate() {
            for (name_index, name) in names.iter().enumerate() {
                owners.push((target, name_index));
                patterns.push(mention_pattern(name));
            }
        }
        // Hundreds of patterns with Unicode classes outgrow the default
        // 10 MiB compile limit. 256 MiB still bounds compile memory. On the
        // real catalog (143 concepts) the whole report, set compile
        // included, takes about 150 ms in release mode; a catalog that
        // exceeds the limit needs the two-stage matcher of plan Task 7 step 5.
        let set = RegexSetBuilder::new(patterns)
            .size_limit(256 << 20)
            .dfa_size_limit(256 << 20)
            .build()
            .expect("escaped mention patterns always compile");
        Self {
            targets,
            owners,
            set,
        }
    }
}

/// `link_analysis`'s mention pass: for each concept (in walk order), every
/// other concept it names in prose without linking to it.
fn unlinked_mentions(concepts: &[&SnapshotDoc]) -> Vec<UnlinkedMention> {
    let matcher = MentionMatcher::new(concepts);
    // One task per mentioning concept; `collect` keeps concept order.
    let per_source: Vec<Vec<UnlinkedMention>> = concepts
        .par_iter()
        .map(|source| mentions_in(source, &matcher))
        .collect();
    per_source.into_iter().flatten().collect()
}

/// The unlinked mentions in one concept, in target order (`okf.py`'s
/// `patterns` order), each with its hits in line order.
fn mentions_in(source: &SnapshotDoc, matcher: &MentionMatcher<'_>) -> Vec<UnlinkedMention> {
    let linked: HashSet<&str> = source.links().iter().map(|l| l.target.as_str()).collect();
    let mut hits: Vec<Vec<MentionHit>> = vec![Vec::new(); matcher.targets.len()];
    for (line, masked, original) in prose_lines(source.text()) {
        let matched = matcher.set.matches(&masked);
        let mut previous_target = None;
        // Ascending pattern order: a target's names are contiguous and
        // sorted, so the first match per target is `okf.py`'s first name.
        for pattern in matched.iter() {
            let (target, name) = matcher.owners[pattern];
            if previous_target == Some(target) {
                continue;
            }
            previous_target = Some(target);
            let (target_path, names) = &matcher.targets[target];
            if *target_path == source.path() || linked.contains(target_path) {
                continue;
            }
            hits[target].push(MentionHit {
                line,
                name: names[name].clone(),
                text: truncate_chars(py_strip(original), HIT_TEXT_LEN).to_string(),
            });
        }
    }
    matcher
        .targets
        .iter()
        .zip(hits)
        .filter(|(_, hits)| !hits.is_empty())
        .map(|((target, _), hits)| UnlinkedMention {
            file: source.path().to_string(),
            target: (*target).to_string(),
            hits,
        })
        .collect()
}

/// An inline code span, `` `[^`]*` ``.
// `LazyLock` compiles each regex once, on first use, like a module-level
// `re.compile` in Python.
static CODE_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`[^`]*`").expect("CODE_SPAN regex is valid"));
/// `https?://\S+` (Python's `\S` excludes `\x1c-\x1f` too).
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s\x1c-\x1f]+").expect("URL regex is valid"));
/// A footnote definition line, `^\[\^[^\]]+\]:`.
static FOOTNOTE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\^[^\]]+\]:").expect("FOOTNOTE_LINE regex is valid"));

/// `prose_lines`: `(file line, masked line, original line)` for body prose,
/// skipping fenced code and footnote definitions, with code spans, links and
/// URLs replaced by a space.
fn prose_lines(text: &str) -> Vec<(usize, String, &str)> {
    let (_, body, offset) = split_frontmatter(text);
    let mut in_fence = false;
    let mut out = Vec::new();
    for (index, line) in body.split('\n').enumerate() {
        if py_lstrip(line).starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || FOOTNOTE_LINE.is_match(line) {
            continue;
        }
        let masked = CODE_SPAN.replace_all(line, " ");
        let masked = mask_links(&masked);
        let masked = URL.replace_all(&masked, " ").into_owned();
        out.push((offset + index + 1, masked, line));
    }
    out
}

/// `LINK_RE.sub(" ", text)`.
fn mask_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for link in find_links(text) {
        out.push_str(&text[copied..link.range.start]);
        out.push(' ');
        copied = link.range.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// The first `n` characters (Python's `s[:n]`).
fn truncate_chars(text: &str, n: usize) -> &str {
    text.char_indices()
        .nth(n)
        .map_or(text, |(cut, _)| &text[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn report(files: &[(&str, &str)]) -> LinksReport {
        let workspace = MemWorkspace::from_files(files.iter().copied());
        links_report(&Snapshot::build(&workspace).expect("snapshot"), true)
    }

    const CATALOG: [(&str, &str); 5] = [
        (
            "index.md",
            "---\nokf_version: \"0.2\"\n---\n# Sections\n\n* [Infra](infra/)\n* [Wrong](/infra/a.md)\n",
        ),
        ("infra/index.md", "# Concepts\n\n* [A](/infra/a.md)\n"),
        (
            "infra/a.md",
            "---\ntype: Host\ntitle: Alpha Server\n---\n# Ports\n[b](b.md) [gone](/infra/gone.md) \
             [p](/infra/b.md#Missing%20Part) [q](/infra/b.md#real-part) [self](#ports)\n",
        ),
        (
            "infra/b.md",
            "---\ntype: Host\n---\n# Real Part\nThe Alpha Server runs `alpha server` here.\n",
        ),
        ("AGENTS.md", "[x](/missing.md)\n"),
    ];

    fn issue(file: &str, line: usize, link: &str) -> LinkIssue {
        LinkIssue {
            file: file.to_string(),
            line,
            link: link.to_string(),
        }
    }

    #[test]
    fn link_problems_match_okf_py() {
        let report = report(&CATALOG);
        assert_eq!(report.broken, [issue("infra/a.md", 6, "/infra/gone.md")]);
        assert_eq!(
            report.broken_anchors,
            [issue("infra/a.md", 6, "/infra/b.md#Missing%20Part")]
        );
        assert_eq!(report.relative, [issue("infra/a.md", 6, "b.md")]);
        assert_eq!(report.orphans, ["infra/a.md"]);
        assert_eq!(report.dead_ends, ["infra/b.md"]);
    }

    #[test]
    fn index_drift_lists_unlisted_and_misfiled_concepts() {
        let problems: Vec<(String, String)> = report(&CATALOG)
            .index_drift
            .into_iter()
            .map(|d| (d.file, d.problem))
            .collect();
        assert_eq!(
            problems,
            [
                (
                    "infra/b.md".to_string(),
                    "not listed in infra/index.md".to_string()
                ),
                (
                    "index.md".to_string(),
                    "line 7: concept /infra/a.md is listed under # Sections (which lists \
                     folders); move it to # Concepts"
                        .to_string()
                ),
            ]
        );
    }

    #[test]
    fn unlinked_mentions_skip_code_and_report_the_first_name() {
        let report = report(&CATALOG);
        assert_eq!(report.unlinked_mentions.len(), 1);
        let mention = &report.unlinked_mentions[0];
        assert_eq!(
            (mention.file.as_str(), mention.target.as_str()),
            ("infra/b.md", "infra/a.md")
        );
        assert_eq!(
            mention.hits,
            [MentionHit {
                line: 5,
                name: "Alpha Server".to_string(),
                text: "The Alpha Server runs `alpha server` here.".to_string(),
            }]
        );
        assert_eq!(
            report.summary,
            LinksSummary {
                concepts: 2,
                broken: 1,
                broken_anchors: 1,
                relative: 1,
                orphans: 1,
                dead_ends: 1,
                index_drift: 2,
                unlinked_mentions: 1,
            }
        );
    }

    #[test]
    fn mentions_can_be_turned_off() {
        let workspace = MemWorkspace::from_files(CATALOG);
        let report = links_report(&Snapshot::build(&workspace).expect("snapshot"), false);
        assert!(report.unlinked_mentions.is_empty());
        assert_eq!(report.summary.unlinked_mentions, 0);
    }

    #[test]
    fn names_for_drops_one_md_and_short_or_repeated_names() {
        // Arrange: a stem ending in `.md.md`, a repeated title and a short alias.
        let workspace = MemWorkspace::from_files([(
            "infra/beta.md.md",
            "---\ntitle: Zeta\naliases: [beta.md, abc, Zeta, \u{c9}clair]\n---\n",
        )]);
        let snapshot = Snapshot::build(&workspace).expect("snapshot");
        let doc = snapshot.get("infra/beta.md.md").expect("the concept");

        // Act
        let names = names_for(doc);

        // Assert: okf.py's names_for on the same document.
        assert_eq!(names, ["Zeta", "beta.md", "\u{c9}clair"]);
    }

    #[test]
    fn mention_boundaries_follow_okf_py() {
        let pattern = Regex::new(&mention_pattern("Time Server")).unwrap();
        for hit in [
            "the time server.",
            "TIME SERVER",
            "(time server)",
            "t\u{131}me server",
        ] {
            assert!(pattern.is_match(hit), "{hit}");
        }
        for miss in [
            "time servers",
            "/time server",
            "x-time server",
            "time server-x",
            "time_server",
        ] {
            assert!(!pattern.is_match(miss), "{miss}");
        }
    }

    #[test]
    fn prose_lines_mask_code_links_and_urls() {
        let lines = prose_lines(
            "---\nk: v\n---\na `b` [c](/d.md) https://e.f/g h\n```\ncode\n```\n[^1]: n\n",
        );
        // From okf.py's prose_lines on the same text: the fence and the
        // footnote are skipped, the empty last line stays.
        let numbered: Vec<(usize, &str)> = lines.iter().map(|(n, m, _)| (*n, m.as_str())).collect();
        assert_eq!(numbered, [(4, "a       h"), (9, "")]);
    }
}
