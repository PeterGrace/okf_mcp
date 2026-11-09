//! Catalog-only proposals: IDs map to supplied immutable evidence, never tools.

pub use super::types::{Proposal, ProposedEvidence, ProposedFinding};

use std::{borrow::Cow, fmt};

use super::{
    collect::{bounded_packet, clean},
    error::RunnerError,
    types::Packet,
};
use crate::error::OkfError;
use crate::maintenance::{
    types::{Assessment, Evidence, FindingInput, Lane, Relationship, Submission, WorkOutcome},
    validate,
};
use serde_json::{json, Value};

/// Decode only a proposal, never a provider envelope or accounting fields.
/// Shape inspection rejects Serde's positional struct arrays; the second decode
/// uses the original bytes so duplicate fields cannot disappear through Value.
pub(crate) fn decode_proposal(bytes: &[u8]) -> Result<Proposal, RunnerError> {
    let invalid = RunnerError::InvalidProposal;
    let shape: Value = serde_json::from_slice(bytes).map_err(|_| invalid)?;
    if !shape.is_object() {
        return Err(invalid);
    }
    let findings = shape
        .get("findings")
        .and_then(Value::as_array)
        .ok_or(invalid)?;
    for finding in findings {
        if !finding.is_object()
            || !finding.get("category").is_some_and(Value::is_string)
            || !finding.get("relationship").is_some_and(Value::is_string)
        {
            return Err(invalid);
        }
        let evidence = finding
            .get("evidence")
            .and_then(Value::as_array)
            .ok_or(invalid)?;
        if !evidence.iter().all(Value::is_object) {
            return Err(invalid);
        }
    }
    serde_json::from_slice(bytes).map_err(|_| invalid)
}

/// Closed model output shape. Byte and contextual limits are enforced after mapping.
pub fn proposal_schema() -> Value {
    json!({
        "type":"object", "additionalProperties":false,
        "required":["summary","findings"],
        "properties":{
            "summary":{"type":"string"},
            "findings":{"type":"array","maxItems":20,"items":{
                "type":"object","additionalProperties":false,
                "required":["category","assertion","follow_up","uncertainty","relationship","evidence"],
                "properties":{
                    "category":{"type":"string","enum":["suggested_link","possible_duplicate","conflicting_assertions","potentially_outdated_information","unverifiable_claim"]},
                    "assertion":{"type":"string"},
                    "follow_up":{"type":"string"},
                    "uncertainty":{"type":"string"},
                    "relationship":{"type":"string","enum":["direct_statement","inference"]},
                    "evidence":{"type":"array","minItems":1,"maxItems":20,"items":{
                        "type":"object","additionalProperties":false,
                        "required":["source_id","excerpt"],
                        "properties":{"source_id":{"type":"string"},"excerpt":{"type":"string"}}
                    }}
                }
            }}
        }
    })
}

/// Where a rejected proposal failed. Indices are 0-based, as in the JSON arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionSite {
    /// The evidence packet the proposal answers, not the model's output.
    Packet,
    /// The proposal or the mapped submission as a whole.
    Proposal,
    /// The proposal's `summary`.
    Summary,
    /// One finding, by index.
    Finding(usize),
    /// One evidence entry of one finding, by indices.
    Evidence {
        /// Index of the finding.
        finding: usize,
        /// Index of the evidence entry within that finding.
        evidence: usize,
    },
}

impl fmt::Display for RejectionSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packet => f.write_str("packet"),
            Self::Proposal => f.write_str("proposal"),
            Self::Summary => f.write_str("summary"),
            Self::Finding(finding) => write!(f, "finding {finding}"),
            Self::Evidence { finding, evidence } => {
                write!(f, "finding {finding} evidence {evidence}")
            }
        }
    }
}

/// Why [`to_submission`] rejected a proposal: the site and the failed check.
///
/// The rendered text (`Display`) holds only indices, packet source ids and
/// fixed check names, never excerpt, source or summary text, so it is safe
/// to journal, log and submit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{site}: {check}")]
pub struct ProposalRejection {
    site: RejectionSite,
    // `Cow` (clone-on-write) holds either a `&'static str` check name or an
    // owned `String` for checks that name a source id, without always allocating.
    check: Cow<'static, str>,
}

impl ProposalRejection {
    fn new(site: RejectionSite, check: impl Into<Cow<'static, str>>) -> Self {
        Self {
            site,
            check: check.into(),
        }
    }

    /// Where the proposal failed.
    pub fn site(&self) -> RejectionSite {
        self.site
    }

    /// The failed check, e.g. `excerpt is not a substring of s1`.
    pub fn check(&self) -> &str {
        &self.check
    }
}

impl From<ProposalRejection> for RunnerError {
    fn from(_: ProposalRejection) -> Self {
        Self::InvalidProposal
    }
}

