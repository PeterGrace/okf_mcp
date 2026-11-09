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
            let source = packet
                .sources
                .iter()
                .find(|source| source.id == item.source_id)
                .ok_or_else(|| {
                    reject(if is_source_label(&item.source_id) {
                        format!("unknown source_id {}", item.source_id).into()
                    } else {
                        "unknown source_id".into()
                    })
                })?;
            if item.excerpt.is_empty() {
                return Err(reject("excerpt is empty".into()));
            }
            if item.excerpt.len() > 2048 {
                return Err(reject("excerpt exceeds 2048 bytes".into()));
            }
            if !source.text.contains(&item.excerpt) {
                return Err(reject(
                    format!("excerpt is not a substring of {}", source.id).into(),
                ));
            }
            evidence.push(Evidence {
                pin: source.pin.clone(),
                excerpt: item.excerpt.clone(),
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
