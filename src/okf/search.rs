//! Ranked full-text search within one catalog snapshot.
//!
//! A concept matches when every whitespace-separated term of the query
//! occurs (case-insensitively, as a substring) in at least one of its fields.
//! Fields, best first: title or alias, tag, description, body. The tier is
//! the best field holding the whole query (terms joined by one space), else
//! 4 plus the best field holding any term. Deprecated concepts sort after the
//! rest; ties break by path.
//!
//! `okf.py` has no search command, so there is nothing to keep parity with:
//! the rules above (decision 3 of the M2 plan) and this module's tests are
//! the specification. M3's `search` tool runs [`search`] per catalog and
//! merges the hits by [`SearchHit::rank`], then path.

use std::cmp::Ordering;

// `rayon::prelude` brings `par_iter` into scope (trait methods are only
// callable when the trait is imported).
use rayon::prelude::*;
use serde::Serialize;

use crate::okf::markdown::split_frontmatter;
use crate::okf::pystr::py_strip;
use crate::okf::snapshot::{Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// Snippets are cut to this many characters.
const SNIPPET_LEN: usize = 160;

/// The ranked fields. The discriminants are the tiers (`field as u8`), and
/// the derived `Ord` follows declaration order, so `Names` is the best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Names = 0,
    Tags = 1,
    Description = 2,
    Body = 3,
}

impl Field {
    /// Every field, best first.
    const ALL: [Field; 4] = [Field::Names, Field::Tags, Field::Description, Field::Body];
    /// Added to the tier when only single terms (not the whole query) match.
    const COUNT: u8 = Field::ALL.len() as u8;
}

/// A concept's lowercased search fields, built once per snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFields {
    /// Title and aliases.
    names: Vec<String>,
    tags: Vec<String>,
    description: String,
    body: String,
    concept_type: String,
    /// `status`, or `stable` when it is missing, empty or not a string.
    status: String,
}

impl SearchFields {
    /// Builds the fields from a parsed concept.
    ///
    /// # Arguments
    /// * `doc` - the concept; its title and aliases must already be set.
    ///
    /// # Returns
    /// The lowercased fields. Non-string `type`, `description` and `status`
    /// count as empty; a scalar `tags` counts as a one-item list.
    pub(crate) fn of(doc: &SnapshotDoc) -> Self {
        let data = doc.frontmatter().data();
        // A closure (Python lambda) capturing `data`; `unwrap_or_default` on
        // an `Option<&str>` gives `""` for a missing key.
        let text = |key: &str| {
            data.and_then(|d| d.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
        };
        let tags = match data.and_then(|d| d.get("tags")) {
            Some(Value::Seq(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_lowercase)
                .collect(),
            Some(Value::Str(tag)) => vec![tag.to_lowercase()],
            Some(Value::Null | Value::Map(_)) | None => Vec::new(),
        };
        // `Option::into_iter` yields zero or one item, so the title (if any)
        // and the aliases chain into one list.
        let names = doc
            .title()
            .into_iter()
            .chain(doc.aliases().iter().map(String::as_str))
            .map(str::to_lowercase)
            .collect();
        let status = text("status");
        Self {
            names,
            tags,
            description: text("description"),
            body: split_frontmatter(doc.text()).1.to_lowercase(),
            concept_type: text("type"),
            status: if status.is_empty() {
                "stable".to_string()
            } else {
                status
            },
        }
    }

    /// Does `field` contain the lowercased `needle`?
    fn contains(&self, field: Field, needle: &str) -> bool {
        match field {
            Field::Names => self.names.iter().any(|n| n.contains(needle)),
            Field::Tags => self.tags.iter().any(|t| t.contains(needle)),
            Field::Description => self.description.contains(needle),
            Field::Body => self.body.contains(needle),
        }
    }

    /// The best field containing `needle`, or `None` if none does.
    fn best_field(&self, needle: &str) -> Option<Field> {
        Field::ALL
            .into_iter()
            .find(|&field| self.contains(field, needle))
    }
}

/// A search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Free text; empty (or only whitespace) matches every concept that
    /// passes the filters.
    pub text: String,
    /// Only concepts of this `type` (case-insensitive).
    pub concept_type: Option<String>,
    /// Only concepts carrying every one of these tags (case-insensitive,
    /// whole tags).
    pub tags: Vec<String>,
    /// Only concepts with this `status` (case-insensitive; missing counts as
    /// `stable`).
    pub status: Option<String>,
    /// At most this many hits.
    pub limit: usize,
}

impl SearchQuery {
    /// A query for `text` with no filters and the design's default limit (20).
    ///
    /// # Arguments
    /// * `text` - the free text.
    ///
    /// # Returns
    /// The query; set the public fields to add filters or change the limit.
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            concept_type: None,
            tags: Vec::new(),
            status: None,
            limit: 20,
        }
    }
}