/// A source id is echoed only when it has the packet's own `s<digits>` form;
/// anything else is model text and stays unquoted.
fn is_source_label(id: &str) -> bool {
    id.strip_prefix('s').is_some_and(|digits| {
        (1..=3).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Fixed check text for an S1 validation error; secret text is never echoed.
fn report_check(error: OkfError) -> Cow<'static, str> {
    match error {
        // Every `Refused` reason in `maintenance::validate` is a fixed literal.
        OkfError::Refused { reason } => Cow::Owned(reason),
        OkfError::SecretDetected { .. } => Cow::Borrowed("secret detected"),
        _ => Cow::Borrowed("report validation failed"),
    }
}

/// Attributes an S1 failure to the first finding that fails on its own, so a
/// rejection names the finding rather than the whole report.
fn locate(submission: &Submission, error: OkfError) -> ProposalRejection {
    submission
        .findings
        .iter()
        .enumerate()
        .find_map(|(index, finding)| {
            let alone = Submission {
                outcome: WorkOutcome::Failed,
                assessment: Assessment::NotAssessed,
                summary: String::new(),
                evidence: vec![],
                findings: vec![finding.clone()],
                reported_tokens: None,
            };
            validate::submission(&alone)
                .err()
                .map(|e| ProposalRejection::new(RejectionSite::Finding(index), report_check(e)))
        })
        .unwrap_or_else(|| ProposalRejection::new(RejectionSite::Proposal, report_check(error)))
}

/// Yields `(byte offset, char)` for `text` with every run of ASCII whitespace
/// folded to one `' '` at the run's first byte. Nothing else changes: no case
/// folding and no Unicode normalization.
fn fold_whitespace(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    // `move` hands `previous_was_space` to the closure, which mutates it as
    // state between calls (like a generator's local in Python).
    let mut previous_was_space = false;
    text.char_indices().filter_map(move |(offset, c)| {
        let space = c.is_ascii_whitespace();
        let keep = !(space && previous_was_space);
        previous_was_space = space;
        keep.then_some((offset, if space { ' ' } else { c }))
    })
}

/// A source with its whitespace folded, plus a map back to the original.
#[derive(Clone)]
struct FoldedSource {
    /// The source after [`fold_whitespace`].
    text: String,
    /// For each byte of `text`, the original offset of the char it belongs to,
    /// then a final entry holding the original length.
    starts: Vec<usize>,
}

impl FoldedSource {
    /// Folds `text` once, in time and memory linear in its length.
    fn new(text: &str) -> Self {
        let mut folded = String::with_capacity(text.len());
        let mut starts = Vec::with_capacity(text.len() + 1);
        for (offset, c) in fold_whitespace(text) {
            folded.push(c);
            starts.extend(std::iter::repeat_n(offset, c.len_utf8()));
        }
        starts.push(text.len());
        Self {
            text: folded,
            starts,
        }
    }
}

/// Finds `excerpt` in `text`, treating each run of ASCII whitespace in the
/// excerpt as matching any nonempty run in the source, and returns the exact
/// source span. A verbatim quote is preferred; otherwise the first tolerant
/// match wins. `folded` supplies the folded source only when it is needed.
///
/// Both searches are `str::find` (two-way, linear time); no pattern is ever
/// compiled from model text.
///
/// # Arguments
/// * `text` - the source text the excerpt must come from.
/// * `excerpt` - the model's excerpt; must be nonempty.
/// * `folded` - returns `text` folded by [`FoldedSource::new`].
///
/// # Returns
/// `Some(span)` where `span` is a slice of `text`, or `None` if no match.
fn source_span<'a>(
    text: &'a str,
    excerpt: &str,
    folded: impl FnOnce() -> &'a FoldedSource,
) -> Option<&'a str> {
    if let Some(start) = text.find(excerpt) {
        return text.get(start..start + excerpt.len());
    }
    let folded = folded();
    let needle: String = fold_whitespace(excerpt).map(|(_, c)| c).collect();
    let at = folded.text.find(&needle)?;
    // `starts[at + needle.len()]` is where the next folded char begins in the
    // source, so a trailing whitespace run is kept whole.
    text.get(*folded.starts.get(at)?..*folded.starts.get(at + needle.len())?)
}

