use super::{corpus, evaluate_value};
use okf_mcp::investigator::{error::RunnerError, evaluate};
use serde_json::{json, Value};

fn one_case() -> Value {
    let mut value = corpus();
    value["cases"].as_array_mut().unwrap().truncate(1);
    value
}
fn run_raw(raw: &[u8]) -> Result<evaluate::EvaluationSummary, RunnerError> {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), raw).unwrap();
    evaluate::run(file.path())
}
fn with_raw_at(mut corpus: Value, path: &str, raw: &str) -> String {
    *corpus.pointer_mut(path).unwrap() = json!("RAW_INSERTION_SENTINEL");
    serde_json::to_string(&corpus)
        .unwrap()
        .replace("\"RAW_INSERTION_SENTINEL\"", raw)
}
fn positional(value: &Value, fields: &[&str]) -> String {
    serde_json::to_string(
        &fields
            .iter()
            .map(|field| &value[*field])
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn duplicate(value: &Value, field: &str) -> String {
    let original = serde_json::to_string(value).unwrap();
    format!(
        "{{{}:{},{}",
        serde_json::to_string(field).unwrap(),
        value[field],
        &original[1..]
    )
}
fn assert_config(raw: &[u8]) {
    let error = run_raw(raw).expect_err("invalid corpus must be rejected");
    assert_eq!(error, RunnerError::Config);
    assert_eq!(error.to_string(), "runner configuration is invalid");
    assert_eq!(format!("{error:?}"), "Config");
}

#[test]
fn investigator_evaluation_rejects_proposal_arrays_and_duplicates() {
    let cases: &[(&str, &[&str])] = &[
        ("/cases/0/proposal", &["summary", "findings"]),
        (
            "/cases/0/proposal/findings/0",
            &[
                "category",
                "assertion",
                "follow_up",
                "uncertainty",
                "relationship",
                "evidence",
            ],
        ),
        (
            "/cases/0/proposal/findings/0/evidence/0",
            &["source_id", "excerpt"],
        ),
    ];
    let mut accepted = Vec::new();
    for (path, fields) in cases {
        let data = one_case();
        for (kind, raw) in [
            ("array", positional(data.pointer(path).unwrap(), fields)),
            (
                "duplicate",
                duplicate(data.pointer(path).unwrap(), fields[0]),
            ),
        ] {
            let result = run_raw(with_raw_at(data.clone(), path, &raw).as_bytes()).unwrap();
            if (
                result.true_positive,
                result.false_positive,
                result.false_negative,
            ) != (0, 0, 1)
            {
                accepted.push((path, kind));
            }
            assert_eq!(result.false_verification, 0);
        }
    }
    assert!(
        accepted.is_empty(),
        "malformed predictions were scored: {accepted:?}"
    );
}

#[test]
fn investigator_evaluation_counts_injection_hidden_by_duplicate_fields() {
    let data = one_case();
    let proposal = &data["cases"][0]["proposal"];
    let mut injected = proposal["findings"].clone();
    injected[0]["assessment"] = json!("supported_current");
    let original = serde_json::to_string(proposal).unwrap();
    let raw = format!("{{\"findings\":{injected},{}", &original[1..]);
    let result = run_raw(with_raw_at(data, "/cases/0/proposal", &raw).as_bytes()).unwrap();
    assert_eq!(
        (
            result.true_positive,
            result.false_positive,
            result.false_negative
        ),
        (0, 0, 1)
    );
    assert_eq!(result.rejected_verification, 1);
    assert_eq!(result.false_verification, 0);
}

#[test]
fn investigator_evaluation_requires_objects_and_unique_fields_at_every_dto() {
    let cases: &[(&str, &[&str])] = &[
        ("", &["schema_version", "cases"]),
        (
            "/cases/0",
            &[
                "id",
                "provenance",
                "annotation_author",
                "review",
                "packet",
                "proposal",
                "expected",
                "admissibility",
            ],
        ),
        (
            "/cases/0/packet",
            &["work_id", "reasons", "primary_id", "sources"],
        ),
        ("/cases/0/packet/sources/0", &["id", "pin", "text"]),
        ("/cases/0/packet/sources/0/pin", &["path", "revision"]),
        ("/cases/0/expected/0", &["category", "paths"]),
        ("/cases/0/admissibility/0", &["key", "admissible"]),
        ("/cases/0/admissibility/0/key", &["category", "paths"]),
    ];
    let mut accepted = Vec::new();
    for (path, fields) in cases {
        let data = one_case();
        for (kind, raw) in [
            ("array", positional(data.pointer(path).unwrap(), fields)),
            (
                "duplicate",
                duplicate(data.pointer(path).unwrap(), fields[0]),
            ),
        ] {
            let raw = with_raw_at(data.clone(), path, &raw);
            if run_raw(raw.as_bytes()).is_ok() {
                accepted.push((path, kind));
            } else {
                assert_config(raw.as_bytes());
            }
        }
    }
    // Case metadata is flattened, not a separate DTO. Each field remains a scalar.
    for field in ["id", "provenance", "annotation_author", "review"] {
        let path = format!("/cases/0/{field}");
        assert_config(with_raw_at(one_case(), &path, "[]").as_bytes());
        assert_config(with_raw_at(one_case(), &path, "{}").as_bytes());
        let data = one_case();
        assert_config(
            with_raw_at(
                data.clone(),
                "/cases/0",
                &duplicate(&data["cases"][0], field),
            )
            .as_bytes(),
        );
    }
    assert!(
        accepted.is_empty(),
        "malformed corpus DTOs were accepted: {accepted:?}"
    );
}

#[test]
fn investigator_evaluation_enum_metadata_requires_documented_strings() {
    let mut accepted = Vec::new();
    for pointer in [
        "/cases/0/provenance",
        "/cases/0/review",
        "/cases/0/packet/reasons/0",
        "/cases/0/expected/0/category",
        "/cases/0/admissibility/0/key/category",
        "/cases/0/proposal/findings/0/category",
        "/cases/0/proposal/findings/0/relationship",
    ] {
        let mut data = one_case();
        let variant = data.pointer(pointer).unwrap().as_str().unwrap().to_owned();
        *data.pointer_mut(pointer).unwrap() = json!({variant: null});
        let raw = serde_json::to_vec(&data).unwrap();
        if pointer.contains("/proposal/") {
            let result = run_raw(&raw).unwrap();
            if (
                result.true_positive,
                result.false_positive,
                result.false_negative,
            ) != (0, 0, 1)
            {
                accepted.push(pointer);
            }
        } else if run_raw(&raw).is_ok() {
            accepted.push(pointer);
        } else {
            assert_config(&raw);
        }
    }
    assert!(
        accepted.is_empty(),
        "non-string enum metadata accepted: {accepted:?}"
    );
}

#[test]
fn investigator_evaluation_exact_file_and_case_limits() {
    let base = br#"{"schema_version":1,"cases":[]}"#;
    for (length, valid) in [
        (16 * 1024 * 1024 - 1, true),
        (16 * 1024 * 1024, true),
        (16 * 1024 * 1024 + 1, false),
    ] {
        let mut bytes = base.to_vec();
        bytes.resize(length, b' ');
        if valid {
            assert_eq!(run_raw(&bytes).unwrap().cases, 0);
        } else {
            assert_config(&bytes);
        }
    }
    let mut case = one_case()["cases"][0].clone();
    case["proposal"] = json!({"summary":"Fixture only.","findings":[]});
    case["expected"] = json!([]);
    case["admissibility"] = json!([]);
    case["packet"]["sources"] =
        json!([{"id":"s0","pin":{"path":"/a.md","revision":1},"text":"Fixture."}]);
    for (count, valid) in [(9_999, true), (10_000, true), (10_001, false)] {
        let cases: Vec<_> = (0..count)
            .map(|id| {
                let mut c = case.clone();
                c["id"] = json!(format!("case-{id}"));
                c
            })
            .collect();
        let raw = serde_json::to_vec(&json!({"schema_version":1,"cases":cases})).unwrap();
        assert!(raw.len() < 16 * 1024 * 1024);
        if valid {
            assert_eq!(run_raw(&raw).unwrap().cases, count);
        } else {
            assert_config(&raw);
        }
    }
}

#[test]
fn investigator_evaluation_paths_annotations_and_identity_boundaries() {
    for path in ["", "../escape.md", "/a/../b.md", "/a\u{0}.md", "/x.txt"] {
        for pointer in [
            "/cases/0/expected/0/paths/0",
            "/cases/0/admissibility/0/key/paths/0",
            "/cases/0/packet/sources/0/pin/path",
        ] {
            let mut data = one_case();
            *data.pointer_mut(pointer).unwrap() = json!(path);
            assert_config(&serde_json::to_vec(&data).unwrap());
        }
    }
    for (length, valid) in [(255, true), (256, true), (257, false)] {
        let path = format!("/{}.md", "a".repeat(length - 4));
        let mut data = one_case();
        data["cases"][0]["expected"][0]["paths"] = json!([path]);
        let raw = serde_json::to_vec(&data).unwrap();
        if valid {
            assert_eq!(run_raw(&raw).unwrap().false_negative, 1);
        } else {
            assert_config(&raw);
        }
    }
    for mode in [
        "duplicate-id",
        "conflicting-annotation",
        "empty-author",
        "empty-id",
        "unknown-field",
    ] {
        let mut data = one_case();
        match mode {
            "duplicate-id" => {
                let c = data["cases"][0].clone();
                data["cases"].as_array_mut().unwrap().push(c);
            }
            "conflicting-annotation" => {
                let mut a = data["cases"][0]["admissibility"][0].clone();
                a["admissible"] = json!(false);
                data["cases"][0]["admissibility"]
                    .as_array_mut()
                    .unwrap()
                    .push(a);
            }
            "empty-author" => data["cases"][0]["annotation_author"] = json!(" "),
            "empty-id" => data["cases"][0]["id"] = json!(""),
            _ => data["cases"][0]["packet"]["source-body-private"] = json!("never echo"),
        }
        assert_config(&serde_json::to_vec(&data).unwrap());
    }
}

#[test]
fn investigator_evaluation_provenance_review_matrix() {
    for provenance in ["synthetic", "reviewed_captured_output"] {
        for review in [
            "synthetic_specification",
            "human_reviewed",
            "ambiguous",
            "unreviewed",
        ] {
            let mut data = one_case();
            data["cases"][0]["provenance"] = json!(provenance);
            data["cases"][0]["review"] = json!(review);
            if provenance == "reviewed_captured_output" && review == "synthetic_specification" {
                assert_config(&serde_json::to_vec(&data).unwrap());
            } else {
                let result = evaluate_value(&data);
                let excluded = matches!(review, "ambiguous" | "unreviewed");
                assert_eq!(result.excluded_unreviewed, u32::from(excluded));
                assert_eq!(result.true_positive, u32::from(!excluded));
                if excluded {
                    assert_eq!((result.precision, result.recall), (None, None));
                }
            }
        }
    }
}