/// How a hit ranks. The derived `Ord` compares fields in declaration order,
/// so it sorts best first: non-deprecated before deprecated, then by tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchRank {
    /// Deprecated concepts sort after all others (`false < true`).
    pub deprecated: bool,
    /// 0-3 for the field holding the whole query, 4-7 for the best field
    /// holding a term (0 = names, 1 = tags, 2 = description, 3 = body).
    pub tier: u8,
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    /// `/`-prefixed concept path.
    pub path: String,
    /// The concept's title.
    pub title: Option<String>,
    /// Its description, as written.
    pub description: Option<String>,
    /// The first body line holding the whole query, else the first holding
    /// any term, stripped and cut to 160 characters; `None` for an empty
    /// query or when no body line matches.
    pub snippet: Option<String>,
    /// The ranking key (M3 merges catalogs by it); not serialized.
    #[serde(skip)]
    pub rank: SearchRank,
}

/// The query, lowercased once.
struct Prepared {
    terms: Vec<String>,
    /// The terms joined by one space.
    phrase: String,
}

/// Searches the concepts of one snapshot.
///
/// # Arguments
/// * `snapshot` - the catalog.
/// * `query` - text, filters and limit.
///
/// # Returns
/// At most `query.limit` hits, best first ([`SearchRank`], then path).
pub fn search(snapshot: &Snapshot, query: &SearchQuery) -> Vec<SearchHit> {
    if query.limit == 0 {
        return Vec::new();
    }
    let terms: Vec<String> = query
        .text
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let prepared = Prepared {
        phrase: terms.join(" "),
        terms,
    };
    let wanted_tags: Vec<String> = query.tags.iter().map(|t| t.to_lowercase()).collect();
    let wanted_type = query.concept_type.as_deref().map(str::to_lowercase);
    let wanted_status = query.status.as_deref().map(str::to_lowercase);

    // Matching is independent per document, so it runs in parallel; the
    // `?` inside `filter_map`'s closure returns `None` (skip) early.
    let mut ranked: Vec<(SearchRank, &SnapshotDoc, &SearchFields)> = snapshot
        .docs()
        .par_iter()
        .filter_map(|doc| {
            let fields = doc.search_fields()?;
            let filtered_out = wanted_type
                .as_ref()
                .is_some_and(|t| *t != fields.concept_type)
                || wanted_status.as_ref().is_some_and(|s| *s != fields.status)
                || !wanted_tags.iter().all(|t| fields.tags.contains(t));
            if filtered_out {
                return None;
            }
            let rank = SearchRank {
                deprecated: fields.status == "deprecated",
                tier: tier(fields, &prepared)?,
            };
            Some((rank, doc, fields))
        })
        .collect();

    let order = |a: &(SearchRank, &SnapshotDoc, &SearchFields),
                 b: &(SearchRank, &SnapshotDoc, &SearchFields)|
     -> Ordering { a.0.cmp(&b.0).then_with(|| a.1.path().cmp(b.1.path())) };
    // Only the best `limit` need sorting: partition around the cut first
    // (average O(n)), then sort the kept `limit` items.
    if ranked.len() > query.limit {
        ranked.select_nth_unstable_by(query.limit - 1, order);
        ranked.truncate(query.limit);
    }
    // Paths are unique, so the order is total and an unstable sort is exact.
    ranked.sort_unstable_by(order);
    ranked
        .into_iter()
        .map(|(rank, doc, fields)| hit(doc, fields, rank, &prepared))
        .collect()
}