/// Converts proposals without ever asserting current-world verification.
/// Aging wins over Changed, including combined lanes. All S1 shape, byte,
/// comparison and primary-context checks run on the fully mapped submission.
///
/// # Errors
/// [`ProposalRejection`] naming the first failed check and where it failed.
/// It converts into [`RunnerError::InvalidProposal`] with `?`.
pub fn to_submission(
    packet: &Packet,
    proposal: &Proposal,
) -> Result<Submission, ProposalRejection> {
    use RejectionSite::{Evidence as AtEvidence, Finding, Packet as AtPacket, Summary};
    let packet = bounded_packet(packet)
        .map_err(|_| ProposalRejection::new(AtPacket, "evidence packet is invalid"))?;
    let primary = &packet.sources[0];
    if proposal.findings.len() > 20 {
        return Err(ProposalRejection::new(
            RejectionSite::Proposal,
            "more than 20 findings",
        ));
    }
    clean(&proposal.summary).map_err(|_| ProposalRejection::new(Summary, "secret detected"))?;
    let mut findings = Vec::with_capacity(proposal.findings.len());
    let mut folded: Vec<Option<FoldedSource>> = vec![None; packet.sources.len()];
    for (f, finding) in proposal.findings.iter().enumerate() {
        let reject = |check: &'static str| ProposalRejection::new(Finding(f), check);
        if finding.relationship == Relationship::ExternalObservation {
            return Err(reject("external_observation is not allowed"));
        }
        if finding.evidence.len() > 20 {
            return Err(reject("more than 20 evidence entries"));
        }
        for (text, check) in [
            (&finding.assertion, "secret detected in assertion"),
            (&finding.follow_up, "secret detected in follow_up"),
            (&finding.uncertainty, "secret detected in uncertainty"),
        ] {
            clean(text).map_err(|_| reject(check))?;
        }
        let mut evidence = Vec::with_capacity(finding.evidence.len());
        for (e, item) in finding.evidence.iter().enumerate() {
            let reject = |check: Cow<'static, str>| {
                ProposalRejection::new(
                    AtEvidence {
                        finding: f,
                        evidence: e,
                    },
                    check,
                )
            };
            clean(&item.source_id).map_err(|_| reject("secret detected in source_id".into()))?;
            clean(&item.excerpt).map_err(|_| reject("secret detected in excerpt".into()))?;
            let index = packet
                .sources
                .iter()
                .position(|source| source.id == item.source_id)
                .ok_or_else(|| {
                    reject(if is_source_label(&item.source_id) {
                        format!("unknown source_id {}", item.source_id).into()
                    } else {
                        "unknown source_id".into()
                    })
                })?;
            let source = &packet.sources[index];
            if item.excerpt.is_empty() {
                return Err(reject("excerpt is empty".into()));
            }
            // `get_or_insert_with` folds each source at most once per proposal,
            // and only when some excerpt is not a verbatim quote of it.
            let span = source_span(&source.text, &item.excerpt, || {
                folded[index].get_or_insert_with(|| FoldedSource::new(&source.text))
            })
            .ok_or_else(|| reject(format!("excerpt is not a substring of {}", source.id).into()))?;
            if span.len() > 2048 {
                return Err(reject("excerpt exceeds 2048 bytes".into()));
            }
            evidence.push(Evidence {
                pin: source.pin.clone(),
                excerpt: span.to_owned(),
            });
        }
        findings.push(FindingInput {
            category: finding.category,
            assertion: finding.assertion.clone(),
            follow_up: finding.follow_up.clone(),
            uncertainty: finding.uncertainty.clone(),
            relationship: finding.relationship,
            evidence,
            external: vec![],
        });
    }
    // A UTF-8 boundary, not a line-ending rewrite or a source truncation.
    let mut end = primary.text.len().min(2048);
    while !primary.text.is_char_boundary(end) {
        end -= 1;
    }
    let evidence = if end == 0 {
        vec![]
    } else {
        vec![Evidence {
            pin: primary.pin.clone(),
            excerpt: primary.text[..end].to_owned(),
        }]
    };
    let aging = packet.reasons.contains(&Lane::Aging);
    let submission = Submission {
        outcome: if aging || evidence.is_empty() {
            WorkOutcome::Inconclusive
        } else {
            WorkOutcome::Completed
        },
        assessment: if aging {
            Assessment::Unverifiable
        } else {
            Assessment::NotAssessed
        },
        summary: proposal.summary.clone(),
        evidence,
        findings,
        reported_tokens: None,
    };
    validate::submission_for_work(&submission, &primary.pin, &packet.reasons)
        .map_err(|error| locate(&submission, error))?;
    Ok(submission)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs [`source_span`] with a freshly folded source.
    fn span<'a>(text: &'a str, excerpt: &str, folded: &'a FoldedSource) -> Option<&'a str> {
        source_span(text, excerpt, || folded)
    }

    #[test]
    fn fold_whitespace_collapses_runs_and_keeps_offsets() {
        let folded: Vec<(usize, char)> = fold_whitespace("a \t\r\nb\u{e9} c").collect();
        assert_eq!(
            folded,
            vec![
                (0, 'a'),
                (1, ' '),
                (5, 'b'),
                (6, '\u{e9}'),
                (8, ' '),
                (9, 'c')
            ]
        );
    }

    #[test]
    fn folded_source_maps_every_byte_and_the_end() {
        let source = FoldedSource::new("\u{e9}  x");
        assert_eq!(source.text, "\u{e9} x");
        assert_eq!(source.starts, vec![0, 0, 2, 4, 5]);
    }

    #[test]
    fn source_span_returns_exact_slices() {
        let text = "keep\n  this\ttext";
        let folded = FoldedSource::new(text);
        assert_eq!(span(text, "keep this text", &folded), Some(text));
        assert_eq!(span(text, "this", &folded), Some("this"));
        assert_eq!(span(text, " this ", &folded), Some("\n  this\t"));
        assert_eq!(span(text, "keepthis", &folded), None);
        assert_eq!(span(text, "Keep this", &folded), None);
    }

    #[test]
    fn source_span_skips_folding_for_verbatim_quotes() {
        let text = "verbatim";
        let found = source_span(text, "batim", || unreachable!("no fold needed"));
        assert_eq!(found, Some("batim"));
    }
}
