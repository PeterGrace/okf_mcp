//! Offline structural fixture evaluation, not factual verification or live-model quality.
//!
//! Input is a version-1 JSON corpus with explicit review and per-relation
//! admissibility. Synthetic labels are test specifications, not human validation
//! of model output. Captured output requires a named human annotator. Ambiguous,
//! unreviewed or incompletely annotated cases contribute to neither denominator.
//! Keys are (category, sorted distinct evidence paths), scoped to each case.
//! Only whole proposals accepted by the production mapper with exact citations
//! are scored: admissible exact keys are TP, other accepted keys FP, unmatched
//! expected keys FN. Structurally rejected proposals contribute no predictions
//! and leave expectations unmatched. Citation rejection counts bad evidence
//! entries; verification rejection counts raw proposals with forbidden outcome
//! or assessment fields. These diagnostics also include excluded cases.
//! No network, subprocess, inference, credentials or model-based grading is used.
//! Independent human corpus review remains an operator gate before automation;
//! neither synthetic examples nor a declared review status establish that review.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};

use super::{
    collect::{bounded_packet, normalized_path},
    error::RunnerError,
    proposal::{decode_proposal, to_submission},
    types::Packet,
};
use crate::maintenance::types::{Assessment, Category};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSummary {
    pub cases: u32,
    pub true_positive: u32,
    pub false_positive: u32,
    pub false_negative: u32,
    pub rejected_citations: u32,
    pub false_verification: u32,
    pub rejected_verification: u32,
    pub excluded_unreviewed: u32,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: u32,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    provenance: Provenance,
    annotation_author: String,
    review: Review,
    packet: Packet,
    // Retain original field occurrences until the shared strict proposal decoder.
    proposal: Box<RawValue>,
    expected: Vec<Relation>,
    admissibility: Vec<Annotation>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Provenance {
    Synthetic,
    ReviewedCapturedOutput,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Review {
    SyntheticSpecification,
    HumanReviewed,
    Ambiguous,
    Unreviewed,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Relation {
    category: Category,
    paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Annotation {
    key: Relation,
    admissible: bool,
}
type Key = (String, Vec<String>);

fn key(relation: &Relation) -> Result<Key, RunnerError> {
    if relation.paths.is_empty() || relation.paths.len() > 20 {
        return Err(RunnerError::Config);
    }
    let paths = relation
        .paths
        .iter()
        .map(|p| normalized_path(p, 1))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| RunnerError::Config)?;
    let category = serde_json::to_value(relation.category).map_err(|_| RunnerError::Config)?;
    Ok((
        category.as_str().ok_or(RunnerError::Config)?.to_owned(),
        paths.into_iter().collect(),
    ))
}
// Visit original field occurrences, including ones a duplicate would overwrite.
// This is diagnostic only; acceptance always uses decode_proposal.
fn verification_attempt(raw: &RawValue) -> Result<bool, RunnerError> {
    struct Fields;
    impl<'de> serde::de::Visitor<'de> for Fields {
        type Value = bool;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<bool, M::Error> {
            let mut found = false;
            while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                found |= matches!(key.as_str(), "assessment" | "outcome")
                    || verification_attempt(&value)
                        .map_err(|_| serde::de::Error::custom("invalid proposal"))?;
            }
            Ok(found)
        }
    }
    match raw.get().trim_start().as_bytes().first() {
        Some(b'{') => serde::Deserializer::deserialize_map(
            &mut serde_json::Deserializer::from_str(raw.get()),
            Fields,
        )
        .map_err(|_| RunnerError::Config),
        Some(b'[') => {
            let values: Vec<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(|_| RunnerError::Config)?;
            values.iter().try_fold(false, |found, value| {
                Ok(found || verification_attempt(value)?)
            })
        }
        _ => Ok(false),
    }
}

// DTO shape inspection is separate from decoding the original bytes: the latter
// rejects duplicate/unknown fields. Enum unit variants also need string checks:
// serde otherwise accepts externally tagged objects outside our JSON contract.
fn corpus_objects(value: &Value) -> Result<(), RunnerError> {
    fn object(value: &Value) -> Result<(), RunnerError> {
        value.as_object().map(|_| ()).ok_or(RunnerError::Config)
    }
    fn array(value: &Value) -> Result<&[Value], RunnerError> {
        value
            .as_array()
            .map(Vec::as_slice)
            .ok_or(RunnerError::Config)
    }
    object(value)?;
    for case in array(&value["cases"])? {
        object(case)?;
        for field in ["id", "provenance", "annotation_author", "review"] {
            if !case[field].is_string() {
                return Err(RunnerError::Config);
            }
        }
        let packet = &case["packet"];
        object(packet)?;
        if !array(&packet["reasons"])?.iter().all(Value::is_string) {
            return Err(RunnerError::Config);
        }
        for source in array(&packet["sources"])? {
            object(source)?;
            object(&source["pin"])?;
        }
        for relation in array(&case["expected"])? {
            object(relation)?;
            if !relation["category"].is_string() {
                return Err(RunnerError::Config);
            }
        }
        for annotation in array(&case["admissibility"])? {
            object(annotation)?;
            object(&annotation["key"])?;
            if !annotation["key"]["category"].is_string() {
                return Err(RunnerError::Config);
            }
        }
    }
    Ok(())
}

/// Evaluate a bounded JSON corpus. Undefined precision/recall serialize as null.
/// Malformed corpus/configuration errors contain no input text or source bodies.
pub fn run(cases: &Path) -> Result<EvaluationSummary, RunnerError> {
    const CAP: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(cases)
        .map_err(|_| RunnerError::Io)?
        .take(CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RunnerError::Io)?;
    if bytes.len() as u64 > CAP {
        return Err(RunnerError::Config);
    }
    corpus_objects(&serde_json::from_slice(&bytes).map_err(|_| RunnerError::Config)?)?;
    let corpus: Corpus = serde_json::from_slice(&bytes).map_err(|_| RunnerError::Config)?;
    if corpus.schema_version != 1 || corpus.cases.len() > 10_000 {
        return Err(RunnerError::Config);
    }
    let mut result = EvaluationSummary::default();
    let mut ids = BTreeSet::new();
    for case in corpus.cases {
        if case.id.trim().is_empty()
            || !ids.insert(case.id)
            || case.annotation_author.trim().is_empty()
        {
            return Err(RunnerError::Config);
        }
        if case.review == Review::SyntheticSpecification
            && !matches!(case.provenance, Provenance::Synthetic)
        {
            return Err(RunnerError::Config);
        }
        let packet = bounded_packet(&case.packet).map_err(|_| RunnerError::Config)?;
        let expected = case
            .expected
            .iter()
            .map(key)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut annotations = BTreeMap::new();
        for annotation in case.admissibility {
            let k = key(&annotation.key)?;
            if let Some(previous) = annotations.insert(k, annotation.admissible) {
                if previous != annotation.admissible {
                    return Err(RunnerError::Config);
                }
            }
        }
        result.cases += 1;
        let blocked = verification_attempt(&case.proposal)?;
        result.rejected_verification += u32::from(blocked);
        let proposal = decode_proposal(case.proposal.get().as_bytes()).ok();
        let mut predictions = BTreeSet::new();
        if let Some(proposal) = &proposal {
            for finding in &proposal.findings {
                for evidence in &finding.evidence {
                    let valid = packet
                        .sources
                        .iter()
                        .find(|s| s.id == evidence.source_id)
                        .is_some_and(|s| {
                            !evidence.excerpt.is_empty()
                                && evidence.excerpt.len() <= 2048
                                && s.text.contains(&evidence.excerpt)
                        });
                    result.rejected_citations += u32::from(!valid);
                }
            }
            if !blocked {
                if let Ok(submission) = to_submission(&packet, proposal) {
                    result.false_verification +=
                        u32::from(submission.assessment == Assessment::SupportedCurrent);
                    for finding in &proposal.findings {
                        let paths = finding
                            .evidence
                            .iter()
                            .filter_map(|e| {
                                packet
                                    .sources
                                    .iter()
                                    .find(|s| s.id == e.source_id)
                                    .map(|s| s.pin.path.clone())
                            })
                            .collect();
                        predictions.insert(key(&Relation {
                            category: finding.category,
                            paths,
                        })?);
                    }
                }
            }
        }
        if !matches!(
            case.review,
            Review::SyntheticSpecification | Review::HumanReviewed
        ) || predictions.iter().any(|k| !annotations.contains_key(k))
        {
            result.excluded_unreviewed += 1;
            continue;
        }
        let matched: BTreeSet<_> = predictions
            .iter()
            .filter(|k| annotations.get(*k) == Some(&true) && expected.contains(*k))
            .cloned()
            .collect();
        result.true_positive += matched.len() as u32;
        result.false_positive += (predictions.len() - matched.len()) as u32;
        result.false_negative += expected.difference(&matched).count() as u32;
    }
    let denominator = result.true_positive + result.false_positive;
    result.precision =
        (denominator != 0).then(|| f64::from(result.true_positive) / f64::from(denominator));
    let denominator = result.true_positive + result.false_negative;
    result.recall =
        (denominator != 0).then(|| f64::from(result.true_positive) / f64::from(denominator));
    Ok(result)
}