/// The tier, or `None` when some term occurs in no field.
fn tier(fields: &SearchFields, query: &Prepared) -> Option<u8> {
    if query.terms.is_empty() {
        return Some(0);
    }
    // The best field holding any term is the best of each term's best field;
    // `try_fold` stops with `None` as soon as one term is found nowhere.
    let best_term = query.terms.iter().try_fold(Field::Body, |best, term| {
        fields.best_field(term).map(|field| best.min(field))
    })?;
    // A one-term phrase is the term itself.
    if query.terms.len() == 1 {
        return Some(best_term as u8);
    }
    // A field holding the phrase holds every term, so no field better than
    // `best_term` can hold it: start the phrase search there.
    let phrase_field = Field::ALL[best_term as usize..]
        .iter()
        .find(|&&field| fields.contains(field, &query.phrase));
    Some(phrase_field.map_or(Field::COUNT + best_term as u8, |&field| field as u8))
}

/// Builds the result for a ranked concept.
fn hit(doc: &SnapshotDoc, fields: &SearchFields, rank: SearchRank, query: &Prepared) -> SearchHit {
    let description = doc
        .frontmatter()
        .data()
        .and_then(|d| d.get("description"))
        .and_then(Value::as_str)
        .map(str::to_string);
    SearchHit {
        path: format!("/{}", doc.path()),
        title: doc.title().map(str::to_string),
        description,
        snippet: snippet(split_frontmatter(doc.text()).1, &fields.body, query),
        rank,
    }
}

/// The first body line holding the phrase, else the first holding any term.
///
/// `lowered` is `body.to_lowercase()`. Lowercasing never adds or removes a
/// `\n`, so the two split into the same lines and can be zipped, which avoids
/// lowercasing the body again.
fn snippet(body: &str, lowered: &str, query: &Prepared) -> Option<String> {
    if query.terms.is_empty() {
        return None;
    }
    let mut first_term_line = None;
    for (line, lower) in body.split('\n').zip(lowered.split('\n')) {
        if lower.contains(query.phrase.as_str()) {
            return Some(cut(line));
        }
        if first_term_line.is_none() && query.terms.iter().any(|t| lower.contains(t.as_str())) {
            first_term_line = Some(line);
        }
    }
    first_term_line.map(cut)
}

