use super::tests::{actor, revision, setup, t0};
use super::{reports, runs};
use crate::db::test_db;
use crate::error::OkfError;
use crate::maintenance::types::*;
use chrono::Duration;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn fixture(pool: &PgPool, catalog: i64, key: &str) -> (Lease, String) {
    let lease = runs::claim(
        pool,
        catalog,
        &actor("reporter"),
        key,
        1,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap()
    .leases
    .remove(0);
    let primary = Evidence {
        pin: lease.pin.clone(),
        excerpt: "Body".into(),
    };
    let input = Submission {
        outcome: WorkOutcome::Completed,
        assessment: Assessment::SupportedCurrent,
        summary: "Investigation".into(),
        evidence: vec![primary.clone()],
        findings: vec![FindingInput {
            category: Category::UnverifiableClaim,
            assertion: "Check".into(),
            follow_up: "Ask".into(),
            uncertainty: "Unknown".into(),
            relationship: Relationship::DirectStatement,
            evidence: vec![Evidence {
                pin: RevisionPin {
                    path: "b.md".into(),
                    revision: 1,
                },
                excerpt: "Body".into(),
            }],
            external: vec![],
        }],
        reported_tokens: None,
    };
    let receipt = reports::submit(
        pool,
        catalog,
        &actor("reporter"),
        &lease.work_id,
        &lease.token,
        key,
        &input,
        t0(),
    )
    .await
    .unwrap();
    (lease, receipt.finding_ids[0].clone())
}

async fn maintenance_snapshot(pool: &PgPool, catalog: i64) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(d) ORDER BY id) FROM maintenance_dispositions d WHERE catalog_id=$1),(SELECT jsonb_agg(to_jsonb(q) ORDER BY credential_id,operation,idempotency_key) FROM maintenance_requests q WHERE catalog_id=$1),(SELECT to_jsonb(s) FROM maintenance_state s WHERE catalog_id=$1),(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM maintenance_runs r WHERE catalog_id=$1),(SELECT jsonb_agg(to_jsonb(w) ORDER BY id) FROM maintenance_work w WHERE catalog_id=$1))")
        .bind(catalog).fetch_one(pool).await.unwrap()
}

