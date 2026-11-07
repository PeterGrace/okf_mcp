//! Catalog-only proposals: IDs map to supplied immutable evidence, never tools.

pub use super::types::{Proposal, ProposedEvidence, ProposedFinding};

use super::{
    collect::{bounded_packet, clean},
    error::RunnerError,
    types::Packet,
};
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

/// Converts proposals without ever asserting current-world verification.
/// Aging wins over Changed, including combined lanes. All S1 shape, byte,
/// comparison and primary-context checks run on the fully mapped submission.
pub fn to_submission(packet: &Packet, proposal: &Proposal) -> Result<Submission, RunnerError> {
    let invalid = |_| RunnerError::InvalidProposal;
    let packet = bounded_packet(packet).map_err(invalid)?;
    let primary = &packet.sources[0];
    if proposal.findings.len() > 20 {
        return Err(RunnerError::InvalidProposal);
    }
    clean(&proposal.summary).map_err(invalid)?;
    let mut findings = Vec::new();
    for finding in &proposal.findings {
        if finding.relationship == Relationship::ExternalObservation || finding.evidence.len() > 20
        {
            return Err(RunnerError::InvalidProposal);
        }
        for text in [&finding.assertion, &finding.follow_up, &finding.uncertainty] {
            clean(text).map_err(invalid)?;
        }
        let mut evidence = Vec::new();
        for item in &finding.evidence {
            clean(&item.source_id).map_err(invalid)?;
            clean(&item.excerpt).map_err(invalid)?;
            let source = packet
                .sources
                .iter()
                .find(|source| source.id == item.source_id)
                .ok_or(RunnerError::InvalidProposal)?;
            if item.excerpt.is_empty()
                || item.excerpt.len() > 2048
                || !source.text.contains(&item.excerpt)
            {
                return Err(RunnerError::InvalidProposal);
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
        .map_err(|_| RunnerError::InvalidProposal)?;
    Ok(submission)
}