/// A line stripped and cut to [`SNIPPET_LEN`] characters (not bytes).
fn cut(line: &str) -> String {
    let text = py_strip(line);
    // `char_indices().nth(n)` finds the byte offset of the n-th character, so
    // the cut never splits a multi-byte UTF-8 character.
    text.char_indices()
        .nth(SNIPPET_LEN)
        .map_or(text, |(end, _)| &text[..end])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn snapshot() -> Snapshot {
        Snapshot::build(&MemWorkspace::from_files([
            (
                "infra/time-server.md",
                "---\ntype: Service\ntitle: GPS time server\ndescription: Stratum 1 NTP.\n\
                 aliases: [NTP server]\ntags: [ntp, time]\n---\nServes time to the LAN.\n",
            ),
            (
                "infra/clock.md",
                "---\ntype: Host\ntitle: Wall clock\ndescription: Shows the time server's time.\n\
                 tags: [Time]\n---\nA clock.\n",
            ),
            (
                "infra/old-ntp.md",
                "---\ntype: Service\ntitle: Old NTP server\nstatus: deprecated\n---\nRetired.\n",
            ),
            (
                "notes/body.md",
                "---\ntype: Note\ntitle: Notes\n---\nfirst line\nWe asked the TIME SERVER twice.\n",
            ),
            ("index.md", "time server\n"),
        ]))
        .unwrap()
    }

    fn paths(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.path.as_str()).collect()
    }

    #[test]
    fn ranks_by_field_then_deprecated_last() {
        // The phrase is in time-server's title (tier 0), clock's description
        // (tier 2) and body.md's body (tier 3); old-ntp has no "time" at all.
        let hits = search(&snapshot(), &SearchQuery::new("time server"));
        assert_eq!(
            paths(&hits),
            ["/infra/time-server.md", "/infra/clock.md", "/notes/body.md"]
        );
        let tiers: Vec<u8> = hits.iter().map(|h| h.rank.tier).collect();
        assert_eq!(tiers, [0, 2, 3]);
        // Both match "ntp server" in a name (tier 0); the deprecated one sorts last.
        let hits = search(&snapshot(), &SearchQuery::new("ntp server"));
        assert_eq!(paths(&hits), ["/infra/time-server.md", "/infra/old-ntp.md"]);
        assert!(hits[1].rank.deprecated && hits[1].rank.tier == 0);
    }

    #[test]
    fn every_term_must_match_somewhere() {
        assert!(search(&snapshot(), &SearchQuery::new("time banana")).is_empty());
        assert_eq!(
            paths(&search(&snapshot(), &SearchQuery::new("lan gps"))),
            ["/infra/time-server.md"]
        );
    }

    #[test]
    fn split_terms_rank_by_their_best_field_plus_four() {
        // "lan" is only in the body, "gps" in the title, the phrase nowhere.
        let hits = search(&snapshot(), &SearchQuery::new("lan gps"));
        assert_eq!(hits[0].rank.tier, 4);
        // "clock" is clock's title, "shows" its description: names win.
        let hits = search(&snapshot(), &SearchQuery::new("shows clock"));
        assert_eq!(paths(&hits), ["/infra/clock.md"]);
        assert_eq!(hits[0].rank.tier, 4);
        // One term is its own phrase, so it ranks in 0-3.
        let hits = search(&snapshot(), &SearchQuery::new("RETIRED"));
        assert_eq!(paths(&hits), ["/infra/old-ntp.md"]);
        assert_eq!(hits[0].rank.tier, 3);
    }

    #[test]
    fn snippets_show_the_first_matching_body_line() {
        let hits = search(&snapshot(), &SearchQuery::new("time server"));
        let body = hits.iter().find(|h| h.path == "/notes/body.md").unwrap();
        assert_eq!(
            body.snippet.as_deref(),
            Some("We asked the TIME SERVER twice.")
        );
        assert_eq!(hits[0].title.as_deref(), Some("GPS time server"));
        assert_eq!(hits[0].description.as_deref(), Some("Stratum 1 NTP."));
        // No body line holds the phrase, so the first line with a term wins.
        let hits = search(&snapshot(), &SearchQuery::new("lan gps"));
        assert_eq!(hits[0].snippet.as_deref(), Some("Serves time to the LAN."));
        // An empty query has no snippet.
        assert!(search(&snapshot(), &SearchQuery::new(""))
            .iter()
            .all(|h| h.snippet.is_none()));
    }

    #[test]
    fn snippets_are_stripped_and_cut_by_characters() {
        let line = "\u{e9}".repeat(200);
        let text = format!("---\ntitle: Long\n---\n  {line}  \n");
        let snapshot =
            Snapshot::build(&MemWorkspace::from_files([("a.md", text.as_str())])).unwrap();
        let hits = search(&snapshot, &SearchQuery::new("\u{c9}"));
        let snippet = hits[0].snippet.as_deref().unwrap();
        assert_eq!(snippet, "\u{e9}".repeat(SNIPPET_LEN));
    }

    #[test]
    fn filters_and_limit() {
        let mut query = SearchQuery::new("");
        query.tags = vec!["TIME".to_string()];
        assert_eq!(
            paths(&search(&snapshot(), &query)),
            ["/infra/clock.md", "/infra/time-server.md"]
        );
        let mut query = SearchQuery::new("server");
        query.concept_type = Some("service".to_string());
        query.status = Some("stable".to_string());
        assert_eq!(
            paths(&search(&snapshot(), &query)),
            ["/infra/time-server.md"]
        );
        let mut query = SearchQuery::new("time");
        query.limit = 1;
        assert_eq!(
            paths(&search(&snapshot(), &query)),
            ["/infra/time-server.md"]
        );
        query.limit = 0;
        assert!(search(&snapshot(), &query).is_empty());
    }

    #[test]
    fn only_concepts_carry_search_fields() {
        let snapshot = snapshot();
        assert!(snapshot.get("index.md").unwrap().search_fields().is_none());
        assert!(snapshot
            .get("notes/body.md")
            .unwrap()
            .search_fields()
            .is_some());
    }

    #[test]
    fn hits_serialize_without_the_rank() {
        let hits = search(&snapshot(), &SearchQuery::new("retired"));
        assert_eq!(
            serde_json::to_string(&hits[0]).unwrap(),
            r#"{"path":"/infra/old-ntp.md","title":"Old NTP server","description":null,"snippet":"Retired."}"#
        );
    }
}