async fn canonical(pool: &PgPool, catalog: i64) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(d) ORDER BY path) FROM documents d WHERE catalog_id=$1),(SELECT jsonb_agg(to_jsonb(r) ORDER BY path,revision) FROM revisions r WHERE catalog_id=$1),(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM changesets c WHERE catalog_id=$1),(SELECT to_jsonb(c) FROM catalogs c WHERE id=$1))")
        .bind(catalog).fetch_one(pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_disposition_preserves_history_and_content() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (lease, id) = fixture(&db.pool, catalog, "first").await;
    let before = canonical(&db.pool, catalog).await;
    let initial = reports::get_finding(&db.pool, catalog, &id).await.unwrap();
    assert_eq!(initial.disposition, Disposition::Open);
    assert!(initial.dispositions.is_empty());
    assert_eq!(initial.actor, "agent/reporter");
    let human = actor("human");
    let mut responses = vec![];
    for (n, disposition) in [
        Disposition::AcceptedForFollowUp,
        Disposition::Dismissed,
        Disposition::Open,
    ]
    .into_iter()
    .enumerate()
    {
        let response = reports::dispose(
            &db.pool,
            catalog,
            &human,
            &id,
            disposition,
            "Human rationale",
            &format!("d{n}"),
            t0() + Duration::seconds(n as i64),
        )
        .await
        .unwrap();
        assert_eq!(response.dispositions.len(), n + 1);
        assert_eq!(response.disposition, disposition);
        assert_eq!(response.dispositions[n].actor, human.actor);
        assert_eq!(
            response.dispositions[n].at,
            (t0() + Duration::seconds(n as i64)).to_rfc3339()
        );
        assert_eq!(response.dispositions[n].rationale, "Human rationale");
        responses.push(response);
    }
    let current = reports::get_finding(&db.pool, catalog, &id).await.unwrap();
    assert_eq!(current.dispositions.len(), 3);
    assert_eq!(current.disposition, Disposition::Open);
    assert!(current.dispositions.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(canonical(&db.pool, catalog).await, before);
    revision(&db.pool, catalog, "b.md", 2, t0(), "Body changed").await;
    let unchanged = maintenance_snapshot(&db.pool, catalog).await;
    let renamed = Actor {
        key_id: human.key_id.clone(),
        actor: "renamed human".into(),
    };
    let retry = reports::dispose(
        &db.pool,
        catalog,
        &renamed,
        &id,
        Disposition::AcceptedForFollowUp,
        "Human rationale",
        "d0",
        t0() + Duration::days(10),
    )
    .await
    .unwrap();
    assert_eq!(retry, responses[0]); // Exact original snapshot, not current history/freshness.
    for (disposition, rationale) in [
        (Disposition::Dismissed, "Human rationale"),
        (Disposition::AcceptedForFollowUp, "different"),
    ] {
        assert!(matches!(
            reports::dispose(
                &db.pool,
                catalog,
                &human,
                &id,
                disposition,
                rationale,
                "d0",
                t0()
            )
            .await,
            Err(OkfError::Refused { .. })
        ));
    }
    assert!(reports::dispose(
        &db.pool,
        catalog + 1,
        &human,
        &id,
        Disposition::Open,
        "",
        "d0",
        t0()
    )
    .await
    .is_err());
    let invalid = Actor {
        key_id: "".into(),
        actor: human.actor.clone(),
    };
    assert!(reports::dispose(
        &db.pool,
        catalog,
        &invalid,
        &id,
        Disposition::Open,
        "",
        "d0",
        t0()
    )
    .await
    .is_err());
    assert_eq!(maintenance_snapshot(&db.pool, catalog).await, unchanged);
    // Compare the full canonical snapshot after dispositions, excluding the deliberate source edit.
    revision(&db.pool, catalog, "b.md", 3, t0(), "Body restored").await;
    let after_edit = canonical(&db.pool, catalog).await;
    let other = reports::dispose(
        &db.pool,
        catalog,
        &actor("other-human"),
        &id,
        Disposition::NeedsInput,
        "",
        "d0",
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(other.dispositions.len(), 4); // Credential-scoped idempotency, not another credential's receipt.
    assert_eq!(other.disposition, Disposition::NeedsInput); // Append order wins over the backward clock.
    assert_eq!(
        reports::get_finding(&db.pool, catalog, &id).await.unwrap(),
        other
    );
    assert_eq!(canonical(&db.pool, catalog).await, after_edit);
    assert_eq!(before[2], after_edit[2]);
    assert_eq!(before[3], after_edit[3]);
    let export = json!({"run": runs::get_run(&db.pool, catalog, &lease.run_id).await.unwrap(), "findings": reports::list_findings(&db.pool, catalog, None, 20).await.unwrap()});
    let text = export.to_string();
    assert!(!text.contains(&lease.token));
    assert!(!text.contains("lease_token"));
    assert!(!text.contains("credential_id"));
    assert_eq!(
        serde_json::to_value(&initial)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "id",
            "run_id",
            "work_id",
            "input",
            "actor",
            "created_at",
            "disposition",
            "dispositions",
            "outdated"
        ]
        .map(str::to_owned)
        .into()
    );
    assert_eq!(
        serde_json::to_value(&other.dispositions[0])
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        5
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_disposition_rejects_invalid_text_and_ids_atomically() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (_, id) = fixture(&db.pool, catalog, "first").await;
    let before = canonical(&db.pool, catalog).await;
    for rationale in [
        "é".repeat(2049),
        format!("password: {}", "SuperSecretPassword123!"),
    ] {
        assert!(reports::dispose(
            &db.pool,
            catalog,
            &actor("human"),
            &id,
            Disposition::Open,
            &rationale,
            "invalid",
            t0()
        )
        .await
        .is_err());
    }
    let secret = format!("{}{}", "ghp_", "Ab3dEf7hIj9kLm2nOp4qRs6tUv8wXy0z1234");
    for (rationale, key) in [(secret.as_str(), "secret-rationale"), ("", secret.as_str())] {
        let error = reports::dispose(
            &db.pool,
            catalog,
            &actor("human"),
            &id,
            Disposition::Open,
            rationale,
            key,
            t0(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, OkfError::SecretDetected { .. }));
        assert!(!error.to_string().contains(&secret));
    }
    for key in ["", "space key", "\n", &"x".repeat(129)] {
        assert!(reports::dispose(
            &db.pool,
            catalog,
            &actor("human"),
            &id,
            Disposition::Open,
            "",
            key,
            t0()
        )
        .await
        .is_err());
    }
    for id in ["", " ", "missing"] {
        assert!(reports::get_finding(&db.pool, catalog, id).await.is_err());
        assert!(reports::dispose(
            &db.pool,
            catalog,
            &actor("human"),
            id,
            Disposition::Open,
            "",
            "invalid",
            t0()
        )
        .await
        .is_err());
    }
    assert!(reports::get_finding(&db.pool, catalog, &id)
        .await
        .unwrap()
        .dispositions
        .is_empty());
    let requests: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM maintenance_requests WHERE catalog_id=$1 AND operation='dispose'",
    )
    .bind(catalog)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(requests, 0);
    assert_eq!(canonical(&db.pool, catalog).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_report_outdated_tracks_all_evidence() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (lease, id) = fixture(&db.pool, catalog, "first").await;
    let original = runs::get_run(&db.pool, catalog, &lease.run_id)
        .await
        .unwrap();
    assert!(
        !reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    assert_eq!(original.work[0].evidence[0].pin, lease.pin);
    assert_eq!(original.work[0].finding_ids, vec![id.clone()]);
    assert_eq!(original.run.completed_count, 1);
    assert_eq!(
        original.work[0].assessment,
        Some(Assessment::SupportedCurrent)
    );
    revision(&db.pool, catalog, "b.md", 2, t0(), "Body new").await;
    assert!(
        reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    // Restore only the neighbor's current pointer to isolate the assessment/primary pin.
    sqlx::query("UPDATE documents SET revision=1 WHERE catalog_id=$1 AND path='b.md'")
        .bind(catalog)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    sqlx::query("UPDATE documents SET deleted_at=$2 WHERE catalog_id=$1 AND path='a.md'")
        .bind(catalog)
        .bind(t0())
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    sqlx::query("DELETE FROM documents WHERE catalog_id=$1 AND path='a.md'")
        .bind(catalog)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    revision(&db.pool, catalog, "a.md", 2, t0(), "Body").await;
    assert!(
        reports::get_finding(&db.pool, catalog, &id)
            .await
            .unwrap()
            .outdated
    );
    assert!(
        reports::list_findings(&db.pool, catalog, None, 20)
            .await
            .unwrap()
            .items[0]
            .outdated
    );
    assert_eq!(
        runs::get_run(&db.pool, catalog, &lease.run_id)
            .await
            .unwrap(),
        original
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_report_pages_are_catalog_scoped() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (first, _) = fixture(&db.pool, catalog, "first").await;
    let (second, _) = fixture(&db.pool, catalog, "second").await;
    let foreign = crate::store::catalogs::create(&db.pool, "foreign", "", t0())
        .await
        .unwrap()
        .id;
    revision(&db.pool, foreign, "a.md", 1, t0(), "Body").await;
    revision(&db.pool, foreign, "b.md", 1, t0(), "Body").await;
    let (foreign_run, foreign_finding) = fixture(&db.pool, foreign, "first").await;
    let all = runs::list_runs(&db.pool, catalog, None, 20).await.unwrap();
    assert_eq!(all.items.len(), 2);
    assert!(all.items[0].id < all.items[1].id); // Equal timestamps: ID is the tie breaker.
    assert_eq!(all.next_after, None);
    let page = runs::list_runs(&db.pool, catalog, None, 1).await.unwrap();
    assert_eq!(page.items, all.items[..1]);
    let last = runs::list_runs(&db.pool, catalog, page.next_after.as_deref(), 1)
        .await
        .unwrap();
    assert_eq!(last.items, all.items[1..]);
    assert_eq!(last.next_after, None);
    let findings = reports::list_findings(&db.pool, catalog, None, 200)
        .await
        .unwrap();
    assert_eq!(findings.items.len(), 2);
    assert!(findings.items[0].id < findings.items[1].id);
    let page = reports::list_findings(&db.pool, catalog, None, 1)
        .await
        .unwrap();
    assert_eq!(page.items, findings.items[..1]);
    let last = reports::list_findings(&db.pool, catalog, page.next_after.as_deref(), 1)
        .await
        .unwrap();
    assert_eq!(last.items, findings.items[1..]);
    assert_eq!(last.next_after, None);
    assert!(
        reports::list_findings(&db.pool, catalog, Some(&findings.items[1].id), 1)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    for cursor in [&foreign_run.run_id, "missing", ""] {
        assert!(matches!(
            runs::list_runs(&db.pool, catalog, Some(cursor), 20).await,
            Err(OkfError::NotFound { .. })
        ));
    }
    for cursor in [&foreign_finding, "missing", ""] {
        assert!(matches!(
            reports::list_findings(&db.pool, catalog, Some(cursor), 20).await,
            Err(OkfError::NotFound { .. })
        ));
    }
    for limit in [0, 201, u32::MAX] {
        assert!(matches!(
            runs::list_runs(&db.pool, catalog, None, limit).await,
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            reports::list_findings(&db.pool, catalog, None, limit).await,
            Err(OkfError::Refused { .. })
        ));
    }
    assert!(matches!(
        runs::get_run(&db.pool, catalog, &foreign_run.run_id).await,
        Err(OkfError::NotFound { .. })
    ));
    assert!(matches!(
        reports::get_finding(&db.pool, catalog, &foreign_finding).await,
        Err(OkfError::NotFound { .. })
    ));
    for id in ["", " ", "missing"] {
        assert!(runs::get_run(&db.pool, catalog, id).await.is_err());
    }
    assert!(runs::list_runs(&db.pool, foreign + 1, None, 20)
        .await
        .unwrap()
        .items
        .is_empty());
    // Unequal timestamps must outrank ID ordering, including cursor continuation.
    sqlx::query("UPDATE maintenance_runs SET created_at=$3 WHERE catalog_id=$1 AND id=$2")
        .bind(catalog)
        .bind(&all.items[1].id)
        .bind(t0() - Duration::seconds(1))
        .execute(&db.pool)
        .await
        .unwrap();
    let by_time = runs::list_runs(&db.pool, catalog, None, 1).await.unwrap();
    assert_eq!(by_time.items[0].id, all.items[1].id);
    assert_eq!(
        runs::list_runs(&db.pool, catalog, by_time.next_after.as_deref(), 200)
            .await
            .unwrap()
            .items[0]
            .id,
        all.items[0].id
    );
    sqlx::query("UPDATE maintenance_findings SET created_at=$3 WHERE catalog_id=$1 AND id=$2")
        .bind(catalog)
        .bind(&findings.items[1].id)
        .bind(t0() - Duration::seconds(1))
        .execute(&db.pool)
        .await
        .unwrap();
    let by_time = reports::list_findings(&db.pool, catalog, None, 1)
        .await
        .unwrap();
    assert_eq!(by_time.items[0].id, findings.items[1].id);
    assert_eq!(
        reports::list_findings(&db.pool, catalog, by_time.next_after.as_deref(), 200)
            .await
            .unwrap()
            .items[0]
            .id,
        findings.items[0].id
    );
    assert!(matches!(
        reports::dispose(
            &db.pool,
            catalog,
            &actor("human"),
            &foreign_finding,
            Disposition::Open,
            "",
            "foreign",
            t0()
        )
        .await,
        Err(OkfError::NotFound { .. })
    ));
    let decision = reports::dispose(
        &db.pool,
        catalog,
        &actor("human"),
        &findings.items[0].id,
        Disposition::Dismissed,
        "Catalog cascade fixture",
        "delete-fixture",
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(decision.dispositions.len(), 1);
    sqlx::query("DELETE FROM catalogs WHERE id=$1")
        .bind(catalog)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(runs::get_run(&db.pool, catalog, &first.run_id)
        .await
        .is_err());
    assert!(runs::get_run(&db.pool, catalog, &second.run_id)
        .await
        .is_err());
    assert!(reports::list_findings(&db.pool, catalog, None, 20)
        .await
        .unwrap()
        .items
        .is_empty());
    let count: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM maintenance_runs WHERE catalog_id=$1) + (SELECT count(*) FROM maintenance_work WHERE catalog_id=$1) + (SELECT count(*) FROM maintenance_findings WHERE catalog_id=$1) + (SELECT count(*) FROM maintenance_evidence WHERE catalog_id=$1) + (SELECT count(*) FROM maintenance_dispositions WHERE catalog_id=$1) + (SELECT count(*) FROM maintenance_requests WHERE catalog_id=$1)").bind(catalog).fetch_one(&db.pool).await.unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        reports::get_finding(&db.pool, foreign, &foreign_finding)
            .await
            .unwrap()
            .id,
        foreign_finding
    );
    assert_eq!(
        runs::get_run(&db.pool, foreign, &foreign_run.run_id)
            .await
            .unwrap()
            .run
            .id,
        foreign_run.run_id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_disposition_concurrent_retry_is_one_append() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (_, id) = fixture(&db.pool, catalog, "first").await;
    let before = canonical(&db.pool, catalog).await;
    let human = actor("human");
    let rationale = "é".repeat(2048); // Exactly 4096 UTF-8 bytes is allowed.
    let (a, b) = tokio::join!(
        reports::dispose(
            &db.pool,
            catalog,
            &human,
            &id,
            Disposition::AcceptedForFollowUp,
            &rationale,
            "race",
            t0()
        ),
        reports::dispose(
            &db.pool,
            catalog,
            &human,
            &id,
            Disposition::AcceptedForFollowUp,
            &rationale,
            "race",
            t0() + Duration::seconds(1)
        ),
    );
    let result = a.unwrap();
    assert_eq!(result, b.unwrap());
    assert_eq!(result.dispositions.len(), 1);
    assert_eq!(result.dispositions[0].rationale, rationale);
    assert_eq!(
        reports::get_finding(&db.pool, catalog, &id).await.unwrap(),
        result
    );
    let credentials: Vec<String> = sqlx::query_scalar(
        "SELECT credential_id FROM maintenance_dispositions WHERE catalog_id=$1 AND finding_id=$2",
    )
    .bind(catalog)
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(credentials, vec![human.key_id]);
    assert_eq!(canonical(&db.pool, catalog).await, before);
}

fn assert_dto<T>(value: &T, fields: &[&str])
where
    T: std::fmt::Debug
        + Clone
        + PartialEq
        + serde::Serialize
        + serde::de::DeserializeOwned
        + schemars::JsonSchema,
{
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(
        json.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        fields.iter().copied().collect()
    );
    assert_eq!(&serde_json::from_value::<T>(json).unwrap(), value);
    assert_eq!(&value.clone(), value);
    assert!(schemars::schema_for!(T)
        .as_object()
        .unwrap()
        .contains_key("properties"));
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_report_assessment_and_sibling_pins_and_dto_contracts() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md", "c.md", "d.md"]).await;
    let lease = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "batch",
        1,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap()
    .leases
    .remove(0);
    let evidence = |path: &str| Evidence {
        pin: RevisionPin {
            path: path.into(),
            revision: 1,
        },
        excerpt: "Body".into(),
    };
    let finding = |path: &str| FindingInput {
        category: Category::UnverifiableClaim,
        assertion: "Assertion".into(),
        follow_up: "Follow up".into(),
        uncertainty: "Uncertain".into(),
        relationship: Relationship::DirectStatement,
        evidence: vec![evidence(path)],
        external: vec![],
    };
    let input = Submission {
        outcome: WorkOutcome::Completed,
        assessment: Assessment::SupportedCurrent,
        summary: "Summary".into(),
        evidence: vec![evidence("a.md"), evidence("c.md")],
        findings: vec![finding("b.md"), finding("d.md")],
        reported_tokens: None,
    };
    let receipt = reports::submit(
        &db.pool,
        catalog,
        &actor("one"),
        &lease.work_id,
        &lease.token,
        "submit",
        &input,
        t0(),
    )
    .await
    .unwrap();
    let id = &receipt.finding_ids[0];
    let original = reports::get_finding(&db.pool, catalog, id).await.unwrap();
    assert_eq!(original.input, input.findings[0]);
    assert!(!original.outdated);
    // A pin cited only in assessment evidence still invalidates the finding.
    revision(&db.pool, catalog, "c.md", 2, t0(), "Body new").await;
    assert!(
        reports::get_finding(&db.pool, catalog, id)
            .await
            .unwrap()
            .outdated
    );
    sqlx::query("UPDATE documents SET revision=1 WHERE catalog_id=$1 AND path='c.md'")
        .bind(catalog)
        .execute(&db.pool)
        .await
        .unwrap();
    // Likewise for evidence cited only by a sibling finding in the same work.
    revision(&db.pool, catalog, "d.md", 2, t0(), "Body new").await;
    assert!(
        reports::get_finding(&db.pool, catalog, id)
            .await
            .unwrap()
            .outdated
    );
    let disposed = reports::dispose(
        &db.pool,
        catalog,
        &actor("human"),
        id,
        Disposition::NeedsInput,
        "Please check",
        "human",
        t0(),
    )
    .await
    .unwrap();
    assert_dto(
        &disposed,
        &[
            "id",
            "run_id",
            "work_id",
            "input",
            "actor",
            "created_at",
            "disposition",
            "dispositions",
            "outdated",
        ],
    );
    assert_dto(
        &disposed.dispositions[0],
        &["id", "disposition", "rationale", "actor", "at"],
    );
    let run = runs::get_run(&db.pool, catalog, &lease.run_id)
        .await
        .unwrap();
    assert_dto(&run, &["run", "work"]);
    assert_dto(
        &run.work[0],
        &[
            "id",
            "pin",
            "reasons",
            "outcome",
            "assessment",
            "summary",
            "evidence",
            "finding_ids",
        ],
    );
    assert_eq!(run.work[0].evidence, input.evidence);
    let page = reports::list_findings(&db.pool, catalog, None, 20)
        .await
        .unwrap();
    assert_dto(&page, &["items", "next_after"]);
    assert_dto(
        &runs::list_runs(&db.pool, catalog, None, 20).await.unwrap(),
        &["items", "next_after"],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_report_terminal_and_running_work_without_findings() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let batch = runs::claim(
        &db.pool,
        catalog,
        &actor("one"),
        "batch",
        2,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    let running = runs::get_run(&db.pool, catalog, &batch.run_id)
        .await
        .unwrap();
    assert_eq!(running.work.len(), 2);
    for work in running.work {
        assert_eq!(work.outcome, None);
        assert_eq!(work.assessment, None);
        assert_eq!(work.summary, None);
        assert!(work.evidence.is_empty());
        assert!(work.finding_ids.is_empty());
    }
    for (lease, outcome, assessment) in [
        (
            &batch.leases[0],
            WorkOutcome::Failed,
            Assessment::NotAssessed,
        ),
        (
            &batch.leases[1],
            WorkOutcome::Inconclusive,
            Assessment::Unverifiable,
        ),
    ] {
        reports::submit(
            &db.pool,
            catalog,
            &actor("one"),
            &lease.work_id,
            &lease.token,
            &lease.work_id,
            &Submission {
                outcome,
                assessment,
                summary: "No evidence".into(),
                evidence: vec![],
                findings: vec![],
                reported_tokens: None,
            },
            t0(),
        )
        .await
        .unwrap();
    }
    let report = runs::get_run(&db.pool, catalog, &batch.run_id)
        .await
        .unwrap();
    assert_eq!(report.run.failed_count, 1);
    assert_eq!(report.run.inconclusive_count, 1);
    assert_eq!(report.run.state, RunState::Completed);
    for work in report.work {
        assert!(work.evidence.is_empty());
        assert!(work.finding_ids.is_empty());
    }
    assert!(reports::list_findings(&db.pool, catalog, None, 20)
        .await
        .unwrap()
        .items
        .is_empty());
}
