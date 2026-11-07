use okf_mcp::investigator::evaluate;
use serde_json::{json, Value};
use std::path::Path;

#[path = "investigator_evaluation_boundaries.rs"]
mod boundaries;

fn corpus() -> Value {
    serde_json::from_str(include_str!(
        "../fixtures/investigator/corpus/evaluation.json"
    ))
    .unwrap()
}
fn evaluate_value(value: &Value) -> evaluate::EvaluationSummary {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec(value).unwrap()).unwrap();
    evaluate::run(file.path()).unwrap()
}
#[test]
fn investigator_evaluation_reports_false_verification() {
    let result = evaluate::run(Path::new(
        "tests/fixtures/investigator/corpus/evaluation.json",
    ))
    .unwrap();
    assert_eq!(result.cases, 11);
    // Four exact keys, two wrong/inadmissible keys, three unmatched expectations.
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (4, 2, 3)
    );
    assert_eq!(result.rejected_citations, 1);
    assert_eq!(result.false_verification, 0);
    assert_eq!(result.rejected_verification, 2);
    assert_eq!(result.excluded_unreviewed, 2);
    assert_eq!(result.precision, Some(4.0 / 6.0));
    assert_eq!(result.recall, Some(4.0 / 7.0));
}
#[test]
fn investigator_evaluation_does_not_call_a_model() {
    // Exercise actual proposals, including raw injections, without passing any
    // client, process adapter, credentials or inference capability into this API.
    let scored = evaluate_value(&corpus());
    assert_eq!(scored.cases, 11);
    assert_eq!(scored.true_positive, 4);
    assert_eq!(scored.rejected_verification, 2);
    let result = evaluate_value(&json!({"schema_version":1,"cases":[]}));
    assert_eq!(result.cases, 0);
    assert_eq!(result.precision, None);
    assert_eq!(result.recall, None);
    let encoded = serde_json::to_value(result).unwrap();
    assert_eq!(encoded["precision"], Value::Null);
    assert_eq!(encoded["recall"], Value::Null);
    assert_eq!(encoded.as_object().unwrap().len(), 10);
}
#[test]
fn investigator_evaluation_deduplicates_and_excludes_both_denominators() {
    let mut data = corpus();
    data["cases"].as_array_mut().unwrap().truncate(1);
    let first = data["cases"][0]["proposal"]["findings"][0].clone();
    data["cases"][0]["proposal"]["findings"]
        .as_array_mut()
        .unwrap()
        .push(first);
    let expected = data["cases"][0]["expected"][0].clone();
    data["cases"][0]["expected"]
        .as_array_mut()
        .unwrap()
        .push(expected);
    let result = evaluate_value(&data);
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (1, 0, 0)
    );
    for review in ["ambiguous", "unreviewed"] {
        data["cases"][0]["review"] = json!(review);
        let result = evaluate_value(&data);
        assert_eq!(
            (
                result.true_positive,
                result.false_positive,
                result.false_negative
            ),
            (0, 0, 0)
        );
        assert_eq!(result.excluded_unreviewed, 1);
        assert_eq!((result.precision, result.recall), (None, None));
    }
}
#[test]
fn investigator_evaluation_one_sided_denominators_and_missing_annotations() {
    let mut data = corpus();
    data["cases"].as_array_mut().unwrap().truncate(1);
    data["cases"][0]["proposal"]["findings"] = json!([]);
    let result = evaluate_value(&data);
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (0, 0, 1)
    );
    assert_eq!((result.precision, result.recall), (None, Some(0.0)));
    let mut data = corpus();
    data["cases"].as_array_mut().unwrap().truncate(1);
    data["cases"][0]["expected"] = json!([]);
    let result = evaluate_value(&data);
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (0, 1, 0)
    );
    assert_eq!((result.precision, result.recall), (Some(0.0), None));
    data["cases"][0]["admissibility"] = json!([]);
    let result = evaluate_value(&data);
    assert_eq!(result.excluded_unreviewed, 1);
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (0, 0, 0)
    );
}
#[test]
fn investigator_evaluation_unknown_source_is_a_rejected_citation() {
    let mut data = corpus();
    data["cases"].as_array_mut().unwrap().truncate(1);
    data["cases"][0]["proposal"]["findings"][0]["evidence"][0]["source_id"] =
        json!("guessed-source");
    let result = evaluate_value(&data);
    assert_eq!(result.rejected_citations, 1);
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (0, 0, 1)
    );
}
#[test]
fn investigator_evaluation_invalid_inputs_are_redacted() {
    let file = tempfile::NamedTempFile::new().unwrap();
    for raw in [
        "source-body-private",
        r#"{"schema_version":2,"cases":[]}"#,
        r#"{"schema_version":1,"cases":[],"secret":"source-body-private"}"#,
    ] {
        std::fs::write(file.path(), raw).unwrap();
        let error = evaluate::run(file.path()).err().unwrap();
        assert!(!format!("{error:?} {error}").contains("source-body-private"));
    }
}
